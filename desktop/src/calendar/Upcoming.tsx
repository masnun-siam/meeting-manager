import { useEffect, useRef, useState } from 'react';
import type { CalendarDay } from '../api';
import { EventView } from './EventView';
import { CALENDAR_DENIED, dayLabel, timeRange } from './format';
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
      <h2 id="upcoming-heading">Upcoming</h2>
      {/* Always mounted: screen readers skip a live region that appears already filled. */}
      <div role="status" className={view.kind === 'loading' ? undefined : styles.problem}>
        {statusText(view)}
      </div>
      {view.kind === 'ready' ? <Days days={view.days} rows={rows.current} onOpen={open} /> : null}
    </section>
  );
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
  if (days.length === 0) return <p>{EMPTY}</p>;

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
              onClick={() => props.onOpen(event.occurrence_id)}
              ref={(node) => {
                if (node) props.rows.set(event.occurrence_id, node);
                else props.rows.delete(event.occurrence_id);
              }}
            >
              <span className={styles.eventTitle}>{event.title}</span>
              <span className={styles.meta}>
                {timeRange(event.start, event.end)}
                {event.calendar ? ` · ${event.calendar}` : ''}
              </span>
              {event.has_notes ? <span className={styles.marker}>notes</span> : null}
            </button>
          </li>
        ))}
      </ul>
    </section>
  ));
}
