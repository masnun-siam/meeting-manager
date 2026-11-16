import type { Note } from '../../api';
import { shownName } from './format';
import styles from './NoteView.module.css';
import type { Autosave } from './useAutosave';

/** Each speaker's label, shown name and evidence, with a rename field for a `done` note. */
export function SpeakersPanel({ note, autosave, editable }: { note: Note; autosave: Autosave; editable: boolean }) {
  if (note.speakers.length === 0) return null;
  return (
    <section aria-labelledby="note-speakers" className={`${styles.section} ${styles.card}`}>
      <h3 id="note-speakers" className={styles.cardTitle}>
        Speakers{' '}
        <span className={styles.count} aria-hidden="true">
          {note.speakers.length}
        </span>
      </h3>
      <ul className={styles.speakers}>
        {note.speakers.map((speaker) => {
          const key = `speaker:${speaker.label}` as const;
          return (
            <li key={speaker.label} className={styles.speaker}>
              <span className={styles.label}>{speaker.label}</span>
              {editable ? (
                <label className={styles.field}>
                  <span className="sr-only">Name for {speaker.label}</span>
                  <input
                    type="text"
                    value={autosave.value(key)}
                    onChange={(e) => autosave.change(key, e.target.value)}
                    onFocus={() => autosave.focus(key)}
                    onBlur={() => autosave.blur(key)}
                  />
                </label>
              ) : null}
              <span className={styles.detail} title={speaker.evidence || undefined}>
                <span className={styles.shown}>{shownName(speaker)}</span>
                {speaker.evidence ? <span className={styles.evidence}>{speaker.evidence}</span> : null}
              </span>
            </li>
          );
        })}
      </ul>
    </section>
  );
}
