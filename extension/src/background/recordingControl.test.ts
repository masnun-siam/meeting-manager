import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { LAST_RECORDING_KEY } from '../shared/lastRecording';
import {
  type ContextUpdate,
  MEETING_CONTEXT_KEY,
  type MeetingContextState,
  type PopupCommand,
  RECORDER_STATUS_KEY,
  type RecorderStatus,
} from '../shared/messages';
import { MIC_PERMISSION_PAGE } from '../shared/micPermission';
import {
  handleContentEvent,
  handleOffscreenEvent,
  handlePopupCommand,
  handleTabRemoved,
  OFFSCREEN_PAGE,
} from './recordingControl';
import { saveRecording } from './save';

const flag = vi.hoisted(() => ({ on: false, continueOn: false }));
vi.mock('../shared/flags', () => ({
  meetingContextEnabled: () => flag.on,
  continueRecordingEnabled: () => flag.continueOn,
}));

vi.mock('./save', async (importOriginal) => ({
  ...(await importOriginal<typeof import('./save')>()),
  saveRecording: vi.fn(async () => ({ kind: 'saved' })),
  takenNames: vi.fn(async () => new Set<string>()),
}));

const T0 = new Date('2026-01-01T10:00:00Z').getTime();
const TAB = 7;
const EXTENSION = 'ext-id';

let stored: Record<string, unknown>;
let local: Record<string, unknown>;
let failLocalRemove = false;
let hasDocument: boolean;
let permission: PermissionState;
let failContextWrite = false;
const setBadgeText = vi.fn();
const getMediaStreamId = vi.fn();
const createDocument = vi.fn();
const closeDocument = vi.fn();
const sendMessage = vi.fn();
const tabsCreate = vi.fn();
const tabsQuery = vi.fn();
const tabsSendMessage = vi.fn();

const status = () => stored[RECORDER_STATUS_KEY] as RecorderStatus;
const sent = () => sendMessage.mock.calls.map(([m]) => m.type);

async function startRecording(): Promise<void> {
  await handlePopupCommand({ type: 'start', language: 'en' });
}

beforeEach(() => {
  vi.useFakeTimers();
  vi.setSystemTime(T0);
  stored = {};
  local = {};
  failLocalRemove = false;
  hasDocument = false;
  permission = 'granted';
  flag.on = false;
  flag.continueOn = false;
  failContextWrite = false;
  for (const mock of [
    setBadgeText,
    getMediaStreamId,
    createDocument,
    closeDocument,
    sendMessage,
    tabsCreate,
    tabsSendMessage,
  ]) {
    mock.mockReset();
  }
  getMediaStreamId.mockResolvedValue('stream-1');
  tabsQuery.mockReset().mockResolvedValue([{ id: TAB }]);
  createDocument.mockImplementation(async () => {
    hasDocument = true;
  });
  closeDocument.mockImplementation(async () => {
    hasDocument = false;
  });
  vi.stubGlobal('navigator', { permissions: { query: async () => ({ state: permission }) } });
  vi.stubGlobal('chrome', {
    storage: {
      session: {
        get: async (key: string) => ({ [key]: stored[key] }),
        set: async (items: Record<string, unknown>) => {
          if (failContextWrite && MEETING_CONTEXT_KEY in items) throw new Error('quota');
          Object.assign(stored, items);
        },
        remove: async (key: string) => {
          delete stored[key];
        },
      },
      local: {
        remove: async (key: string) => {
          if (failLocalRemove) throw new Error('quota');
          delete local[key];
        },
      },
    },
    action: { setBadgeText, setBadgeBackgroundColor: vi.fn() },
    tabs: { query: tabsQuery, create: tabsCreate, sendMessage: tabsSendMessage },
    tabCapture: { getMediaStreamId },
    offscreen: {
      hasDocument: async () => hasDocument,
      createDocument,
      closeDocument,
      Reason: { USER_MEDIA: 'USER_MEDIA' },
    },
    runtime: { id: EXTENSION, sendMessage, getURL: (path: string) => `chrome-extension://id/${path}` },
  });
});

afterEach(() => {
  vi.useRealTimers();
  vi.unstubAllGlobals();
});

describe('start', () => {
  it('opens the mic page and does nothing else when the permission is prompt', async () => {
    permission = 'prompt';
    await startRecording();

    expect(tabsCreate).toHaveBeenCalledWith({ url: `chrome-extension://id/${MIC_PERMISSION_PAGE}` });
    expect(createDocument).not.toHaveBeenCalled();
    expect(stored[RECORDER_STATUS_KEY]).toBeUndefined();
  });

  it.each(['granted', 'denied'] as const)('records when the permission is %s', async (state) => {
    permission = state;
    await startRecording();

    expect(tabsQuery).toHaveBeenCalledWith({ active: true, lastFocusedWindow: true });
    expect(getMediaStreamId).toHaveBeenCalledWith({ targetTabId: TAB });
    expect(createDocument).toHaveBeenCalledWith(
      expect.objectContaining({ url: OFFSCREEN_PAGE, reasons: ['USER_MEDIA'] }),
    );
    expect(status()).toEqual({
      state: { phase: 'recording', startedAt: T0, recordedMs: 0, resumedAt: T0 },
      language: 'en',
      micIncluded: null,
      notice: null,
      tabId: TAB,
    });
    expect(setBadgeText).toHaveBeenLastCalledWith({ text: 'REC' });
    expect(sendMessage).toHaveBeenCalledWith({
      type: 'offscreen_start',
      streamId: 'stream-1',
      language: 'en',
      startedAt: T0,
    });
  });

  it('removes the last recording with the flag on', async () => {
    flag.continueOn = true;
    local[LAST_RECORDING_KEY] = { download_id: 1 };
    await startRecording();
    expect(local[LAST_RECORDING_KEY]).toBeUndefined();
    expect(status().state.phase).toBe('recording');
  });

  it('keeps the last recording with the flag off', async () => {
    local[LAST_RECORDING_KEY] = { download_id: 1 };
    await startRecording();
    expect(local[LAST_RECORDING_KEY]).toEqual({ download_id: 1 });
  });

  it('still records when removing the last recording fails', async () => {
    const error = vi.spyOn(console, 'error').mockImplementation(() => undefined);
    flag.continueOn = true;
    failLocalRemove = true;
    await startRecording();
    expect(status().state.phase).toBe('recording');
    expect(sent()).toContain('offscreen_start');
    expect(error).toHaveBeenCalledWith('Could not clear the last recording');
    error.mockRestore();
  });

  it('reuses an existing offscreen document', async () => {
    hasDocument = true;
    await startRecording();
    expect(createDocument).not.toHaveBeenCalled();
  });

  it('is ignored while a recording is running', async () => {
    await startRecording();
    sendMessage.mockClear();
    await startRecording();
    expect(sendMessage).not.toHaveBeenCalled();
  });

  it.each([
    ['no active tab', () => tabsQuery.mockResolvedValue([]), 'No active tab to record'],
    [
      'getMediaStreamId rejects',
      () => getMediaStreamId.mockRejectedValue(new Error('capture denied')),
      'capture denied',
    ],
    ['createDocument rejects', () => createDocument.mockRejectedValue(new Error('no document')), 'no document'],
  ])('leaves idle with an error notice when %s', async (_name, arrange, message) => {
    arrange();
    await expect(startRecording()).rejects.toThrow(message);

    expect(status().state).toEqual({ phase: 'idle', lastStop: null });
    expect(status().notice).toEqual({ kind: 'error', message });
    expect(status().tabId).toBeNull();
    expect(setBadgeText).toHaveBeenLastCalledWith({ text: '' });
    expect(sendMessage).not.toHaveBeenCalled();
  });
});

describe('meeting context', () => {
  beforeEach(() => {
    flag.on = true;
    tabsQuery.mockResolvedValue([
      { id: TAB, url: 'https://meet.google.com/abc-defg-hij?authuser=1', title: 'Standup' },
    ]);
  });

  const stop = async () => {
    await handleOffscreenEvent({ type: 'offscreen_started', micIncluded: true });
    await handlePopupCommand({ type: 'stop' });
    await handleOffscreenEvent({ type: 'offscreen_stopped', startedAt: T0, reason: 'user' });
  };

  it('stores the identity and the first segment at start', async () => {
    await startRecording();
    expect(stored[MEETING_CONTEXT_KEY]).toMatchObject({
      identity: {
        platform: 'meet',
        url: 'https://meet.google.com/abc-defg-hij',
        meeting_id: 'abc-defg-hij',
        tab_title: 'Standup',
        started_at_ms: T0,
        utc_offset_min: -new Date(T0).getTimezoneOffset() || 0,
      },
      segments: [{ wallStart: T0, wallEnd: null, offsetMs: 0 }],
      capture: 'reading',
    });
  });

  it('sends the context at hand-off and clears it', async () => {
    await startRecording();
    await stop();
    expect(saveRecording).toHaveBeenLastCalledWith(
      expect.objectContaining({
        context: expect.objectContaining({ version: 1, platform: 'meet', started_at_ms: T0 }),
      }),
    );
    expect(stored[MEETING_CONTEXT_KEY]).toBeUndefined();
  });

  it('clears it when saving throws', async () => {
    vi.mocked(saveRecording).mockRejectedValueOnce(new Error('kaboom'));
    await startRecording();
    await stop();
    expect(stored[MEETING_CONTEXT_KEY]).toBeUndefined();
  });

  it('does not stop Start when the context cannot be stored', async () => {
    failContextWrite = true;
    vi.spyOn(console, 'error').mockImplementation(() => undefined);
    await startRecording();
    expect(status().state.phase).toBe('recording');
  });

  it('clears a leftover context when Start fails', async () => {
    stored[MEETING_CONTEXT_KEY] = { old: true };
    getMediaStreamId.mockRejectedValueOnce(new Error('no capture'));
    await expect(startRecording()).rejects.toThrow();
    expect(stored[MEETING_CONTEXT_KEY]).toBeUndefined();
  });

  it('writes nothing and sends no context with the flag off', async () => {
    flag.on = false;
    await startRecording();
    expect(stored[MEETING_CONTEXT_KEY]).toBeUndefined();
    await handleContentEvent(update, recordedTab);
    expect(await handleContentEvent({ type: 'context_hello' }, recordedTab)).toEqual({ recording: false });
    await stop();
    expect(vi.mocked(saveRecording).mock.lastCall?.[0].context).toBeUndefined();
    expect(tabsSendMessage).not.toHaveBeenCalled();
  });

  const recordedTab = { id: EXTENSION, tab: { id: TAB } as chrome.tabs.Tab };
  const update: ContextUpdate = { type: 'context_update', participants: [{ name: 'Ann', self: true }] };
  const context = () => stored[MEETING_CONTEXT_KEY] as MeetingContextState;

  it('sends context_start to a Meet tab after Start', async () => {
    tabsSendMessage.mockResolvedValue({ ok: true });
    await startRecording();
    expect(tabsSendMessage).toHaveBeenCalledWith(TAB, { type: 'context_start', startedAt: T0 });
    expect(context()).toMatchObject({ capture: 'reading', reloadHint: false });
  });

  it('sends no context_start to a Zoom tab', async () => {
    tabsQuery.mockResolvedValue([{ id: TAB, url: 'https://app.zoom.us/wc/123456789/join', title: 'Zoom' }]);
    await startRecording();
    expect(tabsSendMessage).not.toHaveBeenCalled();
  });

  it('keeps recording when context_start throws, with the reload hint', async () => {
    tabsSendMessage.mockRejectedValue(new Error('Could not establish connection'));
    await startRecording();
    expect(status().state.phase).toBe('recording');
    expect(context()).toMatchObject({ capture: 'unavailable', reloadHint: true });
  });

  it('closes and opens segments on pause, resume and stop', async () => {
    await startRecording();
    vi.setSystemTime(T0 + 60_000);
    await handlePopupCommand({ type: 'pause' });
    vi.setSystemTime(T0 + 120_000);
    await handlePopupCommand({ type: 'resume' });
    vi.setSystemTime(T0 + 150_000);
    await handleOffscreenEvent({ type: 'offscreen_started', micIncluded: true });
    await handlePopupCommand({ type: 'stop' });
    expect(context().segments).toEqual([
      { wallStart: T0, wallEnd: T0 + 60_000, offsetMs: 0 },
      { wallStart: T0 + 120_000, wallEnd: T0 + 150_000, offsetMs: 60_000 },
    ]);
  });

  it('merges batches from the recorded tab while recording or paused', async () => {
    await startRecording();
    await handleContentEvent(update, recordedTab);
    await handlePopupCommand({ type: 'pause' });
    await handleContentEvent({ type: 'context_update', participants: [{ name: 'Bob', self: false }] }, recordedTab);
    expect(context()).toMatchObject({
      capture: 'captured',
      participants: [
        { name: 'Ann', self: true },
        { name: 'Bob', self: false },
      ],
    });
  });

  it.each([
    ['another tab', { id: EXTENSION, tab: { id: TAB + 1 } as chrome.tabs.Tab }],
    ['another extension', { id: 'other', tab: { id: TAB } as chrome.tabs.Tab }],
    ['no tab', { id: EXTENSION }],
  ])('ignores a batch from %s', async (_, sender) => {
    await startRecording();
    const before = structuredClone(context());
    await handleContentEvent(update, sender);
    expect(context()).toEqual(before);
    expect(await handleContentEvent({ type: 'context_hello' }, sender)).toEqual({ recording: false });
  });

  it('ignores batches while saving and after hand-off', async () => {
    await startRecording();
    await handleOffscreenEvent({ type: 'offscreen_started', micIncluded: true });
    await handlePopupCommand({ type: 'stop' });
    const before = structuredClone(context());
    await handleContentEvent(update, recordedTab);
    expect(context()).toEqual(before);
    expect(await handleContentEvent({ type: 'context_hello' }, recordedTab)).toEqual({ recording: false });
    await handleOffscreenEvent({ type: 'offscreen_stopped', startedAt: T0, reason: 'user' });
    await handleContentEvent(update, recordedTab);
    expect(stored[MEETING_CONTEXT_KEY]).toBeUndefined();
  });

  it('answers context_hello from the recorded tab, so a reloaded tab carries on', async () => {
    await startRecording();
    await handleContentEvent(update, recordedTab);
    expect(await handleContentEvent({ type: 'context_hello' }, recordedTab)).toEqual({
      recording: true,
      startedAt: T0,
    });
    await handleContentEvent({ type: 'context_update', participants: [{ name: 'Bob', self: false }] }, recordedTab);
    expect(context().participants.map((p) => p.name)).toEqual(['Ann', 'Bob']);
  });

  it('merges the final batch before the hand-off', async () => {
    await startRecording();
    tabsSendMessage.mockResolvedValue(update);
    await stop();
    expect(tabsSendMessage).toHaveBeenLastCalledWith(TAB, { type: 'context_stop' });
    expect(saveRecording).toHaveBeenLastCalledWith(
      expect.objectContaining({ context: expect.objectContaining({ participants: [{ name: 'Ann', self: true }] }) }),
    );
  });

  it.each(['cap', 'capture_ended'] as const)(
    'closes the segment and the open interval on a %s stop',
    async (reason) => {
      await startRecording();
      vi.setSystemTime(T0 + 5_000);
      await handleContentEvent({ type: 'context_update', speaking: [{ at: T0 + 1_000, names: ['Ann'] }] }, recordedTab);
      vi.setSystemTime(T0 + 10_000);
      await handleOffscreenEvent({ type: 'offscreen_started', micIncluded: true });
      await handleOffscreenEvent({ type: 'offscreen_stopped', startedAt: T0, reason });
      expect(saveRecording).toHaveBeenLastCalledWith(
        expect.objectContaining({
          context: expect.objectContaining({ speaking: [{ from: '00:00:01', to: '00:00:10', names: ['Ann'] }] }),
        }),
      );
    },
  );

  it('does not ask a closed tab for the final batch', async () => {
    await startRecording();
    tabsSendMessage.mockClear();
    await handleOffscreenEvent({ type: 'offscreen_started', micIncluded: true });
    await handleTabRemoved(TAB);
    await handleOffscreenEvent({ type: 'offscreen_stopped', startedAt: T0, reason: 'tab_closed' });
    expect(tabsSendMessage).not.toHaveBeenCalled();
    expect(saveRecording).toHaveBeenCalled();
  });
});

describe('running recording', () => {
  beforeEach(startRecording);

  it('sets micIncluded on offscreen_started', async () => {
    await handleOffscreenEvent({ type: 'offscreen_started', micIncluded: true });
    expect(status().micIncluded).toBe(true);
  });

  it('pauses and resumes', async () => {
    await handleOffscreenEvent({ type: 'offscreen_started', micIncluded: true });
    vi.setSystemTime(T0 + 60_000);
    await handlePopupCommand({ type: 'pause' });
    expect(status().state).toEqual({ phase: 'paused', startedAt: T0, recordedMs: 60_000 });
    expect(setBadgeText).toHaveBeenLastCalledWith({ text: 'II' });
    expect(sendMessage).toHaveBeenLastCalledWith({ type: 'offscreen_pause' });

    vi.setSystemTime(T0 + 120_000);
    await handlePopupCommand({ type: 'resume' });
    expect(status().state).toMatchObject({ phase: 'recording', recordedMs: 60_000, resumedAt: T0 + 120_000 });
    expect(setBadgeText).toHaveBeenLastCalledWith({ text: 'REC' });
    expect(sendMessage).toHaveBeenLastCalledWith({ type: 'offscreen_resume' });
  });

  it('stops, then returns to idle with the reason when the offscreen document reports it', async () => {
    await handleOffscreenEvent({ type: 'offscreen_started', micIncluded: true });
    await handlePopupCommand({ type: 'stop' });
    expect(status().state).toMatchObject({ phase: 'saving', reason: 'user' });
    expect(sendMessage).toHaveBeenLastCalledWith({ type: 'offscreen_stop' });

    await handleOffscreenEvent({ type: 'offscreen_stopped', startedAt: T0, reason: 'user' });
    expect(status().state).toEqual({ phase: 'idle', lastStop: 'user' });
    expect(status().tabId).toBeNull();
    expect(closeDocument).toHaveBeenCalled();
    expect(setBadgeText).toHaveBeenLastCalledWith({ text: '' });
  });

  it.each([
    [9_999, false, 'saved'],
    [10_000, true, 'saved'],
  ])('processes only from 10 s: %i ms -> %s', async (ms, process) => {
    await handleOffscreenEvent({ type: 'offscreen_started', micIncluded: true });
    vi.setSystemTime(T0 + ms);
    await handlePopupCommand({ type: 'stop' });
    await handleOffscreenEvent({ type: 'offscreen_stopped', startedAt: T0, reason: 'user' });
    expect(saveRecording).toHaveBeenLastCalledWith(
      expect.objectContaining({ startedAt: T0, language: 'en', process, name: expect.stringMatching(/^meeting-/) }),
    );
    expect(status().notice).toEqual({ kind: 'saved' });
    expect(status().state.phase).toBe('idle');
  });

  it('asks the save to remember the recording with its stop', async () => {
    await handleOffscreenEvent({ type: 'offscreen_started', micIncluded: true });
    vi.setSystemTime(T0 + 30_000);
    await handlePopupCommand({ type: 'stop' });
    vi.setSystemTime(T0 + 31_000);
    await handleOffscreenEvent({ type: 'offscreen_stopped', startedAt: T0, reason: 'user' });
    expect(saveRecording).toHaveBeenLastCalledWith(
      expect.objectContaining({ remember: { recordedMs: 30_000, stoppedAt: T0 + 31_000, stopReason: 'user' } }),
    );
  });

  it('still reaches idle with an error notice when saving throws', async () => {
    vi.mocked(saveRecording).mockRejectedValueOnce(new Error('kaboom'));
    await handleOffscreenEvent({ type: 'offscreen_started', micIncluded: true });
    await handlePopupCommand({ type: 'stop' });
    await handleOffscreenEvent({ type: 'offscreen_stopped', startedAt: T0, reason: 'user' });
    expect(status().state.phase).toBe('idle');
    expect(status().notice).toMatchObject({ kind: 'error' });
    expect(setBadgeText).toHaveBeenLastCalledWith({ text: '' });
  });

  it('holds a stop until offscreen_started arrives', async () => {
    await handlePopupCommand({ type: 'stop' });
    expect(status().state.phase).toBe('saving');
    expect(sent()).toEqual(['offscreen_start']);

    await handleOffscreenEvent({ type: 'offscreen_started', micIncluded: false });
    expect(sent()).toEqual(['offscreen_start', 'offscreen_stop']);
  });

  it('holds a pause until offscreen_started arrives', async () => {
    await handlePopupCommand({ type: 'pause' });
    expect(status().state.phase).toBe('paused');
    expect(setBadgeText).toHaveBeenLastCalledWith({ text: 'II' });
    expect(sent()).toEqual(['offscreen_start']);

    await handleOffscreenEvent({ type: 'offscreen_started', micIncluded: true });
    expect(sent()).toEqual(['offscreen_start', 'offscreen_pause']);
    expect(status().state.phase).toBe('paused');
    expect(status().micIncluded).toBe(true);
  });

  it('drops a pending pause when resumed before offscreen_started', async () => {
    await handlePopupCommand({ type: 'pause' });
    await handlePopupCommand({ type: 'resume' });
    expect(status().state.phase).toBe('recording');

    await handleOffscreenEvent({ type: 'offscreen_started', micIncluded: true });
    expect(sent()).toEqual(['offscreen_start']);
  });

  it('sends only the stop for a pause then stop before offscreen_started', async () => {
    await handlePopupCommand({ type: 'pause' });
    await handlePopupCommand({ type: 'stop' });
    expect(status().state.phase).toBe('saving');

    await handleOffscreenEvent({ type: 'offscreen_started', micIncluded: true });
    expect(sent()).toEqual(['offscreen_start', 'offscreen_stop']);
  });

  it('stops with tab_closed when the captured tab closes, and keeps that reason', async () => {
    await handleOffscreenEvent({ type: 'offscreen_started', micIncluded: true });
    await handleTabRemoved(TAB);
    expect(status().state).toMatchObject({ phase: 'saving', reason: 'tab_closed' });
    expect(sendMessage).toHaveBeenLastCalledWith({ type: 'offscreen_stop' });

    await handleOffscreenEvent({ type: 'offscreen_stopped', startedAt: T0, reason: 'capture_ended' });
    expect(status().state).toEqual({ phase: 'idle', lastStop: 'tab_closed' });
  });

  it('ignores another tab closing', async () => {
    await handleTabRemoved(TAB + 1);
    expect(status().state.phase).toBe('recording');
    expect(sent()).toEqual(['offscreen_start']);
  });

  it('records a cap stop', async () => {
    await handleOffscreenEvent({ type: 'offscreen_started', micIncluded: true });
    await handleOffscreenEvent({ type: 'offscreen_stopped', startedAt: T0, reason: 'cap' });
    expect(status().state).toEqual({ phase: 'idle', lastStop: 'cap' });
  });

  it('resets to idle with an error notice on offscreen_error before started', async () => {
    await handleOffscreenEvent({ type: 'offscreen_error', message: 'no audio' });
    expect(status().state).toEqual({ phase: 'idle', lastStop: null });
    expect(status().notice).toEqual({ kind: 'error', message: 'no audio' });
    expect(closeDocument).toHaveBeenCalled();
    expect(setBadgeText).toHaveBeenLastCalledWith({ text: '' });
  });

  it('only sets the notice on offscreen_error after started', async () => {
    await handleOffscreenEvent({ type: 'offscreen_started', micIncluded: true });
    await handleOffscreenEvent({ type: 'offscreen_error', message: 'disk full' });
    expect(status().state.phase).toBe('recording');
    expect(status().notice).toEqual({ kind: 'error', message: 'disk full' });
    expect(closeDocument).not.toHaveBeenCalled();
  });

  it('keeps the error notice through the stop that follows', async () => {
    await handleOffscreenEvent({ type: 'offscreen_started', micIncluded: true });
    await handleOffscreenEvent({ type: 'offscreen_error', message: 'disk full' });
    await handleOffscreenEvent({ type: 'offscreen_stopped', startedAt: T0, reason: 'capture_ended' });
    expect(status().state).toEqual({ phase: 'idle', lastStop: 'capture_ended' });
    expect(status().notice).toEqual({ kind: 'error', message: 'disk full' });
  });
});

describe('dismiss', () => {
  const idle = (extra: Partial<RecorderStatus> = {}): RecorderStatus => ({
    state: { phase: 'idle', lastStop: 'tab_closed' },
    language: 'en',
    micIncluded: null,
    notice: { kind: 'saved' },
    tabId: null,
    ...extra,
  });

  it('clears the notice and leaves the rest', async () => {
    stored[RECORDER_STATUS_KEY] = idle();
    await handlePopupCommand({ type: 'dismiss', what: 'notice' });

    expect(status()).toEqual(idle({ notice: null }));
    expect(setBadgeText).not.toHaveBeenCalled();
  });

  it('is fine with a notice that is already clear', async () => {
    stored[RECORDER_STATUS_KEY] = idle({ notice: null });
    await handlePopupCommand({ type: 'dismiss', what: 'notice' });

    expect(status()).toEqual(idle({ notice: null }));
  });

  it('clears the stop reason when idle', async () => {
    stored[RECORDER_STATUS_KEY] = idle();
    await handlePopupCommand({ type: 'dismiss', what: 'stop' });

    expect(status()).toEqual(idle({ state: { phase: 'idle', lastStop: null } }));
  });

  it('changes nothing for the stop reason outside idle', async () => {
    await startRecording();
    const before = status();
    await handlePopupCommand({ type: 'dismiss', what: 'stop' });

    expect(status()).toBe(before);
  });

  it('hides the mic warning for this recording only', async () => {
    await startRecording();
    await handleOffscreenEvent({ type: 'offscreen_started', micIncluded: false });
    await handlePopupCommand({ type: 'dismiss', what: 'mic' });
    expect(status()).toMatchObject({ micIncluded: false, micWarningDismissed: true });

    await handlePopupCommand({ type: 'stop' });
    await handleOffscreenEvent({ type: 'offscreen_stopped', startedAt: T0, reason: 'user' });
    await startRecording();

    expect(status().micWarningDismissed).toBeUndefined();
  });

  it.each([{ type: 'dismiss', what: 'context' }, { type: 'dismiss' }])(
    'ignores %j without a write',
    async (command) => {
      stored[RECORDER_STATUS_KEY] = idle();
      const before = status();

      await expect(handlePopupCommand(command as PopupCommand)).resolves.toBeUndefined();
      expect(status()).toBe(before);
    },
  );

  it('runs after a command already queued', async () => {
    await startRecording();
    await handleOffscreenEvent({ type: 'offscreen_started', micIncluded: true });
    await handlePopupCommand({ type: 'stop' });

    const stopped = handleOffscreenEvent({ type: 'offscreen_stopped', startedAt: T0, reason: 'user' });
    const dismissed = handlePopupCommand({ type: 'dismiss', what: 'notice' });
    await Promise.all([stopped, dismissed]);

    expect(status().state).toEqual({ phase: 'idle', lastStop: 'user' });
    expect(status().notice).toBeNull();
  });
});

describe('ordering', () => {
  it('handles events one at a time, in order', async () => {
    await Promise.all([
      startRecording(),
      handleOffscreenEvent({ type: 'offscreen_started', micIncluded: true }),
      handlePopupCommand({ type: 'pause' }),
    ]);
    expect(status().state.phase).toBe('paused');
    expect(status().micIncluded).toBe(true);
  });
});
