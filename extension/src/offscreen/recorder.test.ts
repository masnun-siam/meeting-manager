import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { appendChunk } from '../shared/recordingFile';
import { RECORDING_CAP_MS } from '../shared/recordingState';
import { pauseRecording, resumeRecording, startRecording, stopRecording } from './recorder';

vi.mock('../shared/recordingFile');

const HOUR = 60 * 60 * 1000;
const START = { type: 'offscreen_start', streamId: 'sid', language: 'en', startedAt: 1760866200000 } as const;

class FakeTrack extends EventTarget {
  stop = vi.fn();
}

class FakeStream {
  tracks: FakeTrack[];
  constructor(tracks: FakeTrack[] = []) {
    this.tracks = tracks;
  }
  getTracks = () => this.tracks;
  getVideoTracks = () => this.tracks;
  getAudioTracks = () => this.tracks;
}

class FakeRecorder extends EventTarget {
  static last: FakeRecorder;
  start = vi.fn();
  pause = vi.fn();
  resume = vi.fn();
  constructor(
    public stream: FakeStream,
    public options: MediaRecorderOptions,
  ) {
    super();
    FakeRecorder.last = this;
  }
  emit(data: string) {
    this.dispatchEvent(Object.assign(new Event('dataavailable'), { data: new Blob([data]) }));
  }
  stop = vi.fn(() => {
    this.emit('final');
    this.dispatchEvent(new Event('stop'));
  });
}

const ctxClose = vi.fn(async () => {});
const sources: { stream: FakeStream; connect: ReturnType<typeof vi.fn> }[] = [];
class FakeAudioContext {
  destination = {};
  close = ctxClose;
  createMediaStreamDestination = () => ({ stream: new FakeStream([new FakeTrack()]) });
  createMediaStreamSource = (stream: FakeStream) => {
    const source = { stream, connect: vi.fn() };
    sources.push(source);
    return source;
  };
}

let tabVideo: FakeTrack;
let getUserMedia: ReturnType<typeof vi.fn>;
let sendMessage: ReturnType<typeof vi.fn>;

function sent() {
  return sendMessage.mock.calls.map(([event]) => event);
}

beforeEach(() => {
  vi.useFakeTimers();
  vi.setSystemTime(0);
  sources.length = 0;
  tabVideo = new FakeTrack();
  getUserMedia = vi.fn(async (c: { audio: unknown }) =>
    c.audio === true ? new FakeStream([new FakeTrack()]) : new FakeStream([tabVideo]),
  );
  sendMessage = vi.fn();
  vi.mocked(appendChunk).mockReset().mockResolvedValue();
  ctxClose.mockClear();
  vi.stubGlobal('navigator', { mediaDevices: { getUserMedia } });
  vi.stubGlobal('chrome', { runtime: { sendMessage } });
  vi.stubGlobal('AudioContext', FakeAudioContext);
  vi.stubGlobal('MediaStream', FakeStream);
  vi.stubGlobal('MediaRecorder', FakeRecorder);
});

afterEach(async () => {
  await stopRecording('user');
  vi.useRealTimers();
  vi.unstubAllGlobals();
});

describe('startRecording', () => {
  it('captures the tab at 720p, records vp8/opus at 1 Mbps and reports the mic', async () => {
    await startRecording(START);
    expect(getUserMedia).toHaveBeenCalledWith({
      audio: { mandatory: { chromeMediaSource: 'tab', chromeMediaSourceId: 'sid' } },
      video: {
        mandatory: { chromeMediaSource: 'tab', chromeMediaSourceId: 'sid', maxWidth: 1280, maxHeight: 720 },
      },
    });
    expect(FakeRecorder.last.options).toEqual({
      mimeType: 'video/webm;codecs=vp8,opus',
      videoBitsPerSecond: 1_000_000,
    });
    expect(FakeRecorder.last.start).toHaveBeenCalledWith(5000);
    expect(sent()).toEqual([{ type: 'offscreen_started', micIncluded: true }]);
  });

  it('plays the tab audio back but keeps the mic out of the speakers', async () => {
    await startRecording(START);
    const [tab, mic] = sources;
    expect(tab?.connect).toHaveBeenCalledTimes(2);
    expect(mic?.connect).toHaveBeenCalledTimes(1);
  });

  it('goes on with tab audio only when the mic is refused', async () => {
    getUserMedia.mockImplementation(async (c: { audio: unknown }) => {
      if (c.audio === true) throw new Error('denied');
      return new FakeStream([tabVideo]);
    });
    await startRecording(START);
    expect(sent()).toEqual([{ type: 'offscreen_started', micIncluded: false }]);
    expect(sources).toHaveLength(1);
  });

  it('reports a tab capture failure and starts no recorder', async () => {
    getUserMedia.mockRejectedValue(new Error('no stream'));
    FakeRecorder.last = undefined as never;
    await startRecording(START);
    expect(sent()).toEqual([{ type: 'offscreen_error', message: 'Could not capture the tab: no stream' }]);
    expect(FakeRecorder.last).toBeUndefined();
  });

  it('releases the tab and goes idle when setup fails after the stream is acquired', async () => {
    vi.stubGlobal('MediaRecorder', undefined);
    await startRecording(START);
    expect(sent()).toEqual([
      expect.objectContaining({
        type: 'offscreen_error',
        message: expect.stringContaining('Could not start recording'),
      }),
    ]);
    expect(tabVideo.stop).toHaveBeenCalled();
    vi.stubGlobal('MediaRecorder', FakeRecorder);
    await startRecording(START);
    expect(sent().at(-1)).toEqual({ type: 'offscreen_started', micIncluded: true });
  });

  it('ignores a second start while recording', async () => {
    await startRecording(START);
    await startRecording(START);
    expect(sent()).toHaveLength(1);
  });
});

describe('chunks', () => {
  it('appends every chunk in emit order under startedAt', async () => {
    await startRecording(START);
    for (const d of ['a', 'b', 'c']) FakeRecorder.last.emit(d);
    await vi.advanceTimersByTimeAsync(0);
    const texts = await Promise.all(vi.mocked(appendChunk).mock.calls.map(([, blob]) => blob.text()));
    expect(texts).toEqual(['a', 'b', 'c']);
    expect(vi.mocked(appendChunk).mock.calls.every(([at]) => at === START.startedAt)).toBe(true);
  });

  it('reports a failed append, then stops with capture_ended', async () => {
    vi.mocked(appendChunk).mockRejectedValue(new Error('disk full'));
    await startRecording(START);
    FakeRecorder.last.emit('a');
    await vi.advanceTimersByTimeAsync(0);
    expect(sent().slice(1)).toEqual([
      { type: 'offscreen_error', message: 'Could not write the recording: disk full' },
      { type: 'offscreen_stopped', startedAt: START.startedAt, reason: 'capture_ended' },
    ]);
  });
});

describe('pause and resume', () => {
  it('calls through on the same recorder', async () => {
    await startRecording(START);
    const recorder = FakeRecorder.last;
    pauseRecording();
    resumeRecording();
    expect(recorder.pause).toHaveBeenCalledTimes(1);
    expect(recorder.resume).toHaveBeenCalledTimes(1);
    expect(FakeRecorder.last).toBe(recorder);
  });
});

describe('stopping', () => {
  it('stops with the cap at 3 h of recorded time, not wall time', async () => {
    await startRecording(START);
    await vi.advanceTimersByTimeAsync(HOUR);
    pauseRecording();
    await vi.advanceTimersByTimeAsync(HOUR);
    resumeRecording();
    await vi.advanceTimersByTimeAsync(RECORDING_CAP_MS - HOUR - 1);
    expect(sent().some((e) => e.type === 'offscreen_stopped')).toBe(false);
    await vi.advanceTimersByTimeAsync(1);
    expect(sent().at(-1)).toEqual({ type: 'offscreen_stopped', startedAt: START.startedAt, reason: 'cap' });
  });

  it('stops with capture_ended when the video track ends', async () => {
    await startRecording(START);
    tabVideo.dispatchEvent(new Event('ended'));
    await vi.advanceTimersByTimeAsync(0);
    expect(sent().at(-1)).toMatchObject({ type: 'offscreen_stopped', reason: 'capture_ended' });
  });

  it('appends the final chunk before offscreen_stopped, then releases everything', async () => {
    await startRecording(START);
    await stopRecording('user');
    expect(appendChunk).toHaveBeenCalledTimes(1);
    expect(sent().at(-1)).toEqual({ type: 'offscreen_stopped', startedAt: START.startedAt, reason: 'user' });
    expect(tabVideo.stop).toHaveBeenCalled();
    expect(ctxClose).toHaveBeenCalled();
  });

  it('stops from paused with the user reason', async () => {
    await startRecording(START);
    pauseRecording();
    await stopRecording('user');
    expect(sent().at(-1)).toMatchObject({ type: 'offscreen_stopped', reason: 'user' });
  });

  it('sends one stopped event for duplicate stops', async () => {
    await startRecording(START);
    await Promise.all([stopRecording('user'), stopRecording('capture_ended')]);
    expect(sent().filter((e) => e.type === 'offscreen_stopped')).toHaveLength(1);
  });
});
