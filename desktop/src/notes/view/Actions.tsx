import { useEffect, useRef, useState } from 'react';
import { type Note, noteArchive, noteOpen, noteRerun, noteTrash } from '../../api';
import { attempt } from '../../recording/attempt';
import { hasEdits, NOT_WHILE_PROCESSING, problemText, RECORDING_MISSING } from './format';
import styles from './NoteView.module.css';
import { RerunDialog } from './RerunDialog';

export const CONFIRM_MS = 3000;
export const RERUN_STARTED = 'Summary rerun started';
export const ARCHIVED = 'Archived';

type Props = {
  note: Note;
  /** The latest stored note, read after pending saves are flushed. */
  latest: () => Note;
  flushAll: () => Promise<void>;
  reload: () => Promise<{ ok: boolean; code?: string | null }>;
  markRerun: () => void;
  onBack: () => void;
  say: (message: string | null) => void;
};

/** Open in Obsidian, Open folder, Rerun summary, Archive and Trash (FR-17, FR-19, FR-21). */
export function Actions(props: Props) {
  const { note, say } = props;
  const [armed, setArmed] = useState(false);
  const [asking, setAsking] = useState(false);
  const [busy, setBusy] = useState(false);
  const rerunButton = useRef<HTMLButtonElement>(null);

  useEffect(() => {
    if (!armed) return;
    const timer = setTimeout(() => setArmed(false), CONFIRM_MS);
    return () => clearTimeout(timer);
  }, [armed]);

  const processing = note.status === 'processing';
  const missing = note.recording_missing;

  const open = async (what: 'obsidian' | 'folder') => {
    say(null);
    const result = await attempt(() => noteOpen({ id: note.id, what }));
    if (!result.ok) say(problemText(result.code, result.text));
  };

  const rerun = async (replaceEdits: boolean) => {
    setBusy(true);
    const result = await attempt(() => noteRerun({ id: note.id, replace_edits: replaceEdits }));
    setBusy(false);
    if (!result.ok) {
      say(problemText(result.code, result.text));
      return;
    }
    say(RERUN_STARTED);
    props.markRerun();
    void props.reload();
  };

  const askRerun = async () => {
    say(null);
    await props.flushAll();
    if (hasEdits(props.latest())) setAsking(true);
    else await rerun(false);
  };

  const closeDialog = () => {
    setAsking(false);
    rerunButton.current?.focus();
  };

  const archive = async () => {
    say(null);
    setBusy(true);
    await props.flushAll();
    const result = await attempt(() => noteArchive({ id: note.id }));
    setBusy(false);
    if (!result.ok) {
      say(problemText(result.code, result.text));
      return;
    }
    // Archiving a note whose recording is gone removes its row (recording-folders FR-9).
    const reloaded = await props.reload();
    if (!reloaded.ok && reloaded.code === 'unknown_note') props.onBack();
    else say(ARCHIVED);
  };

  const trash = async () => {
    if (!armed) {
      say(null);
      setArmed(true);
      return;
    }
    setArmed(false);
    setBusy(true);
    await props.flushAll();
    const result = await attempt(() => noteTrash({ id: note.id }));
    setBusy(false);
    if (result.ok) props.onBack();
    else say(problemText(result.code, result.text));
  };

  return (
    <div className={styles.actions}>
      <div className={styles.row}>
        <button type="button" onClick={() => open('obsidian')}>
          Open in Obsidian
        </button>
        <button
          type="button"
          disabled={missing}
          aria-describedby={missing ? 'note-missing' : undefined}
          onClick={() => open('folder')}
        >
          Open folder
        </button>
        <button
          ref={rerunButton}
          type="button"
          disabled={missing}
          aria-describedby={missing ? 'note-missing' : undefined}
          onClick={askRerun}
        >
          Rerun summary
        </button>
        <button
          type="button"
          disabled={processing || busy}
          aria-describedby={processing ? 'note-processing' : undefined}
          onClick={archive}
        >
          Archive
        </button>
        <button
          type="button"
          disabled={processing || busy}
          aria-describedby={processing ? 'note-processing' : undefined}
          onClick={trash}
        >
          {armed ? 'Confirm trash' : 'Trash'}
        </button>
      </div>
      {missing ? (
        <p id="note-missing" className={styles.reason}>
          {RECORDING_MISSING}
        </p>
      ) : null}
      {processing ? (
        <p id="note-processing" className={styles.reason}>
          {NOT_WHILE_PROCESSING}
        </p>
      ) : null}
      {asking ? (
        <RerunDialog
          onCancel={closeDialog}
          onChoose={(replaceEdits) => {
            closeDialog();
            void rerun(replaceEdits);
          }}
        />
      ) : null}
    </div>
  );
}
