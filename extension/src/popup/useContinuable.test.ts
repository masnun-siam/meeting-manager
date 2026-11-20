import { act, cleanup, renderHook, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { LastRecording } from '../shared/lastRecording';
import type { RecorderStatus } from '../shared/messages';
import { initialRecordingState, type RecordingState } from '../shared/recordingState';
import { RECHECK_MS, useContinuable } from './useContinuable';

const flag = vi.hoisted(() => ({ on: true }));
vi.mock('../shared/flags', () => ({ continueRecordingEnabled: () => flag.on }));

const T0 = new Date('2026-01-01T10:00:00Z').getTime();
const MIN = 60_000;

function last(extra: Partial<LastRecording> = {}): LastRecording {
  return {
    download_id: 7,
    path: '/Users/me/Downloads/Meetings/meeting-2026-01-01_0959/recording.webm',
    folder: 'meeting-2026-01-01_0959',
    ext: 'webm',
    recorded_ms: MIN,
    language: 'en',
    stopped_at: T0 - MIN,
    stop_reason: 'tab_closed',
    parts: 1,
    ...extra,
  };
}

function status(state: RecordingState = initialRecordingState, extra: Partial<RecorderStatus> = {}): RecorderStatus {
  return { state, language: 'bn', micIncluded: null, notice: null, tabId: null, continuing: null, ...extra };
}

// One object, as useRecorderStatus keeps it until storage changes: a new status re-runs the file check.
const IDLE = status();

let local: Record<string, unknown>;
let exists: boolean;
let listeners: ((changes: Record<string, { newValue?: unknown }>) => void)[];
const localGet = vi.fn();
const search = vi.fn();

beforeEach(() => {
  vi.useFakeTimers({ shouldAdvanceTime: true });
  vi.setSystemTime(T0);
  flag.on = true;
  local = {};
  exists = true;
  listeners = [];
  localGet.mockReset().mockImplementation(async () => local);
  search.mockReset().mockImplementation(async () => (exists ? [{ id: 7 }] : []));
  vi.stubGlobal('chrome', {
    storage: {
      local: {
        get: localGet,
        onChanged: {
          addListener: vi.fn((fn) => {
            listeners.push(fn);
          }),
          removeListener: vi.fn(),
        },
      },
    },
    downloads: { search },
  });
});

afterEach(() => {
  cleanup();
  vi.useRealTimers();
  vi.unstubAllGlobals();
});

function changeLocal(value: unknown) {
  local = { last_recording: value };
  for (const fn of listeners) fn({ last_recording: { newValue: value } });
}

describe('useContinuable', () => {
  it('offers the stored recording when the rule allows it', async () => {
    local = { last_recording: last() };
    const { result } = renderHook(() => useContinuable(IDLE));

    await waitFor(() => expect(result.current?.last).toEqual(last()));
    expect(result.current?.now).toBeGreaterThanOrEqual(T0);
    expect(localGet).toHaveBeenCalledTimes(1);
    expect(search).toHaveBeenCalledTimes(1);
    expect(search).toHaveBeenCalledWith({ id: 7, exists: true });
  });

  it('treats a null status (none written yet) as idle', async () => {
    local = { last_recording: last() };
    const { result } = renderHook(() => useContinuable(null));

    await waitFor(() => expect(result.current).not.toBeNull());
  });

  it('reads nothing with the flag off', async () => {
    flag.on = false;
    local = { last_recording: last() };
    const { result } = renderHook(() => useContinuable(IDLE));

    await act(async () => {
      vi.advanceTimersByTime(RECHECK_MS);
    });
    expect(result.current).toBeNull();
    expect(localGet).not.toHaveBeenCalled();
    expect(search).not.toHaveBeenCalled();
    expect(listeners).toHaveLength(0);
  });

  it.each<[string, () => void]>([
    ['nothing is stored', () => {}],
    ['it stopped 2 h 1 min ago', () => (local = { last_recording: last({ stopped_at: T0 - 121 * MIN }) })],
    [
      'the download no longer exists',
      () => {
        local = { last_recording: last() };
        exists = false;
      },
    ],
    ['it reached 3 h', () => (local = { last_recording: last({ recorded_ms: 3 * 60 * MIN }) })],
  ])('offers nothing when %s', async (_, arrange) => {
    arrange();
    const { result } = renderHook(() => useContinuable(IDLE));

    await waitFor(() => expect(localGet).toHaveBeenCalled());
    await act(async () => {});
    expect(result.current).toBeNull();
  });

  it('treats a failed downloads search as missing', async () => {
    local = { last_recording: last() };
    search.mockRejectedValue(new Error('downloads unavailable'));
    const { result } = renderHook(() => useContinuable(IDLE));

    await waitFor(() => expect(search).toHaveBeenCalled());
    await act(async () => {});
    expect(result.current).toBeNull();
  });

  it.each<RecordingState>([
    { phase: 'recording', startedAt: T0, recordedMs: 0, resumedAt: T0 },
    { phase: 'paused', startedAt: T0, recordedMs: 0 },
    { phase: 'saving', startedAt: T0, recordedMs: 0, reason: 'user' },
  ])('offers nothing while $phase, without a downloads search', async (state) => {
    local = { last_recording: last() };
    const s = status(state);
    const { result } = renderHook(() => useContinuable(s));

    await waitFor(() => expect(localGet).toHaveBeenCalled());
    await act(async () => {});
    expect(result.current).toBeNull();
    expect(search).not.toHaveBeenCalled();
  });

  it('stops offering when the 2 h window runs out while mounted', async () => {
    local = { last_recording: last({ stopped_at: T0 - 119 * MIN }) };
    const { result } = renderHook(() => useContinuable(IDLE));
    await waitFor(() => expect(result.current).not.toBeNull());

    await act(async () => {
      vi.advanceTimersByTime(MIN - RECHECK_MS);
    });
    expect(result.current).not.toBeNull();
    await act(async () => {
      vi.advanceTimersByTime(RECHECK_MS + RECHECK_MS);
    });
    expect(result.current).toBeNull();
  });

  it('offers a recording saved while mounted', async () => {
    const { result } = renderHook(() => useContinuable(IDLE));
    await waitFor(() => expect(listeners).toHaveLength(1));
    expect(result.current).toBeNull();

    act(() => changeLocal(last({ stopped_at: T0 })));

    await waitFor(() => expect(result.current?.last.stopped_at).toBe(T0));
  });

  it('ignores other keys changing in local storage', async () => {
    const { result } = renderHook(() => useContinuable(IDLE));
    await waitFor(() => expect(listeners).toHaveLength(1));

    act(() => {
      for (const fn of listeners) fn({ language: { newValue: 'en' } });
    });
    expect(localGet).toHaveBeenCalledTimes(1);
    expect(result.current).toBeNull();
  });

  it('re-checks the file when the status changes, so a failed Continue hides the offer', async () => {
    local = { last_recording: last() };
    const { result, rerender } = renderHook(({ s }) => useContinuable(s), { initialProps: { s: IDLE } });
    await waitFor(() => expect(result.current).not.toBeNull());

    exists = false;
    rerender({ s: status(initialRecordingState, { notice: { kind: 'error', message: 'Nothing to continue: …' } }) });

    await waitFor(() => expect(result.current).toBeNull());
    expect(search).toHaveBeenCalledTimes(2);
  });

  it('removes its storage listener on unmount', async () => {
    const { unmount } = renderHook(() => useContinuable(IDLE));
    await waitFor(() => expect(listeners).toHaveLength(1));

    unmount();

    expect(chrome.storage.local.onChanged.removeListener).toHaveBeenCalledWith(listeners[0]);
  });
});
