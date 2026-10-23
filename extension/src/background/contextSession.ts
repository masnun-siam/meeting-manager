// Meeting context session: merges what the Meet content script reports into `meeting_context`.
// The pure helpers take every time as an argument; the storage helpers run inside recordingControl's serial(),
// read the state from storage each time (the service worker sleeps) and never throw.

import { meetingContextEnabled } from '../shared/flags';
import { cleanText } from '../shared/meetingContext';
import {
  type ContentCommand,
  type ContextUpdate,
  MEETING_CONTEXT_KEY,
  type MeetingContextState,
  type Segment,
} from '../shared/messages';

const MAX_NAME = 100;
const MAX_TITLE = 300;
const MAX_PARTICIPANTS = 200;
const MAX_SPEAKERS = 10;
const MAX_INTERVALS = 5_000;
const MIN_INTERVAL_MS = 1_000;
/** How long the worker waits for the content script to answer `context_start` or `context_stop`. */
export const ANSWER_MS = 2_000;

/** A `context_update`, or the reply to `context_stop` (the same fields without `type`). */
export type Batch = Omit<ContextUpdate, 'type'>;

const isObject = (value: unknown): value is Record<string, unknown> => typeof value === 'object' && value !== null;
const optional = (value: unknown, valid: (v: unknown) => boolean) => value === undefined || valid(value);

function isBatch(value: unknown): value is Batch {
  if (!isObject(value)) return false;
  return (
    optional(value.meetingTitle, (v) => v === null || typeof v === 'string') &&
    optional(
      value.participants,
      (v) =>
        Array.isArray(v) && v.every((p) => isObject(p) && typeof p.name === 'string' && typeof p.self === 'boolean'),
    ) &&
    optional(
      value.speaking,
      (v) =>
        Array.isArray(v) &&
        v.every(
          (c) =>
            isObject(c) &&
            Number.isFinite(c.at) &&
            Array.isArray(c.names) &&
            c.names.every((n) => typeof n === 'string'),
        ),
    ) &&
    optional(value.gaveUp, (v) => typeof v === 'boolean')
  );
}

function segmentAt(segments: Segment[], wallMs: number): Segment | undefined {
  return segments.find((s) => wallMs >= s.wallStart && (s.wallEnd === null || wallMs < s.wallEnd));
}

/** The recording-clock ms for a wall-clock time, or null when it falls outside every segment (a pause). */
export function mapToRecordingMs(segments: Segment[], wallMs: number): number | null {
  const segment = segmentAt(segments, wallMs);
  return segment ? segment.offsetMs + (wallMs - segment.wallStart) : null;
}

function markTruncated(state: MeetingContextState, field: string): void {
  if (!state.truncated.includes(field)) state.truncated.push(field);
}

const sameNames = (a: string[], b: string[]) => a.length === b.length && a.every((n) => b.includes(n));

function cleanNames(names: string[]): string[] {
  return [...new Set(names.map((n) => cleanText(n, MAX_NAME)).filter(Boolean))];
}

function addInterval(state: MeetingContextState, interval: MeetingContextState['speaking'][number]): void {
  if (interval.toMs - interval.fromMs < MIN_INTERVAL_MS) return;
  const last = state.speaking.at(-1);
  if (last && interval.fromMs <= last.toMs && sameNames(last.names, interval.names)) {
    last.toMs = Math.max(last.toMs, interval.toMs);
  } else if (state.speaking.length >= MAX_INTERVALS) {
    markTruncated(state, 'speaking');
  } else {
    state.speaking.push(interval);
  }
}

/** Ends the open set at `wallMs`: its wall-clock span is mapped onto every segment it overlaps, so pause time yields nothing. */
function closeSpeaking(state: MeetingContextState, wallMs: number): void {
  const open = state.openSpeaking;
  state.openSpeaking = null;
  if (!open) return;
  for (const s of state.segments) {
    const from = Math.max(open.at, s.wallStart);
    const to = Math.min(wallMs, s.wallEnd ?? Number.POSITIVE_INFINITY);
    if (to > from) {
      addInterval(state, {
        fromMs: s.offsetMs + (from - s.wallStart),
        toMs: s.offsetMs + (to - s.wallStart),
        names: open.names,
      });
    }
  }
}

/**
 * For the hand-off: an interval still open ends at the last segment's end (Stop).
 * Not done at pause or stop, so a late batch with earlier changes can still end it sooner.
 */
export function settleContext(state: MeetingContextState): MeetingContextState {
  const wallEnd = state.segments.at(-1)?.wallEnd;
  if (!state.openSpeaking || typeof wallEnd !== 'number') return state;
  const next = structuredClone(state);
  closeSpeaking(next, wallEnd);
  return next;
}

function applySpeaking(state: MeetingContextState, change: { at: number; names: string[] }): void {
  let names = cleanNames(change.names);
  if (names.length > MAX_SPEAKERS) {
    names = names.slice(0, MAX_SPEAKERS);
    markTruncated(state, 'speaking');
  }
  const open = state.openSpeaking;
  if (open && sameNames(open.names, names)) return;
  closeSpeaking(state, change.at);
  state.openSpeaking = names.length > 0 ? { at: change.at, names } : null;
}

/** Unions by name (a later `self: true` sticks); returns how many usable names the batch carried. */
function mergeParticipants(state: MeetingContextState, participants: Batch['participants'] = []): number {
  let usable = 0;
  for (const p of participants) {
    const name = cleanText(p.name, MAX_NAME);
    if (!name) continue;
    usable++;
    const known = state.participants.find((q) => q.name === name);
    if (known) known.self ||= p.self;
    else if (state.participants.length < MAX_PARTICIPANTS) state.participants.push({ name, self: p.self });
    else markTruncated(state, 'participants');
  }
  return usable;
}

/** Merges one batch: roster union, title, speaking intervals on the recording clock, limits and the FR-5 state. A malformed batch changes nothing. */
export function applyBatch(state: MeetingContextState, batch: unknown): MeetingContextState {
  if (!isBatch(batch)) return state;
  const next = structuredClone(state);
  const title = typeof batch.meetingTitle === 'string' ? cleanText(batch.meetingTitle, MAX_TITLE) : '';
  if (title) next.meetingTitle = title;
  const named = mergeParticipants(next, batch.participants);
  for (const change of [...(batch.speaking ?? [])].sort((a, b) => a.at - b.at)) applySpeaking(next, change);
  if (title || named > 0) next.capture = 'captured';
  else if (batch.gaveUp && next.capture === 'reading') next.capture = 'unavailable';
  return next;
}

export async function readContext(): Promise<MeetingContextState | undefined> {
  if (!meetingContextEnabled()) return undefined;
  const stored = await chrome.storage.session.get(MEETING_CONTEXT_KEY);
  return stored[MEETING_CONTEXT_KEY] as MeetingContextState | undefined;
}

async function updateContext(change: (state: MeetingContextState) => MeetingContextState): Promise<void> {
  try {
    const state = await readContext();
    if (state) await chrome.storage.session.set({ [MEETING_CONTEXT_KEY]: change(state) });
  } catch {
    console.error('Could not update the meeting context');
  }
}

/** Closes the open segment (pause or stop); a closed one is left alone. */
export function closeSegment(at: number): Promise<void> {
  return updateContext((state) => {
    const last = state.segments.at(-1);
    if (!last || last.wallEnd !== null) return state;
    return { ...state, segments: [...state.segments.slice(0, -1), { ...last, wallEnd: at }] };
  });
}

export function openSegment(at: number, offsetMs: number): Promise<void> {
  return updateContext((state) => ({
    ...state,
    segments: [...state.segments, { wallStart: at, wallEnd: null, offsetMs }],
  }));
}

export function mergeBatch(batch: unknown): Promise<void> {
  return updateContext((state) => applyBatch(state, batch));
}

async function isMeet(): Promise<boolean> {
  try {
    return (await readContext())?.identity.platform === 'meet';
  } catch {
    return false;
  }
}

/** Sends `command` to the tab; rejects on no receiver or no answer within ANSWER_MS. */
async function ask(tabId: number, command: ContentCommand): Promise<unknown> {
  let timer: ReturnType<typeof setTimeout> | undefined;
  const timeout = new Promise<never>((_, reject) => {
    timer = setTimeout(() => reject(new Error('No answer from the Meet tab')), ANSWER_MS);
  });
  try {
    return await Promise.race([chrome.tabs.sendMessage(tabId, command), timeout]);
  } finally {
    clearTimeout(timer);
  }
}

/** Tells a Meet tab's content script to start reading. A failure only sets the unavailable state with the reload hint. */
export async function startContext(tabId: number, startedAt: number): Promise<void> {
  if (!(await isMeet())) return;
  try {
    await ask(tabId, { type: 'context_start', startedAt });
  } catch {
    await updateContext((state) => ({ ...state, capture: 'unavailable', reloadHint: true }));
  }
}

/** Asks the Meet tab for its last batch and merges it; no answer within ANSWER_MS hands off what was merged. */
export async function collectFinalBatch(tabId: number): Promise<void> {
  if (!(await isMeet())) return;
  let reply: unknown;
  try {
    reply = await ask(tabId, { type: 'context_stop' });
  } catch {
    return;
  }
  await mergeBatch(reply);
}
