import { useEffect, useRef, useState } from 'react';
import type { CalendarDay } from '../api';
import { CalendarIcon, LockIcon } from '../icons';
import { EventView } from './EventView';
import { CALENDAR_DENIED, dayLabel, isToday, localDate, timeOf } from './format';
import styles from './Upcoming.module.css';
import { type UpcomingView, useUpcoming } from './useUpcoming';

export const EMPTY = 'Nothing on your calendar for the next 7 days.';

export function Upcoming({ onOpenNote }: { onOpenNote: (id: number) => void }) {
  const view = useUpcoming();
  const [selected, setSelected] = useState<string | null>(null);
  const returnTo = useRef<string | null>(null);
  const rows = useRef(new Map<string, HTMLButtonElement>());

  useEffect(() => {
    if (selected !== null || returnTo.current === null) return;
    rows.current.get(returnTo.current)?.focus();
    returnTo.current = null;
  }, [selected]);

  if (selected !== null) {
    return (
      <section className={styles.upcoming}>
        <EventView occurrenceId={selected} onBack={() => setSelected(null)} onOpenNote={onOpenNote} />
      </section>
    );
  }

  const open = (occurrenceId: string) => {
    returnTo.current = occurrenceId;
    setSelected(occurrenceId);
  };

  return (
    <section className={styles.upcoming} aria-labelledby="upcoming-heading">
      <div className={styles.head}>
        <h2 id="upcoming-heading">Upcoming</h2>
        <p className={styles.sub}>Your next 7 days. Open a meeting to record it with its details.</p>
      </div>
      {/* Always mounted: screen readers skip a live region that appears already filled. */}
      <div role="status" className={statusClass(view)}>
        {view.kind === 'denied' ? <LockIcon size={18} /> : null}
        {statusText(view)}
      </div>
      {view.kind === 'ready' ? <Days days={view.days} rows={rows.current} onOpen={open} /> : null}
    </section>
  );
}

function statusClass(view: UpcomingView): string {
  if (view.kind === 'loading') return styles.loading;
  if (view.kind === 'denied') return styles.denied;
  if (view.kind === 'error' || (view.kind === 'ready' && view.error)) return styles.problem;
  return styles.quiet;
}

/** Today's meeting under way, else today's next one: the row that gets a badge. */
function highlight(days: CalendarDay[], now: Date): { id: string; label: string } | null {
  const today = days.find((day) => isToday(localDate(day.date), now));
  if (!today) return null;
  const t = now.getTime();
  const current = today.events.find((e) => new Date(e.start).getTime() <= t && new Date(e.end).getTime() > t);
  if (current) return { id: current.occurrence_id, label: 'Now' };
  const next = today.events.find((e) => new Date(e.start).getTime() > t);
  return next ? { id: next.occurrence_id, label: 'Up next' } : null;
}

function statusText(view: UpcomingView): string | null {
  switch (view.kind) {
    case 'loading':
      return 'Loading calendar…';
    case 'denied':
      return CALENDAR_DENIED;
    case 'error':
      return view.message;
    case 'ready':
      return view.error;
  }
}

function Days(props: { days: CalendarDay[]; rows: Map<string, HTMLButtonElement>; onOpen: (id: string) => void }) {
  const days = props.days.filter((day) => day.events.length > 0);
  if (days.length === 0)
    return (
      <div className={styles.empty}>
        <CalendarIcon size={28} />
        <p>{EMPTY}</p>
      </div>
    );

  const now = new Date();
  const badge = highlight(days, now);

  return days.map((day) => (
    <section key={day.date} className={styles.day} aria-labelledby={`day-${day.date}`}>
      <h3 id={`day-${day.date}`} className={styles.dayHeading}>
        {dayLabel(day.date)}
      </h3>
      <ul className={styles.events}>
        {day.events.map((event) => (
          <li key={event.occurrence_id}>
            <button
              type="button"
              className={styles.row}
              data-past={new Date(event.end).getTime() < now.getTime() || undefined}
              data-badge={badge?.id === event.occurrence_id || undefined}
              onClick={() => props.onOpen(event.occurrence_id)}
              ref={(node) => {
                if (node) props.rows.set(event.occurrence_id, node);
                else props.rows.delete(event.occurrence_id);
              }}
            >
              <span className={styles.time}>
                <span>{timeOf(event.start)}</span>
                <span className={styles.end}>{timeOf(event.end)}</span>
              </span>
              <span className={styles.main}>
                <span className={styles.eventTitle}>{event.title}</span>
                {event.calendar ? <span className={styles.meta}>{event.calendar}</span> : null}
              </span>
              {badge?.id === event.occurrence_id ? <span className={styles.badge}>{badge.label}</span> : null}
              {event.has_notes ? <span className={styles.marker}>notes</span> : null}
              <span className={styles.chevron} aria-hidden="true">
                ›
              </span>
            </button>
          </li>
        ))}
      </ul>
    </section>
  ));
}
