import { useEffect, useRef, useState } from 'react';
import {
  type AttendeeResponse,
  type CalendarEventDetail,
  calendarEvent,
  type EarlierNote,
  type Language,
  recorderStart,
} from '../api';
import { BackIcon, CalendarIcon, ClockIcon, MicIcon, NotesIcon, PeopleIcon, RecordIcon } from '../icons';
import { attempt } from '../recording/attempt';
import { LanguageSelect } from '../recording/LanguageSelect';
import { useRecorderStatus } from '../recording/useRecorderStatus';
import styles from './EventView.module.css';
import { CALENDAR_DENIED, localDate, timeOf, tooFar } from './format';

export const EARLIER_LIMIT = 20;
export const TOO_FAR = 'This event starts more than 7 days from now, so it cannot be recorded yet.';
export const BUSY = 'A recording is already running.';

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
      <button ref={back} type="button" data-variant="plain" className={styles.back} onClick={props.onBack}>
        <BackIcon size={14} />
        Back
      </button>
      {/* Always mounted: screen readers skip a live region that appears already filled. */}
      <div role="status" className={loaded.kind === 'error' ? styles.problem : undefined}>
        {loaded.kind === 'loading' ? 'Loading event…' : null}
        {loaded.kind === 'error' ? loaded.message : null}
      </div>
      {loaded.kind === 'ready' ? (
        <>
          <h2 ref={heading} tabIndex={-1} className={styles.title}>
            {loaded.event.title}
          </h2>
          <StartFromEvent event={loaded.event} />
          <div className={styles.columns}>
            <Details event={loaded.event} />
            <Earlier earlier={loaded.earlier} onOpenNote={props.onOpenNote} />
          </div>
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
    <div className={styles.details}>
      <dl className={styles.facts}>
        <dt>
          <ClockIcon size={14} />
          Time
        </dt>
        <dd>{when(event)}</dd>
        {event.calendar ? (
          <>
            <dt>
              <CalendarIcon size={14} />
              Calendar
            </dt>
            <dd>{event.calendar}</dd>
          </>
        ) : null}
        {event.organizer ? (
          <>
            <dt>
              <PeopleIcon size={14} />
              Organizer
            </dt>
            <dd>{event.organizer}</dd>
          </>
        ) : null}
      </dl>
      {event.attendees.length > 0 ? (
        <>
          <h3 className={styles.subhead}>Attendees</h3>
          <ul className={styles.attendees}>
            {keyed(event.attendees).map(({ key, attendee }) => (
              <li key={key} className={styles.attendee}>
                <span className={styles.avatar} data-initials={initials(attendee.name)} aria-hidden="true" />
                <span className={styles.name}>{attendee.name}</span>
                <span className="sr-only"> (</span>
                <span className={styles.response} data-response={attendee.response}>
                  {RESPONSES[attendee.response]}
                </span>
                <span className="sr-only">)</span>
              </li>
            ))}
          </ul>
        </>
      ) : null}
      {event.description ? (
        <>
          <h3 className={styles.subhead}>Notes</h3>
          <p className={styles.notes}>{event.description}</p>
        </>
      ) : null}
    </div>
  );
}

/** Up to two initials for the attendee's avatar circle. */
function initials(name: string): string {
  const parts = name.trim().split(/\s+/).filter(Boolean);
  return parts
    .slice(0, 2)
    .map((part) => part[0]?.toUpperCase() ?? '')
    .join('');
}

function StartFromEvent({ event }: { event: CalendarEventDetail }) {
  const { status, refresh } = useRecorderStatus();
  const [chosen, setChosen] = useState<Language | null>(null);
  const [message, setMessage] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const idle = status?.state === 'idle';
  const running = status !== null && !idle;
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
      <div className={styles.startText}>
        <span className={styles.startIcon} aria-hidden="true">
          <MicIcon size={18} />
        </span>
        <span>
          <strong>Record this meeting</strong>
          <span className={styles.startHint}>The note is linked to this event and its attendees.</span>
        </span>
      </div>
      <LanguageSelect className={styles.field} value={language} disabled={status === null} onChange={setChosen} />
      <button
        type="button"
        data-variant="record"
        className={styles.startButton}
        onClick={start}
        disabled={!idle || far || busy}
      >
        <RecordIcon />
        Start recording
      </button>
      <div role="status" className={styles.problem}>
        {far ? <p>{TOO_FAR}</p> : null}
        {running ? <p>{BUSY}</p> : null}
        {message ? <p>{message}</p> : null}
      </div>
    </div>
  );
}

function Earlier(props: { earlier: EarlierNote[]; onOpenNote: (id: number) => void }) {
  if (props.earlier.length === 0) return null;
  return (
    <div className={styles.earlierBox}>
      <h3 className={styles.subhead}>Earlier notes</h3>
      <ul className={styles.earlier}>
        {props.earlier.slice(0, EARLIER_LIMIT).map((note) => (
          <li key={note.id}>
            <button type="button" className={styles.earlierRow} onClick={() => props.onOpenNote(note.id)}>
              <NotesIcon size={14} />
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
    </div>
  );
}
