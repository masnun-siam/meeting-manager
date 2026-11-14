import styles from './NoteView.module.css';
import type { Autosave } from './useAutosave';

/** "My notes": editable whatever the note's status (FR-18). */
export function MyNotes({ autosave }: { autosave: Autosave }) {
  return (
    <section aria-labelledby="note-my-notes" className={`${styles.section} ${styles.card}`}>
      <h3 id="note-my-notes" className={styles.cardTitle}>
        My notes
      </h3>
      <textarea
        placeholder="Your own notes. Saved as you type and kept when the summary is rerun."
        aria-labelledby="note-my-notes"
        className={styles.markdown}
        value={autosave.value('my_notes')}
        onChange={(e) => autosave.change('my_notes', e.target.value)}
        onFocus={() => autosave.focus('my_notes')}
        onBlur={() => autosave.blur('my_notes')}
      />
    </section>
  );
}
