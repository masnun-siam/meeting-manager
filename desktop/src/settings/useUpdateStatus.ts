import { useCallback, useEffect, useRef, useState } from 'react';
import { type UpdateState, type UpdateStatus, updateStatus } from '../api';
import { attempt } from '../recording/attempt';

export const UPDATE_POLL_MS = 1000;

const BUSY: readonly UpdateState[] = ['checking', 'downloading', 'ready', 'installing'];

export const isBusy = (state: UpdateState | undefined) => state !== undefined && BUSY.includes(state);

export type UpdateView = {
  /** Null until the first reply. */
  status: UpdateStatus | null;
  /** Why the last read failed; the previous status is kept. */
  problem: string | null;
  /** Reads the status now; resolves to this read's status, or null when it failed or a newer read won. */
  reload: () => Promise<UpdateStatus | null>;
};

/** `update_status`, read on mount and every second while a check, download or install runs; the latest reply wins. */
export function useUpdateStatus(): UpdateView {
  const [status, setStatus] = useState<UpdateStatus | null>(null);
  const [problem, setProblem] = useState<string | null>(null);
  const active = useRef(true);
  const requests = useRef(0);
  const applied = useRef(0);

  const reload = useCallback(async () => {
    const id = ++requests.current;
    const reply = await attempt(updateStatus);
    if (!active.current || id < applied.current) return null;
    applied.current = id;
    if (!reply.ok) {
      setProblem(reply.text);
      return null;
    }
    const { ok: _, ...next } = reply.reply;
    setStatus(next);
    setProblem(null);
    return next;
  }, []);

  useEffect(() => {
    active.current = true;
    void reload();
    return () => {
      active.current = false;
    };
  }, [reload]);

  const busy = isBusy(status?.state);
  useEffect(() => {
    if (!busy) return;
    const timer = setInterval(reload, UPDATE_POLL_MS);
    return () => clearInterval(timer);
  }, [busy, reload]);

  return { status, problem, reload };
}
