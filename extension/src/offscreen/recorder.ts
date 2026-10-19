// Capture logic for the offscreen document: one recording at a time, held in module state.

import type { OffscreenCommand, OffscreenEvent } from '../shared/messages';
import { appendChunk } from '../shared/recordingFile';
import {
  initialRecordingState,
  type RecordingState,
  remainingMs,
  type StopReason,
  transition,
} from '../shared/recordingState';

const CHUNK_MS = 5000;

interface Session {
  startedAt: number;
  state: RecordingState;
  recorder: MediaRecorder;
  ctx: AudioContext;
  streams: MediaStream[];
  capTimer: ReturnType<typeof setTimeout> | undefined;
  appends: Promise<void>;
  appendFailed: boolean;
}

let busy = false;
let session: Session | null = null;

function emit(event: OffscreenEvent): void {
  void chrome.runtime.sendMessage(event);
}

function messageOf(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

function stopTracks(streams: MediaStream[]): void {
  for (const stream of streams) {
    for (const track of stream.getTracks()) track.stop();
  }
}

function tabConstraints(streamId: string): MediaStreamConstraints {
  const tab = { mandatory: { chromeMediaSource: 'tab', chromeMediaSourceId: streamId } };
  // Chrome's legacy tab-capture constraints, not in lib.dom's MediaTrackConstraints.
  return {
    audio: tab,
    video: { mandatory: { ...tab.mandatory, maxWidth: 1280, maxHeight: 720 } },
  } as MediaStreamConstraints;
}

async function micStream(): Promise<MediaStream | null> {
  try {
    return await navigator.mediaDevices.getUserMedia({ audio: true });
  } catch {
    // The mic page (#18) grants the permission ahead of time; without it the recording is tab audio only.
    return null;
  }
}

function armCap(s: Session): void {
  clearTimeout(s.capTimer);
  s.capTimer = setTimeout(() => void stopRecording('cap'), remainingMs(s.state, Date.now()));
}

function queueAppend(s: Session, chunk: Blob): void {
  s.appends = s.appends.then(async () => {
    if (s.appendFailed) return;
    try {
      await appendChunk(s.startedAt, chunk);
    } catch (error) {
      s.appendFailed = true;
      emit({ type: 'offscreen_error', message: `Could not write the recording: ${messageOf(error)}` });
      void stopRecording('capture_ended');
    }
  });
}

export async function startRecording(cmd: Extract<OffscreenCommand, { type: 'offscreen_start' }>): Promise<void> {
  if (busy) return;
  busy = true;

  let tabStream: MediaStream;
  try {
    tabStream = await navigator.mediaDevices.getUserMedia(tabConstraints(cmd.streamId));
  } catch (error) {
    busy = false;
    emit({ type: 'offscreen_error', message: `Could not capture the tab: ${messageOf(error)}` });
    return;
  }
  const mic = await micStream();
  const streams = mic ? [tabStream, mic] : [tabStream];

  let ctx: AudioContext | undefined;
  try {
    ctx = new AudioContext();
    const dest = ctx.createMediaStreamDestination();
    const tabSource = ctx.createMediaStreamSource(tabStream);
    // Tab capture mutes the tab, so its audio also goes back to the speakers. The mic never does.
    tabSource.connect(dest);
    tabSource.connect(ctx.destination);
    if (mic) ctx.createMediaStreamSource(mic).connect(dest);

    const recorder = new MediaRecorder(
      new MediaStream([...tabStream.getVideoTracks(), ...dest.stream.getAudioTracks()]),
      { mimeType: 'video/webm;codecs=vp8,opus', videoBitsPerSecond: 1_000_000 },
    );
    const s: Session = {
      startedAt: cmd.startedAt,
      state: transition(initialRecordingState, { type: 'start', at: Date.now() }),
      recorder,
      ctx,
      streams,
      capTimer: undefined,
      appends: Promise.resolve(),
      appendFailed: false,
    };
    recorder.addEventListener('dataavailable', (event) => queueAppend(s, (event as BlobEvent).data));
    tabStream.getVideoTracks()[0]?.addEventListener('ended', () => void stopRecording('capture_ended'));

    session = s;
    recorder.start(CHUNK_MS);
    armCap(s);
    emit({ type: 'offscreen_started', micIncluded: mic !== null });
  } catch (error) {
    stopTracks(streams);
    await ctx?.close();
    session = null;
    busy = false;
    emit({ type: 'offscreen_error', message: `Could not start recording: ${messageOf(error)}` });
  }
}

export function pauseRecording(): void {
  if (!session) return;
  const next = transition(session.state, { type: 'pause', at: Date.now() });
  if (next === session.state) return;
  session.state = next;
  clearTimeout(session.capTimer);
  session.recorder.pause();
}

export function resumeRecording(): void {
  if (!session) return;
  const next = transition(session.state, { type: 'resume', at: Date.now() });
  if (next === session.state) return;
  session.state = next;
  session.recorder.resume();
  armCap(session);
}

export async function stopRecording(reason: StopReason): Promise<void> {
  const s = session;
  if (!s) return;
  const next = transition(s.state, { type: 'stop', at: Date.now(), reason });
  if (next === s.state) return;
  s.state = next;
  clearTimeout(s.capTimer);

  const stopped = new Promise<void>((resolve) => s.recorder.addEventListener('stop', () => resolve(), { once: true }));
  s.recorder.stop();
  await stopped;
  await s.appends;
  stopTracks(s.streams);
  await s.ctx.close();

  session = null;
  busy = false;
  emit({ type: 'offscreen_stopped', startedAt: s.startedAt, reason });
}
