import { useCallback, useEffect, useRef, useState } from 'react';
import { type Note, noteGet } from '../../api';
import { type Attempt, attempt } from '../../recording/attempt';
import { problemText } from './format';

export const POLL_MS = 3000;
/** How long the view keeps polling after a rerun it started, to follow the note to `processing`. */
export const RERUN_FOLLOW_MS = 10_000;

export type NoteState = { kind: 'loading' } | { kind: 'error'; message: string } | { kind: 'ready'; note: Note };

export type UseNote = {
  state: NoteState;
  /** Fetches the note again; the reply is applied unless something newer was applied meanwhile. */
  reload: () => Promise<Attempt<{ note: Note }>>;
  /** Applies a note another command replied with (it is newer than any poll in flight). */
  apply: (note: Note) => void;
  /** Starts the after-rerun polling window. */
  markRerun: () => void;
};

/**
 * Loads note `id` and polls it every 3 s while the window is visible and the note is processing or
 * a rerun from this view is under 10 s old. `skipPoll` is asked before each tick (a focused field
 * with unsaved text). The latest request wins over older ones.
 */
export function useNote(id: number, skipPoll: () => boolean): UseNote {
  const [state, setState] = useState<NoteState>({ kind: 'loading' });
  // Bumped by each rerun, so a second rerun restarts the 10 s window.
  const [reruns, setReruns] = useState(0);
  const [following, setFollowing] = useState(false);
  const requests = useRef(0);
  const applied = useRef(0);
  const active = useRef(true);
  const skip = useRef(skipPoll);
  skip.current = skipPoll;

  const apply = useCallback((note: Note) => {
    applied.current = ++requests.current;
    setState({ kind: 'ready', note });
  }, []);

  const reload = useCallback(async () => {
    const request = ++requests.current;
    const result = await attempt(() => noteGet({ id }));
    if (!active.current || request < applied.current) return result;
    applied.current = request;
    if (result.ok) setState({ kind: 'ready', note: result.reply.note });
    else
      setState((current) =>
        // A failed refresh keeps the note on screen; only a gone note or a first load replaces it.
        current.kind === 'ready' && result.code !== 'unknown_note'
          ? current
          : { kind: 'error', message: problemText(result.code, result.text) },
      );
    return result;
  }, [id]);

  useEffect(() => {
    active.current = true;
    setState({ kind: 'loading' });
    void reload();
    return () => {
      active.current = false;
    };
  }, [reload]);

  const markRerun = useCallback(() => setReruns((n) => n + 1), []);

  useEffect(() => {
    if (reruns === 0) return;
    setFollowing(true);
    const timer = setTimeout(() => setFollowing(false), RERUN_FOLLOW_MS);
    return () => clearTimeout(timer);
  }, [reruns]);

  const processing = state.kind === 'ready' && state.note.status === 'processing';
  const polling = processing || following;

  useEffect(() => {
    if (!polling) return;
    const timer = setInterval(() => {
      if (document.visibilityState === 'visible' && !skip.current()) void reload();
    }, POLL_MS);
    return () => clearInterval(timer);
  }, [polling, reload]);

  return { state, reload, apply, markRerun };
}
