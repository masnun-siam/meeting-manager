import { useEffect, useRef, useState } from 'react';
import type { CalendarDay } from '../api';
import { EventView } from './EventView';
import { CALENDAR_DENIED, dayLabel, timeRange } from './format';
import styles from './Upcoming.module.css';
import { useUpcoming } from './useUpcoming';

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
      {view.kind === 'loading' ? <p role="status">Loading calendar…</p> : null}
      {view.kind === 'denied' ? (
        <p role="status" className={styles.problem}>
          {CALENDAR_DENIED}
        </p>
      ) : null}
      {view.kind === 'error' ? (
        <p role="status" className={styles.problem}>
          {view.message}
        </p>
      ) : null}
      {view.kind === 'ready' ? (
        <>
          <div role="status" className={styles.problem}>
            {view.error}
          </div>
          <Days days={view.days} rows={rows.current} onOpen={open} />
        </>
      ) : null}
    </section>
  );
}

function Days(props: { days: CalendarDay[]; rows: Map<string, HTMLButtonElement>; onOpen: (id: string) => void }) {
  const days = props.days.filter((day) => day.events.length > 0);
  if (days.length === 0) return <p>{EMPTY}</p>;

  return days.map((day) => (
    <section key={day.date} className={styles.day} aria-label={dayLabel(day.date)}>
      <h3 className={styles.dayHeading}>{dayLabel(day.date)}</h3>
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
