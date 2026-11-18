import { type ReactNode, useCallback, useEffect, useRef, useState } from 'react';
import type { Note } from '../../api';
import { BackIcon } from '../../icons';
import { formatDuration, formatRecorded, languageLabel, sourceLabel } from '../list/format';
import { Actions } from './Actions';
import { calendarName, eventStart } from './format';
import { MyNotes } from './MyNotes';
import styles from './NoteView.module.css';
import { Player, type PlayerHandle } from './Player';
import { SpeakersPanel } from './SpeakersPanel';
import { Summary } from './Summary';
import { Tabs } from './TranscriptTab';
import { type Autosave, type SaveState, useAutosave } from './useAutosave';
import { useNote } from './useNote';

export const UNTITLED = 'Untitled meeting';
export const UNSAVED = 'Some changes are not saved. Press Back again to leave without them.';
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
  const [message, setMessage] = useState<string | null>(null);
  const heading = useRef<HTMLHeadingElement>(null);
  const back = useRef<HTMLButtonElement>(null);
  // Set by the player; #210 hands `seek` to the transcript and #211 calls `pause` first.
  const [player, setPlayer] = useState<PlayerHandle | null>(null);

  useEffect(() => {
    if (state.kind === 'ready') heading.current?.focus();
    else if (state.kind === 'error') back.current?.focus();
  }, [state.kind]);

  const warned = useRef(false);
  const leave = async () => {
    await autosave.flushAll();
    // A refused or failed save keeps its text here: leaving would drop it without a word.
    if (autosave.unsaved() && !warned.current) {
      warned.current = true;
      setMessage(UNSAVED);
      return;
    }
    onBack();
  };

  return (
    <article className={styles.note}>
      <button ref={back} type="button" data-variant="plain" className={styles.back} onClick={leave}>
        <BackIcon size={14} />
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
          <div className={styles.header}>
            {/* The h2 stays the focus target; for a `done` note the title is edited in place. */}
            <h2 ref={heading} tabIndex={-1} className={note.status === 'done' ? 'sr-only' : styles.heading}>
              {note.title ?? UNTITLED}
            </h2>
            {note.status === 'done' ? (
              <input
                type="text"
                aria-label="Title"
                placeholder={UNTITLED}
                className={`${styles.heading} ${styles.titleInput}`}
                value={autosave.value('title')}
                onChange={(e) => autosave.change('title', e.target.value)}
                onFocus={() => autosave.focus('title')}
                onBlur={() => autosave.blur('title')}
                onKeyDown={(e) => {
                  if (e.key === 'Enter') e.currentTarget.blur();
                }}
              />
            ) : null}
            <div role="status" className={styles.saveLine} data-state={autosave.saveState}>
              {autosave.saveState === 'error' ? autosave.problem : SAVE_TEXT[autosave.saveState]}
            </div>
          </div>
          <Facts note={note} />
          <Status note={note} />
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
            latest={() => autosave.latest() ?? note}
            flushAll={autosave.flushAll}
            reload={reload}
            apply={apply}
            pause={() => player?.pause()}
            markRerun={markRerun}
            onBack={onBack}
            say={setMessage}
          />
          <Editors
            note={note}
            autosave={autosave}
            player={<Player note={note} say={setMessage} reload={reload} onHandle={setPlayer} />}
          />
        </>
      ) : null}
    </article>
  );
}

function Facts({ note }: { note: Note }) {
  const language = languageLabel(note.language);
  const length = formatDuration(note.duration_s);
  const calendar = note.event ? calendarName(note.event.calendar) : null;
  return (
    <dl className={styles.facts}>
      <div>
        <dt className="sr-only">Recorded</dt>
        <dd>{formatRecorded(note.recorded_at)}</dd>
      </div>
      {length !== null ? (
        <div>
          <dt className="sr-only">Duration</dt>
          <dd>{length}</dd>
        </div>
      ) : null}
      {language ? (
        <div>
          <dt className="sr-only">Language</dt>
          <dd>{language}</dd>
        </div>
      ) : null}
      <div>
        <dt className="sr-only">Source</dt>
        <dd>{sourceLabel(note.source)}</dd>
      </div>
      {note.event ? (
        <div className={styles.eventFact}>
          <dt>Event</dt>
          <dd>
            {note.event.title} · {eventStart(note.event.start)}
            {calendar ? ` · ${calendar}` : null}
          </dd>
        </div>
      ) : null}
    </dl>
  );
}

function Status({ note }: { note: Note }) {
  if (note.status === 'done') return null;
  if (note.status === 'processing')
    return (
      <p className={styles.state} data-status="processing">
        <span className={styles.spinner} aria-hidden="true" />
        Processing…
      </p>
    );
  if (note.status === 'no_speech')
    return (
      <p className={styles.state} data-status="no_speech">
        No speech detected
      </p>
    );
  return (
    <p className={styles.state} data-status="failed">
      Failed{note.failed_stage ? ` at ${note.failed_stage}` : ''}
      {note.error ? `: ${note.error}` : ''}
    </p>
  );
}

/** Summary and speaker names are editable only for a `done` note; "My notes" always. */
function Editors({ note, autosave, player }: { note: Note; autosave: Autosave; player: ReactNode }) {
  const editable = note.status === 'done';
  return (
    <Tabs
      summary={<Summary note={note} autosave={autosave} editable={editable} />}
      transcript={note.transcript}
      myNotes={<MyNotes autosave={autosave} />}
      hasMyNotes={autosave.value('my_notes').length > 0}
      aside={note.speakers.length > 0 ? <SpeakersPanel note={note} autosave={autosave} editable={editable} /> : null}
      player={player}
    />
  );
}
