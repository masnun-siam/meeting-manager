import { useCallback, useEffect, useRef, useState } from 'react';
import { type NotesListRow, notesList } from '../../api';

export type NotesView =
  | { kind: 'loading' }
  | { kind: 'list'; notes: NotesListRow[]; query: string | null; problem: string | null }
  | { kind: 'unavailable'; message: string }
  | { kind: 'failed'; message: string };

type Result =
  | { kind: 'ok'; notes: NotesListRow[] }
  | { kind: 'unavailable'; message: string }
  | { kind: 'failed'; message: string };

export const POLL_MS = 3000;

export const errorMessage = (error: unknown) => (error instanceof Error ? error.message : String(error));

async function fetchNotes(query: string | null): Promise<Result> {
  try {
    const reply = await notesList({ query });
    if (reply.ok) return { kind: 'ok', notes: reply.notes };
    return { kind: reply.code === 'store_unavailable' ? 'unavailable' : 'failed', message: reply.message };
  } catch (error) {
    return { kind: 'failed', message: errorMessage(error) };
  }
}

function nextView(prev: NotesView, result: Result, query: string | null): NotesView {
  switch (result.kind) {
    case 'ok':
      return { kind: 'list', notes: result.notes, query, problem: null };
    case 'unavailable':
      return result;
    case 'failed':
      return prev.kind === 'list' ? { ...prev, problem: result.message } : result;
  }
}

const visible = () => document.visibilityState === 'visible';

/**
 * The notes matching `query` (`null`: all), refetched at once when the query changes, every 3 s while the window
 * is visible and when it becomes visible again. The latest request wins, and nothing asked before the current
 * query was set is applied.
 */
export function useNotes(query: string | null): { view: NotesView; refresh: () => void } {
  const [view, setView] = useState<NotesView>({ kind: 'loading' });
  const requests = useRef(0);
  const applied = useRef(0);
  const firstOfQuery = useRef(0);
  const current = useRef(query);
  const active = useRef(true);

  const refresh = useCallback(() => {
    const id = ++requests.current;
    const asked = current.current;
    fetchNotes(asked).then((result) => {
      if (!active.current || id < applied.current || id < firstOfQuery.current) return;
      applied.current = id;
      setView((prev) => nextView(prev, result, asked));
    });
  }, []);

  useEffect(() => {
    active.current = true;
    const timer = setInterval(() => visible() && refresh(), POLL_MS);
    const onVisibility = () => visible() && refresh();
    document.addEventListener('visibilitychange', onVisibility);
    return () => {
      active.current = false;
      clearInterval(timer);
      document.removeEventListener('visibilitychange', onVisibility);
    };
  }, [refresh]);

  useEffect(() => {
    current.current = query;
    firstOfQuery.current = requests.current + 1;
    refresh();
  }, [query, refresh]);

  return { view, refresh };
}
