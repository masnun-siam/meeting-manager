import { useCallback, useEffect, useRef, useState } from 'react';
import type { Note } from '../../api';
import { Actions } from './Actions';
import { calendarName, duration, eventStart, languageName, recordedAt, sourceName } from './format';
import { MyNotes } from './MyNotes';
import styles from './NoteView.module.css';
import { SpeakersPanel } from './SpeakersPanel';
import { Summary } from './Summary';
import { Tabs } from './TranscriptTab';
import { type Autosave, type SaveState, useAutosave } from './useAutosave';
import { useNote } from './useNote';

export const UNTITLED = 'Untitled meeting';
const SAVE_TEXT: Record<SaveState, string | null> = {
  idle: null,
  saving: 'Saving…',
  saved: 'All changes saved',
  error: null,
};

/** One note (FR-17 to FR-21). Keyed by id so no draft or timer of one note reaches another. */
export function NoteView({ id, onBack }: { id: number; onBack: () => void }) {
  return <OneNote key={id} id={id} onBack={onBack} />;
}

function OneNote({ id, onBack }: { id: number; onBack: () => void }) {
  const saving = useRef<Autosave | null>(null);
  const { state, reload, apply, markRerun } = useNote(id, () => saving.current?.focusedDirty() ?? false);
  const note = state.kind === 'ready' ? state.note : null;
  const reloadQuietly = useCallback(() => void reload(), [reload]);
  const autosave = useAutosave({ id, note, apply, reload: reloadQuietly });
  saving.current = autosave;
  const latest = useRef(note);
  latest.current = note;
  const [message, setMessage] = useState<string | null>(null);
  const heading = useRef<HTMLHeadingElement>(null);
  const back = useRef<HTMLButtonElement>(null);

  useEffect(() => {
    if (state.kind === 'ready') heading.current?.focus();
    else if (state.kind === 'error') back.current?.focus();
  }, [state.kind]);

  const leave = () => {
    void autosave.flushAll();
    onBack();
  };

  return (
    <article className={styles.note}>
      <button ref={back} type="button" onClick={leave}>
        Back
      </button>
      {/* Always mounted: screen readers skip a live region that appears already filled. */}
      <div role="status" className={styles.problem}>
        {state.kind === 'loading' ? 'Loading…' : null}
        {state.kind === 'error' ? state.message : null}
        {state.kind === 'ready' ? message : null}
      </div>
      {note ? (
        <>
          <h2 ref={heading} tabIndex={-1} className={styles.heading}>
            {note.title ?? UNTITLED}
          </h2>
          <Facts note={note} />
          <Status note={note} />
          <div role="status" className={styles.saveLine}>
            {autosave.saveState === 'error' ? autosave.problem : SAVE_TEXT[autosave.saveState]}
          </div>
          <div role="alert" className={styles.banner}>
            {autosave.vaultError !== null ? (
              <>
                <span>Vault note not updated: {autosave.vaultError}</span>{' '}
                <button type="button" onClick={autosave.retryVault}>
                  Retry
                </button>
              </>
            ) : null}
          </div>
          <Actions
            note={note}
            latest={() => latest.current ?? note}
            flushAll={autosave.flushAll}
            reload={reload}
            markRerun={markRerun}
            onBack={onBack}
            say={setMessage}
          />
          <Editors note={note} autosave={autosave} />
        </>
      ) : null}
    </article>
  );
}

function Facts({ note }: { note: Note }) {
  const language = languageName(note.language);
  const calendar = note.event ? calendarName(note.event.calendar) : null;
  return (
    <dl className={styles.facts}>
      <dt>Recorded</dt>
      <dd>{recordedAt(note.recorded_at)}</dd>
      {note.duration_s !== null ? (
        <>
          <dt>Duration</dt>
          <dd>{duration(note.duration_s)}</dd>
        </>
      ) : null}
      {language ? (
        <>
          <dt>Language</dt>
          <dd>{language}</dd>
        </>
      ) : null}
      <dt>Source</dt>
      <dd>{sourceName(note.source)}</dd>
      {note.event ? (
        <>
          <dt>Event</dt>
          <dd>
            {note.event.title} · {eventStart(note.event.start)}
            {calendar ? ` · ${calendar}` : null}
          </dd>
        </>
      ) : null}
    </dl>
  );
}

function Status({ note }: { note: Note }) {
  if (note.status === 'done') return null;
  if (note.status === 'processing') return <p className={styles.state}>Processing…</p>;
  if (note.status === 'no_speech') return <p className={styles.state}>No speech detected</p>;
  return (
    <p className={styles.state}>
      Failed{note.failed_stage ? ` at ${note.failed_stage}` : ''}
      {note.error ? `: ${note.error}` : ''}
    </p>
  );
}

/** Title, summary and speaker names are editable only for a `done` note; "My notes" always. */
function Editors({ note, autosave }: { note: Note; autosave: Autosave }) {
  const editable = note.status === 'done';
  return (
    <>
      {editable ? (
        <label className={styles.stacked}>
          <span>Title</span>
          <input
            type="text"
            value={autosave.value('title')}
            onChange={(e) => autosave.change('title', e.target.value)}
            onFocus={() => autosave.focus('title')}
            onBlur={() => autosave.blur('title')}
          />
        </label>
      ) : null}
      <Tabs summary={<Summary note={note} autosave={autosave} editable={editable} />} transcript={note.transcript} />
      <SpeakersPanel note={note} autosave={autosave} editable={editable} />
      <MyNotes autosave={autosave} />
    </>
  );
}
