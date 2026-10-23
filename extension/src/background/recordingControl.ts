// Recording control: the service worker's state owner. Every handler reads the status from storage first,
// so a service worker that slept mid-recording carries on.

import { meetingContextEnabled } from '../shared/flags';
import { contextForHandoff, identityFromTab, trimToLimit, utcOffsetMin } from '../shared/meetingContext';
import {
  type ContentEvent,
  type ContextHelloReply,
  MEETING_CONTEXT_KEY,
  type MeetingContextState,
  type Notice,
  type OffscreenCommand,
  type OffscreenEvent,
  type PopupCommand,
  RECORDER_STATUS_KEY,
  type RecorderStatus,
} from '../shared/messages';
import { micPermissionState, openMicPermissionPage } from '../shared/micPermission';
import { initialRecordingState, type RecordingState, type StopReason, transition } from '../shared/recordingState';
import {
  closeSegment,
  collectFinalBatch,
  mergeBatch,
  openSegment,
  readContext,
  settleContext,
  startContext,
} from './contextSession';
import { nextFreeName, recordingName, saveRecording, takenNames } from './save';

export const OFFSCREEN_PAGE = 'src/offscreen/offscreen.html';

const MIN_PROCESS_MS = 10_000;

const BADGE_COLOR = '#d93025';

const idleStatus: RecorderStatus = {
  state: initialRecordingState,
  language: 'bn',
  micIncluded: null,
  notice: null,
  tabId: null,
};

let queue: Promise<unknown> = Promise.resolve();

/** Runs `fn` after every earlier call has finished; a failure rejects only its own caller. */
export function serial<T>(fn: () => Promise<T>): Promise<T> {
  const run = queue.then(fn);
  queue = run.catch(() => undefined);
  return run;
}

export async function readStatus(): Promise<RecorderStatus> {
  const stored = await chrome.storage.session.get(RECORDER_STATUS_KEY);
  return (stored[RECORDER_STATUS_KEY] as RecorderStatus | undefined) ?? idleStatus;
}

export async function writeStatus(status: RecorderStatus): Promise<void> {
  await chrome.storage.session.set({ [RECORDER_STATUS_KEY]: status });
}

async function setBadge(state: RecordingState): Promise<void> {
  const text = state.phase === 'recording' ? 'REC' : state.phase === 'paused' ? 'II' : '';
  await chrome.action.setBadgeBackgroundColor({ color: BADGE_COLOR });
  await chrome.action.setBadgeText({ text });
}

/** Writes the status and the badge that goes with its state. */
async function commit(status: RecorderStatus): Promise<void> {
  await writeStatus(status);
  await setBadge(status.state);
}

/** Chrome allows one offscreen document; either reason's document can serve the recording file's blob URL. */
export async function ensureOffscreen(
  reason: chrome.offscreen.Reason = chrome.offscreen.Reason.USER_MEDIA,
  justification = 'Record the meeting tab and microphone',
): Promise<void> {
  if (await chrome.offscreen.hasDocument()) return;
  await chrome.offscreen.createDocument({ url: OFFSCREEN_PAGE, reasons: [reason], justification });
}

export async function closeOffscreen(): Promise<void> {
  if (await chrome.offscreen.hasDocument()) await chrome.offscreen.closeDocument();
}

async function sendToOffscreen(command: OffscreenCommand): Promise<void> {
  await chrome.runtime.sendMessage(command);
}

function messageOf(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

async function clearContext(): Promise<void> {
  if (!meetingContextEnabled()) return;
  try {
    await chrome.storage.session.remove(MEETING_CONTEXT_KEY);
  } catch {
    console.error('Could not clear the meeting context');
  }
}

/** Stores the captured tab's identity and the first segment. Never fails Start. */
async function storeContext(tab: chrome.tabs.Tab, startedAt: number): Promise<void> {
  try {
    const offset = utcOffsetMin(startedAt);
    const zone = Intl.DateTimeFormat().resolvedOptions().timeZone || null;
    const context: MeetingContextState = {
      identity: identityFromTab({ url: tab.url, title: tab.title }, startedAt, offset, zone),
      segments: [{ wallStart: startedAt, wallEnd: null, offsetMs: 0 }],
      meetingTitle: null,
      participants: [],
      speaking: [],
      openSpeaking: null,
      truncated: [],
      capture: 'reading',
      reloadHint: false,
    };
    await chrome.storage.session.set({ [MEETING_CONTEXT_KEY]: context });
  } catch {
    console.error('Could not store the meeting context');
  }
}

/** The stored context trimmed for the `process` message, or undefined. */
async function contextToSend(): Promise<ReturnType<typeof trimToLimit> | undefined> {
  try {
    const state = await readContext();
    return state && trimToLimit(contextForHandoff(settleContext(state)));
  } catch {
    return undefined;
  }
}

/** Back to idle with an error notice, the offscreen document closed and the badge cleared. */
async function resetWithError(status: RecorderStatus, message: string): Promise<void> {
  await closeOffscreen();
  await clearContext();
  await commit({
    ...status,
    state: initialRecordingState,
    micIncluded: null,
    notice: { kind: 'error', message },
    tabId: null,
  });
}

async function start(language: RecorderStatus['language']): Promise<void> {
  const status = await readStatus();
  if (status.state.phase !== 'idle') return;
  if ((await micPermissionState()) === 'prompt') {
    await openMicPermissionPage();
    return;
  }
  try {
    const [tab] = await chrome.tabs.query({ active: true, lastFocusedWindow: true });
    if (tab?.id === undefined) throw new Error('No active tab to record');
    const streamId = await chrome.tabCapture.getMediaStreamId({ targetTabId: tab.id });
    await ensureOffscreen();
    const state = transition(status.state, { type: 'start', at: Date.now() });
    if (state.phase !== 'recording') return;
    await commit({ state, language, micIncluded: null, notice: null, tabId: tab.id });
    if (meetingContextEnabled()) await storeContext(tab, state.startedAt);
    await sendToOffscreen({ type: 'offscreen_start', streamId, language, startedAt: state.startedAt });
    await startContext(tab.id, state.startedAt);
  } catch (error) {
    await resetWithError(status, messageOf(error));
    throw error;
  }
}

async function pauseOrResume(type: 'pause' | 'resume'): Promise<void> {
  const status = await readStatus();
  const at = Date.now();
  const state = transition(status.state, { type, at });
  if (state === status.state) return;
  await commit({ ...status, state });
  await sendToOffscreen({ type: type === 'pause' ? 'offscreen_pause' : 'offscreen_resume' });
  if (state.phase === 'paused') await closeSegment(at);
  else if (state.phase === 'recording') await openSegment(at, state.recordedMs);
}

/** Moves to `saving`. Before `offscreen_started` the offscreen document would ignore the stop, so it waits. */
async function requestStop(status: RecorderStatus, reason: StopReason): Promise<void> {
  const at = Date.now();
  const state = transition(status.state, { type: 'stop', at, reason });
  if (state === status.state) return;
  await commit({ ...status, state });
  await closeSegment(at);
  if (status.micIncluded !== null) await sendToOffscreen({ type: 'offscreen_stop' });
}

async function offscreenStarted(micIncluded: boolean): Promise<void> {
  const status = await readStatus();
  if (status.state.phase === 'idle') return;
  await writeStatus({ ...status, micIncluded });
  if (status.state.phase === 'saving') await sendToOffscreen({ type: 'offscreen_stop' });
}

async function offscreenStopped(reason: StopReason): Promise<void> {
  const status = await readStatus();
  const saving = transition(status.state, { type: 'stop', at: Date.now(), reason });
  let notice: Notice;
  try {
    if (saving.phase !== 'saving') throw new Error('No recording to save');
    // A cap or capture_ended stop comes from the offscreen document, without requestStop.
    await closeSegment(Date.now());
    // A closed tab cannot answer, so it is not waited for.
    if (status.tabId !== null && saving.reason !== 'tab_closed') await collectFinalBatch(status.tabId);
    const base = recordingName(saving.startedAt);
    notice = await saveRecording({
      startedAt: saving.startedAt,
      name: nextFreeName(base, await takenNames(base)),
      language: status.language,
      process: saving.recordedMs >= MIN_PROCESS_MS,
      context: await contextToSend(),
    });
  } catch (error) {
    notice = { kind: 'error', message: `Could not save the recording (${messageOf(error)}).` };
  }
  const state = transition(saving, { type: 'saved' });
  await clearContext();
  await closeOffscreen();
  await commit({
    ...status,
    state,
    micIncluded: null,
    notice: status.notice?.kind === 'error' ? status.notice : notice,
    tabId: null,
  });
}

async function offscreenError(message: string): Promise<void> {
  const status = await readStatus();
  if (status.micIncluded === null) {
    await resetWithError(status, message);
    return;
  }
  // An `offscreen_stopped` follows once the recording is running.
  await writeStatus({ ...status, notice: { kind: 'error', message } });
}

export function handlePopupCommand(command: PopupCommand): Promise<void> {
  return serial(async () => {
    switch (command.type) {
      case 'start':
        return start(command.language);
      case 'pause':
      case 'resume':
        return pauseOrResume(command.type);
      case 'stop':
        return requestStop(await readStatus(), 'user');
    }
  });
}

export function handleOffscreenEvent(event: OffscreenEvent): Promise<void> {
  return serial(async () => {
    switch (event.type) {
      case 'offscreen_started':
        return offscreenStarted(event.micIncluded);
      case 'offscreen_stopped':
        return offscreenStopped(event.reason);
      case 'offscreen_error':
        return offscreenError(event.message);
    }
  });
}

export function handleTabRemoved(tabId: number): Promise<void> {
  return serial(async () => {
    const status = await readStatus();
    if (status.tabId !== tabId) return;
    await requestStop(status, 'tab_closed');
  });
}

/** Content-script events count only from this extension's recorded tab while it records; the rest is ignored. */
export function handleContentEvent(
  event: ContentEvent,
  sender: chrome.runtime.MessageSender,
): Promise<ContextHelloReply | undefined> {
  return serial(async () => {
    const status = await readStatus();
    const { state } = status;
    const running = state.phase === 'recording' || state.phase === 'paused';
    const fromRecordedTab =
      meetingContextEnabled() && sender.id === chrome.runtime.id && sender.tab?.id === status.tabId && running;
    if (event.type === 'context_hello') {
      return fromRecordedTab && running ? { recording: true, startedAt: state.startedAt } : { recording: false };
    }
    if (fromRecordedTab) await mergeBatch(event);
    return undefined;
  });
}
