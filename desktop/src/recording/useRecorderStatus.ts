import { useCallback, useEffect, useRef, useState } from 'react';
import { type RecorderState, type RecorderStatus, recorderStatus } from '../api';
import { attempt } from './attempt';

export const POLL_MS = 1000;

export type RecorderView = {
  /** Null until the first reply. */
  status: RecorderStatus | null;
  /** Why the last poll failed; the previous status is kept. */
  error: string | null;
  refresh: () => void;
  /** Applies the `state` a Pause/Resume/Stop/Retry reply returned, without waiting for the next poll. */
  setState: (state: RecorderState) => void;
};

/** `recorder_status`, polled every second while mounted; the latest request's reply wins. */
export function useRecorderStatus(): RecorderView {
  const [status, setStatus] = useState<RecorderStatus | null>(null);
  const [error, setError] = useState<string | null>(null);
  const requests = useRef(0);
  const applied = useRef(0);
  const active = useRef(true);

  const refresh = useCallback(() => {
    const id = ++requests.current;
    attempt(recorderStatus).then((result) => {
      if (!active.current || id < applied.current) return;
      applied.current = id;
      if (result.ok) {
        const { ok: _, ...next } = result.reply;
        setStatus(next);
        setError(null);
      } else {
        setError(result.text);
      }
    });
  }, []);

  const setState = useCallback((state: RecorderState) => {
    // A poll sent before the command could still answer with the old state: ignore every one in flight.
    applied.current = ++requests.current;
    setStatus((previous) => (previous ? { ...previous, state } : previous));
  }, []);

  useEffect(() => {
    active.current = true;
    refresh();
    const timer = setInterval(refresh, POLL_MS);
    return () => {
      active.current = false;
      clearInterval(timer);
    };
  }, [refresh]);

  return { status, error, refresh, setState };
}
