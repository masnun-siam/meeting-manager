import { useEffect, useRef, useState } from 'react';
import {
  type AttendeeResponse,
  type CalendarEventDetail,
  calendarEvent,
  type EarlierNote,
  type Language,
  recorderStart,
} from '../api';
import { attempt } from '../recording/attempt';
import { LanguageSelect } from '../recording/LanguageSelect';
import { useRecorderStatus } from '../recording/useRecorderStatus';
import styles from './EventView.module.css';
import { CALENDAR_DENIED, localDate, timeOf, tooFar } from './format';

export const EARLIER_LIMIT = 20;
export const TOO_FAR = 'This event starts more than 7 days from now, so it cannot be recorded yet.';

const RESPONSES: Record<AttendeeResponse, string> = {
  accepted: 'Accepted',
  declined: 'Declined',
  tentative: 'Tentative',
  needs_action: 'No response',
};

type Loaded =
  | { kind: 'loading' }
  | { kind: 'error'; message: string }
  | { kind: 'ready'; event: CalendarEventDetail; earlier: EarlierNote[] };

function useEvent(occurrenceId: string): Loaded {
  const [loaded, setLoaded] = useState<Loaded>({ kind: 'loading' });

  useEffect(() => {
    let active = true;
    setLoaded({ kind: 'loading' });
    attempt(() => calendarEvent({ occurrence_id: occurrenceId })).then((result) => {
      if (!active) return;
      if (result.ok) setLoaded({ kind: 'ready', event: result.reply.event, earlier: result.reply.earlier });
      else setLoaded({ kind: 'error', message: result.code === 'calendar_denied' ? CALENDAR_DENIED : result.text });
    });
    return () => {
      active = false;
    };
  }, [occurrenceId]);

  return loaded;
}

function when(event: CalendarEventDetail): string {
  const day = new Date(event.start).toLocaleDateString(undefined, { weekday: 'long', month: 'long', day: 'numeric' });
  return `${day}, ${timeOf(event.start)}–${timeOf(event.end)}`;
}

export function EventView(props: { occurrenceId: string; onBack: () => void; onOpenNote: (id: number) => void }) {
  const loaded = useEvent(props.occurrenceId);
  const heading = useRef<HTMLHeadingElement>(null);
  const back = useRef<HTMLButtonElement>(null);

  useEffect(() => {
    if (loaded.kind === 'ready') heading.current?.focus();
    else if (loaded.kind === 'error') back.current?.focus();
  }, [loaded.kind]);

  return (
    <article className={styles.event}>
      <button ref={back} type="button" onClick={props.onBack}>
        Back
      </button>
      {loaded.kind === 'loading' ? <p role="status">Loading event…</p> : null}
      {loaded.kind === 'error' ? (
        <p role="status" className={styles.problem}>
          {loaded.message}
        </p>
      ) : null}
      {loaded.kind === 'ready' ? (
        <>
          <h2 ref={heading} tabIndex={-1} className={styles.title}>
            {loaded.event.title}
          </h2>
          <Details event={loaded.event} />
          <StartFromEvent event={loaded.event} />
          <Earlier earlier={loaded.earlier} onOpenNote={props.onOpenNote} />
        </>
      ) : null}
    </article>
  );
}

/** Attendees have no id and names can repeat: number each repeat of a name. */
function keyed(attendees: CalendarEventDetail['attendees']) {
  const seen = new Map<string, number>();
  return attendees.map((attendee) => {
    const n = (seen.get(attendee.name) ?? 0) + 1;
    seen.set(attendee.name, n);
    return { key: `${attendee.name}#${n}`, attendee };
  });
}

function Details({ event }: { event: CalendarEventDetail }) {
  return (
    <>
      <dl className={styles.facts}>
        <dt>Time</dt>
        <dd>{when(event)}</dd>
        {event.calendar ? (
          <>
            <dt>Calendar</dt>
            <dd>{event.calendar}</dd>
          </>
        ) : null}
        {event.organizer ? (
          <>
            <dt>Organizer</dt>
            <dd>{event.organizer}</dd>
          </>
        ) : null}
      </dl>
      {event.attendees.length > 0 ? (
        <>
          <h3>Attendees</h3>
          <ul className={styles.attendees}>
            {keyed(event.attendees).map(({ key, attendee }) => (
              <li key={key}>
                {attendee.name} <span className={styles.response}>({RESPONSES[attendee.response]})</span>
              </li>
            ))}
          </ul>
        </>
      ) : null}
      {event.description ? (
        <>
          <h3>Notes</h3>
          <p className={styles.notes}>{event.description}</p>
        </>
      ) : null}
    </>
  );
}

function StartFromEvent({ event }: { event: CalendarEventDetail }) {
  const { status, refresh } = useRecorderStatus();
  const [chosen, setChosen] = useState<Language | null>(null);
  const [message, setMessage] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const idle = status?.state === 'idle';
  const far = tooFar(event.start);
  const language = chosen ?? status?.language ?? 'en';

  const start = async () => {
    setMessage(null);
    setBusy(true);
    const result = await attempt(() => recorderStart({ language, event_occurrence_id: event.occurrence_id }));
    setBusy(false);
    if (result.ok) refresh();
    else setMessage(result.text);
  };

  return (
    <div className={styles.start}>
      <LanguageSelect className={styles.field} value={language} disabled={status === null} onChange={setChosen} />
      <button type="button" onClick={start} disabled={!idle || far || busy}>
        Start recording
      </button>
      <div role="status" className={styles.problem}>
        {far ? <p>{TOO_FAR}</p> : null}
        {message ? <p>{message}</p> : null}
      </div>
    </div>
  );
}

function Earlier(props: { earlier: EarlierNote[]; onOpenNote: (id: number) => void }) {
  if (props.earlier.length === 0) return null;
  return (
    <>
      <h3>Earlier notes</h3>
      <ul className={styles.earlier}>
        {props.earlier.slice(0, EARLIER_LIMIT).map((note) => (
          <li key={note.id}>
            <button type="button" onClick={() => props.onOpenNote(note.id)}>
              {localDate(note.recorded_at).toLocaleDateString(undefined, {
                year: 'numeric',
                month: 'short',
                day: 'numeric',
              })}{' '}
              · {note.title ?? 'Untitled meeting'}
            </button>
          </li>
        ))}
      </ul>
    </>
  );
}
