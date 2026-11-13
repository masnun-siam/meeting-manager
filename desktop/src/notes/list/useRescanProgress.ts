import { useCallback, useEffect, useRef, useState } from 'react';
import { type RescanStatus, rescanStatus } from '../../api';

export const RESCAN_POLL_MS = 1000;

/**
 * The backfill's progress, read on mount and every second while it runs; the latest reply wins. `onFinished` is
 * called once each time a run is seen to stop. `watch()` is for a run just started here: it counts as running until a
 * status says otherwise, so a run that ends before the first status still calls `onFinished`.
 */
export function useRescanProgress(onFinished: () => void): { progress: RescanStatus | null; watch: () => void } {
  const [progress, setProgress] = useState<RescanStatus | null>(null);
  const finished = useRef(onFinished);
  const wasRunning = useRef(false);
  const active = useRef(true);
  const requests = useRef(0);
  const applied = useRef(0);

  useEffect(() => {
    finished.current = onFinished;
  }, [onFinished]);

  const check = useCallback(async () => {
    const id = ++requests.current;
    let reply: Awaited<ReturnType<typeof rescanStatus>>;
    try {
      reply = await rescanStatus();
    } catch {
      return;
    }
    if (!active.current || !reply.ok || id < applied.current) return;
    applied.current = id;
    const { running, done, total } = reply;
    if (wasRunning.current && !running) finished.current();
    wasRunning.current = running;
    setProgress({ running, done, total });
  }, []);

  useEffect(() => {
    active.current = true;
    check();
    return () => {
      active.current = false;
    };
  }, [check]);

  const running = progress?.running === true;
  useEffect(() => {
    if (!running) return;
    const timer = setInterval(check, RESCAN_POLL_MS);
    return () => clearInterval(timer);
  }, [running, check]);

  const watch = useCallback(() => {
    wasRunning.current = true;
    check();
  }, [check]);

  return { progress, watch };
}
