import { beforeEach, describe, expect, it, vi } from 'vitest';
import { LAST_RECORDING_KEY } from '../shared/lastRecording';
import { sendNative } from '../shared/nativeClient';
import { nextFreeName, recordingName, saveRecording, takenNames } from './save';

vi.mock('../shared/nativeClient', () => ({ sendNative: vi.fn() }));
const flag = vi.hoisted(() => ({ continueOn: false }));
vi.mock('../shared/flags', () => ({ continueRecordingEnabled: () => flag.continueOn }));
const removeEntry = vi.fn();
vi.mock('../shared/recordingFile', () => ({ deleteRecordingFile: (...a: unknown[]) => removeEntry(...a) }));

const STARTED = new Date(2026, 9, 19, 14, 30, 59).getTime();
const opts = { startedAt: STARTED, name: 'meeting-2026-10-19_1430', language: 'en' as const, process: true };

let listener: (delta: unknown) => void;
const download = vi.fn();
const search = vi.fn();
const sendMessage = vi.fn();
const localSet = vi.fn();

/** `download` resolves with id 5, then Chrome reports `delta` for it. */
function finishWith(delta: Record<string, unknown>) {
  download.mockImplementation(async () => {
    setTimeout(() => listener({ id: 5, ...delta }), 0);
    return 5;
  });
}

beforeEach(() => {
  for (const m of [download, search, sendMessage, removeEntry, localSet, vi.mocked(sendNative)]) m.mockReset();
  flag.continueOn = false;
  sendMessage.mockResolvedValue({ url: 'blob:x' });
  removeEntry.mockResolvedValue(undefined);
  search.mockResolvedValue([{ filename: '/Users/me/Downloads/Meetings/meeting-2026-10-19_1430/recording.webm' }]);
  vi.stubGlobal('chrome', {
    runtime: { sendMessage },
    storage: { local: { set: localSet } },
    downloads: {
      download,
      search,
      onChanged: {
        addListener: (fn: typeof listener) => {
          listener = fn;
        },
        removeListener: vi.fn(),
      },
    },
  });
});

describe('naming', () => {
  it('formats the local start time with zero padding', () => {
    expect(recordingName(STARTED)).toBe('meeting-2026-10-19_1430');
    expect(recordingName(new Date(2026, 0, 2, 3, 4).getTime())).toBe('meeting-2026-01-02_0304');
  });

  it('skips taken names', () => {
    expect(nextFreeName('a', new Set())).toBe('a');
    expect(nextFreeName('a', new Set(['a']))).toBe('a-2');
    expect(nextFreeName('a', new Set(['a', 'a-2']))).toBe('a-3');
  });

  it('reads taken names from the download history', async () => {
    search.mockResolvedValue([
      { filename: '/d/Meetings/a/recording.webm' },
      { filename: 'C:\\d\\Meetings\\a-2\\recording.webm' },
    ]);
    expect(await takenNames('a')).toEqual(new Set(['a', 'a-2']));
    expect(search).toHaveBeenCalledWith({ filenameRegex: 'Meetings/a(-\\d+)?/recording\\.webm$', exists: true });
  });
});

describe('saveRecording', () => {
  it('downloads, deletes the origin-private file and sends process', async () => {
    finishWith({ state: { current: 'complete' } });
    vi.mocked(sendNative).mockResolvedValue({ ok: true, job_id: 'j1' });
    expect(await saveRecording(opts)).toEqual({ kind: 'saved' });
    expect(download).toHaveBeenCalledWith({
      url: 'blob:x',
      filename: 'Meetings/meeting-2026-10-19_1430/recording.webm',
      saveAs: false,
      conflictAction: 'uniquify',
    });
    expect(removeEntry).toHaveBeenCalledWith(STARTED);
    expect(sendNative).toHaveBeenCalledWith({
      type: 'process',
      path: '/Users/me/Downloads/Meetings/meeting-2026-10-19_1430/recording.webm',
      language: 'en',
    });
  });

  it('sends the context with process when given', async () => {
    finishWith({ state: { current: 'complete' } });
    vi.mocked(sendNative).mockResolvedValue({ ok: true, job_id: 'j1' });
    const context = {
      version: 1 as const,
      platform: 'other' as const,
      url: null,
      meeting_id: null,
      tab_title: null,
      meeting_title: null,
      started_at_ms: 1,
      utc_offset_min: 0,
      time_zone: null,
      participants: [],
      speaking: [],
      truncated: [],
    };
    await saveRecording({ ...opts, context });
    expect(sendNative).toHaveBeenCalledWith(expect.objectContaining({ type: 'process', context }));
  });

  it('saves but does not process a short recording', async () => {
    finishWith({ state: { current: 'complete' } });
    expect(await saveRecording({ ...opts, process: false })).toEqual({ kind: 'too_short' });
    expect(sendNative).not.toHaveBeenCalled();
    expect(removeEntry).toHaveBeenCalled();
  });

  it.each([
    ['host_missing', 'setup'],
    ['not_configured', 'setup'],
    ['invalid_path', 'error'],
  ] as const)('maps %s to a %s notice', async (code, kind) => {
    finishWith({ state: { current: 'complete' } });
    vi.mocked(sendNative).mockResolvedValue({ ok: false, code, message: 'msg' });
    expect(await saveRecording(opts)).toEqual({ kind, message: 'msg' });
  });

  it('reports a thrown native error', async () => {
    finishWith({ state: { current: 'complete' } });
    vi.mocked(sendNative).mockRejectedValue(new Error('boom'));
    expect(await saveRecording(opts)).toEqual({
      kind: 'error',
      message: 'Could not reach the meeting tool: boom',
    });
  });

  it('keeps the file and says the disk is full when the download is interrupted', async () => {
    finishWith({ state: { current: 'interrupted' }, error: { current: 'FILE_NO_SPACE' } });
    const notice = await saveRecording(opts);
    expect(notice).toMatchObject({ kind: 'error', message: expect.stringContaining('Disk full') });
    expect(removeEntry).not.toHaveBeenCalled();
    expect(sendNative).not.toHaveBeenCalled();
  });

  it('still processes when deleting the origin-private file fails', async () => {
    finishWith({ state: { current: 'complete' } });
    removeEntry.mockRejectedValue(new Error('locked'));
    vi.mocked(sendNative).mockResolvedValue({ ok: true, job_id: 'j1' });
    expect(await saveRecording(opts)).toEqual({ kind: 'saved' });
  });

  it('names other interruptions', async () => {
    finishWith({ state: { current: 'interrupted' }, error: { current: 'NETWORK_FAILED' } });
    expect(await saveRecording(opts)).toMatchObject({ message: expect.stringContaining('(NETWORK_FAILED)') });
  });

  it('keeps the file when the offscreen document has none', async () => {
    sendMessage.mockResolvedValue({ error: 'gone' });
    expect(await saveRecording(opts)).toMatchObject({ kind: 'error' });
    expect(download).not.toHaveBeenCalled();
  });
});

describe('saveRecording remembers the last recording', () => {
  const PATH = '/Users/me/Downloads/Meetings/meeting-2026-10-19_1430/recording.webm';
  const remember = { recordedMs: 5_000, stoppedAt: STARTED + 5_000, stopReason: 'user' as const };

  beforeEach(() => {
    flag.continueOn = true;
  });

  it('stores it after a complete download, even when too short to process', async () => {
    finishWith({ state: { current: 'complete' } });
    expect(await saveRecording({ ...opts, process: false, remember })).toEqual({ kind: 'too_short' });
    expect(localSet).toHaveBeenCalledWith({
      [LAST_RECORDING_KEY]: {
        download_id: 5,
        path: PATH,
        folder: 'meeting-2026-10-19_1430',
        ext: 'webm',
        recorded_ms: 5_000,
        language: 'en',
        stopped_at: STARTED + 5_000,
        stop_reason: 'user',
        parts: 1,
      },
    });
    expect(localSet.mock.invocationCallOrder[0]).toBeLessThan(removeEntry.mock.invocationCallOrder[0]);
  });

  it('stores nothing when the download is interrupted', async () => {
    finishWith({ state: { current: 'interrupted' }, error: { current: 'FILE_NO_SPACE' } });
    await saveRecording({ ...opts, remember });
    expect(localSet).not.toHaveBeenCalled();
  });

  it('stores nothing with the flag off', async () => {
    flag.continueOn = false;
    finishWith({ state: { current: 'complete' } });
    await saveRecording({ ...opts, process: false, remember });
    expect(localSet).not.toHaveBeenCalled();
  });

  it('stores nothing without remember', async () => {
    finishWith({ state: { current: 'complete' } });
    await saveRecording({ ...opts, process: false });
    expect(localSet).not.toHaveBeenCalled();
  });

  it('still saves and processes when the write fails, logging a fixed text', async () => {
    const error = vi.spyOn(console, 'error').mockImplementation(() => undefined);
    finishWith({ state: { current: 'complete' } });
    localSet.mockRejectedValue(new Error(`quota ${PATH}`));
    vi.mocked(sendNative).mockResolvedValue({ ok: true, job_id: 'j1' });
    expect(await saveRecording({ ...opts, remember })).toEqual({ kind: 'saved' });
    expect(error).toHaveBeenCalledWith('Could not store the last recording');
    expect(sendNative).toHaveBeenCalled();
    error.mockRestore();
  });
});
