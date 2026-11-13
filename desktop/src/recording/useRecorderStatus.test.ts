import { act, renderHook, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { Ok, RecorderStatus } from '../api';
import * as api from '../api';
import { deferred, statusReply } from './test-status';
import { POLL_MS, useRecorderStatus } from './useRecorderStatus';

vi.mock('../api', async () => (await import('../api.mock')).mockApi());

const recorderStatus = vi.mocked(api.recorderStatus);

beforeEach(() => {
  vi.useFakeTimers({ shouldAdvanceTime: true });
  recorderStatus.mockReset().mockResolvedValue(statusReply());
});

afterEach(() => {
  vi.useRealTimers();
});

describe('useRecorderStatus', () => {
  it('polls every second', async () => {
    recorderStatus.mockResolvedValueOnce(statusReply({ state: 'recording', elapsed_s: 4 }));
    recorderStatus.mockResolvedValueOnce(statusReply({ state: 'recording', elapsed_s: 5 }));
    const { result } = renderHook(() => useRecorderStatus());

    await waitFor(() => expect(result.current.status?.elapsed_s).toBe(4));
    await act(() => vi.advanceTimersByTimeAsync(POLL_MS));
    expect(result.current.status?.elapsed_s).toBe(5);
  });

  it('ignores an older reply that arrives after a newer one', async () => {
    const slow = deferred<Ok<RecorderStatus>>();
    recorderStatus.mockReturnValueOnce(slow.promise);
    recorderStatus.mockResolvedValueOnce(statusReply({ state: 'recording', elapsed_s: 9 }));
    const { result } = renderHook(() => useRecorderStatus());

    await act(() => vi.advanceTimersByTimeAsync(POLL_MS));
    expect(result.current.status?.elapsed_s).toBe(9);

    await act(async () => slow.resolve(statusReply({ state: 'idle', elapsed_s: 1 })));
    expect(result.current.status).toMatchObject({ state: 'recording', elapsed_s: 9 });
  });

  it('applies a command state at once and ignores polls already in flight', async () => {
    const { result } = renderHook(() => useRecorderStatus());
    await waitFor(() => expect(result.current.status?.state).toBe('idle'));

    const inFlight = deferred<Ok<RecorderStatus>>();
    recorderStatus.mockReturnValueOnce(inFlight.promise);
    await act(() => vi.advanceTimersByTimeAsync(POLL_MS));
    act(() => result.current.setState('paused'));
    expect(result.current.status?.state).toBe('paused');

    await act(async () => inFlight.resolve(statusReply({ state: 'recording' })));
    expect(result.current.status?.state).toBe('paused');
  });

  it('keeps the last status and exposes the error when a poll fails', async () => {
    recorderStatus.mockResolvedValueOnce(statusReply({ state: 'recording', elapsed_s: 3 }));
    recorderStatus.mockRejectedValueOnce(new Error('ipc closed'));
    const { result } = renderHook(() => useRecorderStatus());
    await waitFor(() => expect(result.current.status?.elapsed_s).toBe(3));

    await act(() => vi.advanceTimersByTimeAsync(POLL_MS));
    expect(result.current.status?.elapsed_s).toBe(3);
    expect(result.current.error).toBe('Could not reach the Meeting app: ipc closed');

    await act(() => vi.advanceTimersByTimeAsync(POLL_MS));
    expect(result.current.error).toBeNull();
  });

  it('stops polling on unmount', async () => {
    const { unmount } = renderHook(() => useRecorderStatus());
    await act(() => vi.advanceTimersByTimeAsync(POLL_MS));
    unmount();
    const calls = recorderStatus.mock.calls.length;

    await act(() => vi.advanceTimersByTimeAsync(POLL_MS * 5));
    expect(recorderStatus).toHaveBeenCalledTimes(calls);
  });
});
