import { useEffect, useState } from 'react';
import { type NotesListRow, rescan } from '../../api';
import { SearchIcon } from '../../icons';
import { formatDuration, formatRecorded, languageLabel, sourceLabel, statusText } from './format';
import styles from './NotesList.module.css';
import { errorMessage, useNotes } from './useNotes';
import { useRescanProgress } from './useRescanProgress';

export const SEARCH_DEBOUNCE_MS = 200;

const asQuery = (input: string) => (input.trim() === '' ? null : input.trim());

export function NotesList({ onOpen }: { onOpen: (id: number) => void }) {
  const [input, setInput] = useState('');
  const [query, setQuery] = useState<string | null>(null);
  const { view, refresh } = useNotes(query);
  const { progress, watch } = useRescanProgress(refresh);
  const [actionProblem, setActionProblem] = useState<string | null>(null);

  useEffect(() => {
    const timer = setTimeout(() => setQuery(asQuery(input)), SEARCH_DEBOUNCE_MS);
    return () => clearTimeout(timer);
  }, [input]);

  async function rescanFolders() {
    setActionProblem(null);
    try {
      const reply = await rescan();
      if (reply.ok) watch();
      else setActionProblem(reply.message);
    } catch (error) {
      setActionProblem(errorMessage(error));
    }
  }

  const problem = actionProblem ?? (view.kind === 'list' ? view.problem : view.kind === 'failed' ? view.message : null);

  return (
    <section className={styles.notes}>
      <div className={styles.head}>
        <div>
          <h2>All notes</h2>
          <p className={styles.sub}>
            {view.kind === 'list' && view.notes.length > 0
              ? `${view.notes.length} ${view.notes.length === 1 ? 'meeting' : 'meetings'}${view.query === null ? '' : ' found'}`
              : 'Every recorded meeting, newest first.'}
          </p>
        </div>
        <label className={styles.searchBox}>
          <SearchIcon size={14} />
          <input
            type="search"
            aria-label="Search meetings"
            placeholder="Search meetings"
            className={styles.search}
            value={input}
            onChange={(event) => setInput(event.target.value)}
          />
        </label>
      </div>
      <p role="status" className={styles.progress}>
        {progress?.running && `Loading meetings… ${progress.done} of ${progress.total}`}
      </p>
      {view.kind === 'loading' && <p className={styles.quiet}>Loading meetings…</p>}
      {view.kind === 'unavailable' && (
        <div role="alert" className={styles.problem}>
          <p>Notes database can't be opened: {view.message}</p>
          <button type="button" data-variant="primary" onClick={rescanFolders}>
            Rescan folders
          </button>
        </div>
      )}
      {view.kind === 'list' && view.notes.length === 0 && (
        <p className={styles.empty}>
          {view.query === null ? (
            <>
              No meetings yet — start a recording or run <code>meeting import</code>.
            </>
          ) : (
            'No meetings match.'
          )}
        </p>
      )}
      {view.kind === 'list' && view.notes.length > 0 && (
        <ul aria-label="Notes" className={styles.rows}>
          {view.notes.map((note) => (
            <NoteRow key={note.id} note={note} onOpen={onOpen} />
          ))}
        </ul>
      )}
      <p role="status" className={styles.problem}>
        {problem}
      </p>
    </section>
  );
}

function NoteRow({ note, onOpen }: { note: NotesListRow; onOpen: (id: number) => void }) {
  const meta = [
    formatRecorded(note.recorded_at),
    formatDuration(note.duration_s),
    languageLabel(note.language),
    sourceLabel(note.source),
  ].filter((part) => part !== null);

  return (
    <li>
      <button type="button" className={styles.row} data-status={note.status} onClick={() => onOpen(note.id)}>
        <i className={styles.mark} aria-hidden="true" />
        <span className={styles.main}>
          <span className={styles.title}>{note.title ?? note.folder_name}</span>
          <span className={styles.meta}>{meta.join(' · ')}</span>
        </span>
        <span className={styles.status}>
          <span className={styles.chip}>{statusText(note)}</span>
          {note.status === 'failed' && note.error && (
            <span className={styles.problem}>
              <span className={styles.sep}> — </span>
              {note.error}
            </span>
          )}
          {note.recording_missing && (
            <span className={styles.problem}>
              <span className={styles.sep}> · </span>
              Recording missing
            </span>
          )}
        </span>
        <span className={styles.chevron} aria-hidden="true">
          ›
        </span>
      </button>
    </li>
  );
}
