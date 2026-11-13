import { useCallback, useEffect, useRef, useState } from 'react';
import { type CalendarInfo, calendarStatus, calendars, setHiddenCalendars } from '../api';
import { attempt } from '../recording/attempt';
import styles from './Settings.module.css';

export const CALENDAR_DENIED = 'Allow Meeting in System Settings › Privacy & Security › Calendars';

type View =
  | { kind: 'loading' }
  | { kind: 'denied' }
  | { kind: 'not_asked' }
  | { kind: 'failed'; message: string }
  | { kind: 'list'; calendars: CalendarInfo[] };

const hiddenOf = (list: CalendarInfo[]) => new Set(list.filter((c) => c.hidden).map((c) => c.id));

/** FR-20: one checkbox per calendar; a checked box means its events show in Upcoming. */
export function CalendarsSection() {
  const [view, setView] = useState<View>({ kind: 'loading' });
  const [hidden, setHidden] = useState<Set<string>>(new Set());
  const [problem, setProblem] = useState<string | null>(null);
  const active = useRef(true);
  // The set the UI shows, ahead of React's render, so two quick toggles both count.
  const wanted = useRef<Set<string>>(new Set());
  // The set the app last confirmed; a failed latest change goes back to it.
  const confirmed = useRef<Set<string>>(new Set());
  const queue = useRef<Promise<void>>(Promise.resolve());
  const latest = useRef(0);

  const show = useCallback((next: Set<string>) => {
    wanted.current = next;
    setHidden(next);
  }, []);

  const apply = useCallback(
    (list: CalendarInfo[]) => {
      confirmed.current = hiddenOf(list);
      show(confirmed.current);
      setView({ kind: 'list', calendars: list });
    },
    [show],
  );

  useEffect(() => {
    active.current = true;
    (async () => {
      const status = await attempt(calendarStatus);
      if (!active.current) return;
      if (!status.ok) return setView({ kind: 'failed', message: status.text });
      if (status.reply.access !== 'granted') return setView({ kind: status.reply.access });
      const reply = await attempt(calendars);
      if (!active.current) return;
      if (reply.ok) apply(reply.reply.calendars);
      else setView({ kind: 'failed', message: reply.text });
    })();
    return () => {
      active.current = false;
    };
  }, [apply]);

  function toggle(id: string) {
    const next = new Set(wanted.current);
    if (next.has(id)) next.delete(id);
    else next.add(id);
    show(next);
    setProblem(null);
    const request = ++latest.current;
    queue.current = queue.current.then(async () => {
      const reply = await attempt(() => setHiddenCalendars({ ids: [...next] }));
      if (!active.current) return;
      if (request !== latest.current) {
        // A newer change is queued; remember what the app saved so a failure of that one reverts to it.
        if (reply.ok) confirmed.current = hiddenOf(reply.reply.calendars);
        return;
      }
      if (reply.ok) {
        apply(reply.reply.calendars);
      } else {
        show(confirmed.current);
        setProblem(reply.text);
      }
    });
  }

  return (
    <section aria-labelledby="settings-calendars" className={styles.section}>
      <h3 id="settings-calendars">Calendars</h3>
      {view.kind === 'loading' && <p>Loading calendars…</p>}
      {view.kind === 'denied' && <p>{CALENDAR_DENIED}</p>}
      {view.kind === 'not_asked' && <p>Calendars appear here once you allow access in Upcoming.</p>}
      {view.kind === 'failed' && (
        <p role="alert" className={styles.problem}>
          {view.message}
        </p>
      )}
      {view.kind === 'list' && view.calendars.length === 0 && <p>No calendars found</p>}
      {view.kind === 'list' && view.calendars.length > 0 && (
        <ul aria-label="Calendars" className={styles.plain}>
          {view.calendars.map((calendar) => (
            <li key={calendar.id}>
              <label className={styles.calendar}>
                <input type="checkbox" checked={!hidden.has(calendar.id)} onChange={() => toggle(calendar.id)} />{' '}
                {calendar.title} <span className={styles.muted}>{calendar.account}</span>
              </label>
            </li>
          ))}
        </ul>
      )}
      <p role="status" className={styles.problem}>
        {problem}
      </p>
    </section>
  );
}
