// Pure recording lifecycle: no Chrome APIs and no clock reads, every time is passed in (epoch ms).

export const RECORDING_CAP_MS = 3 * 60 * 60 * 1000;

export type StopReason = 'user' | 'cap' | 'tab_closed' | 'capture_ended';

export type RecordingState =
  | { phase: 'idle'; lastStop: StopReason | null }
  | { phase: 'recording'; startedAt: number; recordedMs: number; resumedAt: number }
  | { phase: 'paused'; startedAt: number; recordedMs: number }
  | { phase: 'saving'; startedAt: number; recordedMs: number; reason: StopReason };

export type RecordingEvent =
  | { type: 'start'; at: number }
  | { type: 'pause'; at: number }
  | { type: 'resume'; at: number }
  | { type: 'stop'; at: number; reason: StopReason }
  | { type: 'saved' };

export const initialRecordingState: RecordingState = { phase: 'idle', lastStop: null };

/** Recorded time so far, excluding paused time, never above the cap. */
export function elapsedMs(state: RecordingState, now: number): number {
  switch (state.phase) {
    case 'idle':
      return 0;
    case 'recording':
      return Math.min(RECORDING_CAP_MS, state.recordedMs + Math.max(0, now - state.resumedAt));
    default:
      return state.recordedMs;
  }
}

export function remainingMs(state: RecordingState, now: number): number {
  return RECORDING_CAP_MS - elapsedMs(state, now);
}

/** Returns `state` itself (same reference) for an event the lifecycle does not allow. */
export function transition(state: RecordingState, event: RecordingEvent): RecordingState {
  switch (event.type) {
    case 'start':
      return state.phase === 'idle'
        ? { phase: 'recording', startedAt: event.at, recordedMs: 0, resumedAt: event.at }
        : state;
    case 'pause':
      return state.phase === 'recording'
        ? { phase: 'paused', startedAt: state.startedAt, recordedMs: elapsedMs(state, event.at) }
        : state;
    case 'resume':
      return state.phase === 'paused'
        ? { phase: 'recording', startedAt: state.startedAt, recordedMs: state.recordedMs, resumedAt: event.at }
        : state;
    case 'stop':
      // Paused time cannot reach the cap, so a `cap` stop is only valid while recording.
      if (state.phase === 'recording' || (state.phase === 'paused' && event.reason !== 'cap')) {
        return {
          phase: 'saving',
          startedAt: state.startedAt,
          recordedMs: elapsedMs(state, event.at),
          reason: event.reason,
        };
      }
      return state;
    case 'saved':
      return state.phase === 'saving' ? { phase: 'idle', lastStop: state.reason } : state;
  }
}
