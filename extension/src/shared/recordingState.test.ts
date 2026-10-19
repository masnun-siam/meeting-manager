import { describe, expect, it } from 'vitest';
import {
  elapsedMs,
  initialRecordingState,
  RECORDING_CAP_MS,
  type RecordingEvent,
  type RecordingState,
  remainingMs,
  type StopReason,
  transition,
} from './recordingState';

function run(events: RecordingEvent[], from: RecordingState = initialRecordingState): RecordingState {
  return events.reduce(transition, from);
}

const recording = run([{ type: 'start', at: 0 }]);
const paused = run([{ type: 'pause', at: 1000 }], recording);

describe('recordingState', () => {
  it('start moves to recording with a full cap remaining', () => {
    expect(recording.phase).toBe('recording');
    expect(elapsedMs(recording, 0)).toBe(0);
    expect(remainingMs(recording, 0)).toBe(RECORDING_CAP_MS);
  });

  it('excludes paused time from elapsed (FR-2)', () => {
    const state = run(
      [
        { type: 'pause', at: 120_000 },
        { type: 'resume', at: 180_000 },
      ],
      recording,
    );
    expect(elapsedMs(state, 300_000)).toBe(240_000);
  });

  it('freezes recorded time at stop (FR-3)', () => {
    const state = run(
      [
        { type: 'pause', at: 60_000 },
        { type: 'resume', at: 120_000 },
        { type: 'stop', at: 180_000, reason: 'user' },
      ],
      recording,
    );
    expect(state).toMatchObject({ phase: 'saving', recordedMs: 120_000, reason: 'user' });
    expect(elapsedMs(state, 999_999)).toBe(120_000);
  });

  it('never exceeds the cap, even for a late cap stop', () => {
    expect(elapsedMs(recording, RECORDING_CAP_MS + 5000)).toBe(RECORDING_CAP_MS);
    expect(remainingMs(recording, RECORDING_CAP_MS + 5000)).toBe(0);
    const state = transition(recording, { type: 'stop', at: RECORDING_CAP_MS + 200, reason: 'cap' });
    expect(state).toMatchObject({ phase: 'saving', recordedMs: RECORDING_CAP_MS });
  });

  it.each<StopReason>(['user', 'cap', 'tab_closed', 'capture_ended'])(
    'stops from recording with %s, then idles',
    (reason) => {
      const saving = transition(recording, { type: 'stop', at: 5000, reason });
      expect(saving).toMatchObject({ phase: 'saving', reason, startedAt: 0 });
      expect(transition(saving, { type: 'saved' })).toEqual({ phase: 'idle', lastStop: reason });
    },
  );

  it.each<StopReason>(['user', 'tab_closed', 'capture_ended'])('stops from paused with %s', (reason) => {
    const saving = transition(paused, { type: 'stop', at: 5000, reason });
    expect(saving).toMatchObject({ phase: 'saving', reason, recordedMs: 1000 });
  });

  it('returns the same reference for events the lifecycle does not allow', () => {
    const saving = transition(recording, { type: 'stop', at: 1, reason: 'user' });
    const invalid: [RecordingState, RecordingEvent][] = [
      [initialRecordingState, { type: 'pause', at: 1 }],
      [initialRecordingState, { type: 'resume', at: 1 }],
      [initialRecordingState, { type: 'stop', at: 1, reason: 'user' }],
      [initialRecordingState, { type: 'saved' }],
      [recording, { type: 'start', at: 1 }],
      [recording, { type: 'resume', at: 1 }],
      [recording, { type: 'saved' }],
      [paused, { type: 'start', at: 1 }],
      [paused, { type: 'pause', at: 1 }],
      [paused, { type: 'stop', at: 1, reason: 'cap' }],
      [saving, { type: 'stop', at: 2, reason: 'tab_closed' }],
      [saving, { type: 'start', at: 2 }],
    ];
    for (const [state, event] of invalid) {
      expect(transition(state, event)).toBe(state);
    }
  });

  it('does not go negative when the clock moves back', () => {
    expect(elapsedMs(recording, -5000)).toBe(0);
    const later = run([{ type: 'start', at: 10_000 }]);
    expect(elapsedMs(later, 4000)).toBe(0);
    expect(transition(later, { type: 'pause', at: 4000 })).toMatchObject({ recordedMs: 0 });
  });
});
