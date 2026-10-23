import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { MEETING_CONTEXT_KEY, type MeetingContextState, type Segment } from '../shared/messages';
import {
  ANSWER_MS,
  applyBatch,
  closeSegment,
  collectFinalBatch,
  mapToRecordingMs,
  openSegment,
  startContext,
} from './contextSession';

const flag = vi.hoisted(() => ({ on: true }));
vi.mock('../shared/flags', () => ({ meetingContextEnabled: () => flag.on }));

const T0 = 1_000_000;
const TAB = 7;

function context(overrides: Partial<MeetingContextState> = {}): MeetingContextState {
  return {
    identity: {
      platform: 'meet',
      url: 'https://meet.google.com/abc-defg-hij',
      meeting_id: 'abc-defg-hij',
      tab_title: 'Meet',
      started_at_ms: T0,
      utc_offset_min: 360,
      time_zone: 'Asia/Dhaka',
    },
    segments: [{ wallStart: T0, wallEnd: null, offsetMs: 0 }],
    meetingTitle: null,
    participants: [],
    speaking: [],
    openSpeaking: null,
    truncated: [],
    capture: 'reading',
    reloadHint: false,
    ...overrides,
  };
}

// Recorded 0–60 s, paused 60–120 s, recording again from 120 s (offset 60 s).
const paused: Segment[] = [
  { wallStart: T0, wallEnd: T0 + 60_000, offsetMs: 0 },
  { wallStart: T0 + 120_000, wallEnd: null, offsetMs: 60_000 },
];

const change = (s: number, ...names: string[]) => ({ at: T0 + s * 1000, names });
const interval = (from: number, to: number, ...names: string[]) => ({ fromMs: from * 1000, toMs: to * 1000, names });

describe('mapToRecordingMs', () => {
  it('maps before, inside and after a pause', () => {
    expect(mapToRecordingMs(paused, T0 + 30_000)).toBe(30_000);
    expect(mapToRecordingMs(paused, T0 + 90_000)).toBeNull();
    expect(mapToRecordingMs(paused, T0 + 130_000)).toBe(70_000);
    expect(mapToRecordingMs(paused, T0 - 1)).toBeNull();
  });
});

describe('applyBatch speaking', () => {
  it('turns changes into intervals, closed by the next change', () => {
    const next = applyBatch(context(), { speaking: [change(0, 'A'), change(30, 'B'), change(60)] });
    expect(next.speaking).toEqual([interval(0, 30, 'A'), interval(30, 60, 'B')]);
    expect(next.openSpeaking).toBeNull();
  });

  it('keeps the last change open across batches', () => {
    const first = applyBatch(context(), { speaking: [change(0, 'A')] });
    expect(first.openSpeaking).toEqual(change(0, 'A'));
    expect(applyBatch(first, { speaking: [change(10)] }).speaking).toEqual([interval(0, 10, 'A')]);
  });

  it('maps each change across a pause inside one batch and drops changes while paused', () => {
    const next = applyBatch(context({ segments: paused }), {
      speaking: [change(10, 'A'), change(90, 'B'), change(130, 'A'), change(140)],
    });
    // A ends at the pause; B (during the pause) is dropped; A again from its next change.
    expect(next.speaking).toEqual([interval(10, 60, 'A'), interval(70, 80, 'A')]);
  });

  it('ends an interval still open in a closed segment at its end', () => {
    const next = applyBatch(context({ segments: paused }), { speaking: [change(50, 'A')] });
    expect(next.speaking).toEqual([interval(50, 60, 'A')]);
    expect(next.openSpeaking).toBeNull();
  });

  it('drops intervals under 1 s', () => {
    const next = applyBatch(context(), { speaking: [change(0, 'A'), change(0.5, 'B'), change(5)] });
    expect(next.speaking).toEqual([interval(0.5, 5, 'B')]);
  });

  it('merges touching intervals with the same set of names only', () => {
    const merged = applyBatch(context({ segments: paused }), {
      speaking: [change(10, 'A', 'B'), change(120, 'B', 'A'), change(130, 'C'), change(135)],
    });
    expect(merged.speaking).toEqual([interval(10, 70, 'A', 'B'), interval(70, 75, 'C')]);
  });

  it('keeps the first 10 names and lists speaking in truncated once', () => {
    const names = Array.from({ length: 12 }, (_, i) => `P${i}`);
    const next = applyBatch(context(), { speaking: [change(0, ...names), change(5), change(6, ...names), change(9)] });
    expect(next.speaking[0].names).toEqual(names.slice(0, 10));
    expect(next.truncated).toEqual(['speaking']);
  });

  it('keeps the earliest 5,000 intervals', () => {
    const speaking = Array.from({ length: 5_001 }, (_, i) => interval(i * 2, i * 2 + 1, i % 2 ? 'A' : 'B'));
    const full = context({ speaking: speaking.slice(0, 5_000) });
    const next = applyBatch(full, { speaking: [change(20_000, 'C'), change(20_005)] });
    expect(next.speaking).toHaveLength(5_000);
    expect(next.speaking.at(-1)).toEqual(full.speaking.at(-1));
    expect(next.truncated).toEqual(['speaking']);
  });
});

describe('applyBatch roster, title and capture', () => {
  it('unions participants by name, cleans them and keeps self', () => {
    const first = applyBatch(context(), { participants: [{ name: ' Ann ', self: false }] });
    const next = applyBatch(first, {
      participants: [
        { name: 'Ann', self: true },
        { name: 'Bob\u0007', self: false },
        { name: '   ', self: false },
      ],
    });
    expect(next.participants).toEqual([
      { name: 'Ann', self: true },
      { name: 'Bob', self: false },
    ]);
    expect(applyBatch(next, { participants: [{ name: 'Ann', self: false }] }).participants[0].self).toBe(true);
  });

  it('keeps the first 200 names', () => {
    const participants = Array.from({ length: 201 }, (_, i) => ({ name: `P${i}`, self: false }));
    const next = applyBatch(context(), { participants });
    expect(next.participants).toHaveLength(200);
    expect(next.participants.at(-1)?.name).toBe('P199');
    expect(next.truncated).toEqual(['participants']);
  });

  it('cleans and cuts the title, and keeps it when a batch has none', () => {
    const next = applyBatch(context(), { meetingTitle: ` ${'x'.repeat(400)}` });
    expect(next.meetingTitle).toBe('x'.repeat(300));
    expect(applyBatch(next, { meetingTitle: null }).meetingTitle).toBe('x'.repeat(300));
  });

  it('moves capture through reading, captured, gave up and recovery', () => {
    expect(applyBatch(context(), {}).capture).toBe('reading');
    expect(applyBatch(context(), { meetingTitle: 'Sync' }).capture).toBe('captured');
    expect(applyBatch(context(), { participants: [] }).capture).toBe('reading');
    const gaveUp = applyBatch(context(), { gaveUp: true });
    expect(gaveUp).toMatchObject({ capture: 'unavailable', reloadHint: false });
    expect(applyBatch(gaveUp, { participants: [{ name: 'Ann', self: true }] }).capture).toBe('captured');
    expect(applyBatch(context({ capture: 'captured' }), { gaveUp: true }).capture).toBe('captured');
  });

  it.each([
    null,
    'x',
    { participants: [{ name: 1, self: false }] },
    { speaking: [{ at: 'now', names: [] }] },
    { meetingTitle: 3 },
    { gaveUp: 'yes' },
  ])('ignores a malformed batch %j', (batch) => {
    const state = context();
    expect(applyBatch(state, batch)).toBe(state);
  });
});

describe('storage and the content script', () => {
  let stored: Record<string, unknown>;
  const tabsSendMessage = vi.fn();

  beforeEach(() => {
    vi.useFakeTimers();
    flag.on = true;
    stored = { [MEETING_CONTEXT_KEY]: context() };
    tabsSendMessage.mockReset();
    vi.stubGlobal('chrome', {
      storage: {
        session: {
          get: async (key: string) => ({ [key]: stored[key] }),
          set: async (items: Record<string, unknown>) => Object.assign(stored, items),
        },
      },
      tabs: { sendMessage: tabsSendMessage },
    });
  });

  afterEach(() => {
    vi.useRealTimers();
    vi.unstubAllGlobals();
  });

  const state = () => stored[MEETING_CONTEXT_KEY] as MeetingContextState;

  it('closes and opens segments, ending an open interval at the pause', async () => {
    stored[MEETING_CONTEXT_KEY] = context({ openSpeaking: change(10, 'A') });
    await closeSegment(T0 + 60_000);
    await closeSegment(T0 + 70_000);
    await openSegment(T0 + 120_000, 60_000);
    expect(state().segments).toEqual(paused);
    expect(state().speaking).toEqual([interval(10, 60, 'A')]);
    expect(state().openSpeaking).toBeNull();
  });

  it('sends context_start to a Meet tab and leaves the state alone on success', async () => {
    tabsSendMessage.mockResolvedValue({ ok: true });
    await startContext(TAB, T0);
    expect(tabsSendMessage).toHaveBeenCalledWith(TAB, { type: 'context_start', startedAt: T0 });
    expect(state()).toEqual(context());
  });

  it('sets unavailable with the reload hint when context_start is rejected', async () => {
    tabsSendMessage.mockRejectedValue(new Error('Could not establish connection'));
    await startContext(TAB, T0);
    expect(state()).toEqual(context({ capture: 'unavailable', reloadHint: true }));
  });

  it('sets unavailable with the reload hint when context_start is not answered in 2 s', async () => {
    tabsSendMessage.mockReturnValue(new Promise(() => undefined));
    const started = startContext(TAB, T0);
    await vi.advanceTimersByTimeAsync(ANSWER_MS);
    await started;
    expect(state()).toMatchObject({ capture: 'unavailable', reloadHint: true });
  });

  it('sends nothing to a tab that is not Meet', async () => {
    stored[MEETING_CONTEXT_KEY] = context({ identity: { ...context().identity, platform: 'zoom' } });
    await startContext(TAB, T0);
    await collectFinalBatch(TAB);
    expect(tabsSendMessage).not.toHaveBeenCalled();
  });

  it('merges the final batch', async () => {
    stored[MEETING_CONTEXT_KEY] = context({ segments: [{ wallStart: T0, wallEnd: T0 + 60_000, offsetMs: 0 }] });
    tabsSendMessage.mockResolvedValue({ participants: [{ name: 'Ann', self: true }], speaking: [change(56, 'Ann')] });
    await collectFinalBatch(TAB);
    expect(tabsSendMessage).toHaveBeenCalledWith(TAB, { type: 'context_stop' });
    expect(state()).toMatchObject({
      capture: 'captured',
      participants: [{ name: 'Ann', self: true }],
      speaking: [interval(56, 60, 'Ann')],
    });
  });

  it('hands off after 2 s when context_stop is not answered', async () => {
    tabsSendMessage.mockReturnValue(new Promise(() => undefined));
    let done = false;
    const collected = collectFinalBatch(TAB).then(() => {
      done = true;
    });
    await vi.advanceTimersByTimeAsync(ANSWER_MS - 1);
    expect(done).toBe(false);
    await vi.advanceTimersByTimeAsync(1);
    await collected;
    expect(state()).toEqual(context());
  });

  it('hands off at once when context_stop is rejected', async () => {
    tabsSendMessage.mockRejectedValue(new Error('tab gone'));
    await collectFinalBatch(TAB);
    expect(vi.getTimerCount()).toBe(0);
    expect(state()).toEqual(context());
  });

  it('does nothing with the flag off', async () => {
    flag.on = false;
    await startContext(TAB, T0);
    await collectFinalBatch(TAB);
    await closeSegment(T0 + 1);
    expect(tabsSendMessage).not.toHaveBeenCalled();
    expect(state()).toEqual(context());
  });
});
