import { listen } from '@tauri-apps/api/event';
import { useCallback, useEffect, useRef, useState } from 'react';
import { type CalendarDay, calendarEvents } from '../api';
import { attempt } from '../recording/attempt';

export const CHANGED_EVENT = 'calendar-changed';
/** EventKit posts change notifications in bursts; one refetch per burst is enough. */
export const CHANGE_DEBOUNCE_MS = 300;

export type UpcomingView =
  | { kind: 'loading' }
  | { kind: 'denied' }
  | { kind: 'error'; message: string }
  // `error` is a failed refetch: the last good list stays on screen.
  | { kind: 'ready'; days: CalendarDay[]; error: string | null };

/**
 * `calendar_events` on mount, when the window gains focus and when Calendar reports a change (no timer);
 * the latest request's reply wins.
 */
export function useUpcoming(): UpcomingView {
  const [view, setView] = useState<UpcomingView>({ kind: 'loading' });
  const requests = useRef(0);
  const applied = useRef(0);
  const active = useRef(true);

  const refresh = useCallback(() => {
    const id = ++requests.current;
    attempt(calendarEvents).then((result) => {
      if (!active.current || id < applied.current) return;
      applied.current = id;
      setView((previous) => {
        if (result.ok) return { kind: 'ready', days: result.reply.days, error: null };
        if (result.code === 'calendar_denied') return { kind: 'denied' };
        if (previous.kind === 'ready') return { ...previous, error: result.text };
        return { kind: 'error', message: result.text };
      });
    });
  }, []);

  useEffect(() => {
    active.current = true;
    // Per effect run, not the shared ref: under StrictMode a first run's `listen` can resolve after
    // the second run has set the ref again.
    let cancelled = false;
    let unlisten: (() => void) | null = null;
    let debounce: ReturnType<typeof setTimeout> | undefined;
    const onChange = () => {
      clearTimeout(debounce);
      debounce = setTimeout(refresh, CHANGE_DEBOUNCE_MS);
    };

    refresh();
    window.addEventListener('focus', refresh);
    listen(CHANGED_EVENT, onChange).then(
      (stop) => {
        // Unmounted before `listen` resolved: stop listening at once.
        if (!cancelled) unlisten = stop;
        else stop();
      },
      // Outside the Tauri webview there is no event bus; focus still refreshes.
      () => {},
    );

    return () => {
      active.current = false;
      cancelled = true;
      window.removeEventListener('focus', refresh);
      clearTimeout(debounce);
      unlisten?.();
    };
  }, [refresh]);

  return view;
}
