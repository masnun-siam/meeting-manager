import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { MEETING_CONTEXT_KEY, RECORDER_STATUS_KEY, type RecorderStatus } from '../shared/messages';
import { MIC_PERMISSION_PAGE } from '../shared/micPermission';
import { handleOffscreenEvent, handlePopupCommand, handleTabRemoved, OFFSCREEN_PAGE } from './recordingControl';
import { saveRecording } from './save';

const flag = vi.hoisted(() => ({ on: false }));
vi.mock('../shared/flags', () => ({ meetingContextEnabled: () => flag.on }));

vi.mock('./save', async (importOriginal) => ({
  ...(await importOriginal<typeof import('./save')>()),
  saveRecording: vi.fn(async () => ({ kind: 'saved' })),
  takenNames: vi.fn(async () => new Set<string>()),
}));

const T0 = new Date('2026-01-01T10:00:00Z').getTime();
const TAB = 7;

let stored: Record<string, unknown>;
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

const status = () => stored[RECORDER_STATUS_KEY] as RecorderStatus;
const sent = () => sendMessage.mock.calls.map(([m]) => m.type);

async function startRecording(): Promise<void> {
  await handlePopupCommand({ type: 'start', language: 'en' });
}

beforeEach(() => {
  vi.useFakeTimers();
  vi.setSystemTime(T0);
  stored = {};
  hasDocument = false;
  permission = 'granted';
  flag.on = false;
  failContextWrite = false;
  for (const mock of [setBadgeText, getMediaStreamId, createDocument, closeDocument, sendMessage, tabsCreate]) {
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
    },
    action: { setBadgeText, setBadgeBackgroundColor: vi.fn() },
    tabs: { query: tabsQuery, create: tabsCreate },
    tabCapture: { getMediaStreamId },
    offscreen: {
      hasDocument: async () => hasDocument,
      createDocument,
      closeDocument,
      Reason: { USER_MEDIA: 'USER_MEDIA' },
    },
    runtime: { sendMessage, getURL: (path: string) => `chrome-extension://id/${path}` },
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
    await stop();
    expect(vi.mocked(saveRecording).mock.lastCall?.[0].context).toBeUndefined();
  });
});

describe('running recording', () => {
  beforeEach(startRecording);

  it('sets micIncluded on offscreen_started', async () => {
    await handleOffscreenEvent({ type: 'offscreen_started', micIncluded: true });
    expect(status().micIncluded).toBe(true);
  });

  it('pauses and resumes', async () => {
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
