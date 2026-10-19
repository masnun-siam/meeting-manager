import { useCallback, useEffect, useRef, useState } from 'react';
import type { JobSummary } from '../shared/native';
import { sendNative } from '../shared/nativeClient';

export type JobsView = { kind: 'loading' } | { kind: 'ready'; jobs: JobSummary[] } | { kind: 'error'; message: string };

const POLL_MS = 3000;

async function fetchView(): Promise<JobsView> {
  try {
    const reply = await sendNative({ type: 'list_jobs', limit: 10 });
    return reply.ok ? { kind: 'ready', jobs: reply.jobs } : { kind: 'error', message: reply.message };
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    return { kind: 'error', message: `Could not reach the meeting tool: ${message}` };
  }
}

/** The 10 most recent jobs, polled every 3 s while mounted; the latest reply wins. */
export function useJobs(): { view: JobsView; refresh: () => void } {
  const [view, setView] = useState<JobsView>({ kind: 'loading' });
  const requests = useRef(0);
  const applied = useRef(0);
  const active = useRef(true);

  const refresh = useCallback(() => {
    const id = ++requests.current;
    fetchView().then((next) => {
      if (!active.current || id < applied.current) return;
      applied.current = id;
      setView(next);
    });
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

  return { view, refresh };
}
