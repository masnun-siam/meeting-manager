import { useId, useState } from 'react';
import type { Note } from '../../api';
import styles from './NoteView.module.css';
import type { Autosave } from './useAutosave';

/** The summary: `body_html` in read mode, the markdown in a textarea in edit mode (`done` notes only). */
export function Summary({ note, autosave, editable }: { note: Note; autosave: Autosave; editable: boolean }) {
  const [editing, setEditing] = useState(false);
  const textarea = useId();

  const toggle = () => {
    if (editing) autosave.blur('body_md');
    setEditing(!editing);
  };

  return (
    <section aria-labelledby={`${textarea}-heading`} className={styles.section}>
      <div className={styles.sectionHead}>
        <h3 id={`${textarea}-heading`}>Summary</h3>
        {editable ? (
          <button type="button" onClick={toggle} aria-pressed={editing}>
            {editing ? 'Done' : 'Edit'}
          </button>
        ) : null}
      </div>
      {editable && editing ? (
        <>
          <label htmlFor={textarea} className={styles.hint}>
            Summary (markdown)
          </label>
          <textarea
            id={textarea}
            className={styles.markdown}
            value={autosave.value('body_md')}
            onChange={(e) => autosave.change('body_md', e.target.value)}
            onFocus={() => autosave.focus('body_md')}
            onBlur={() => autosave.blur('body_md')}
          />
        </>
      ) : (
        <div
          className={styles.body}
          // biome-ignore lint/security/noDangerouslySetInnerHtml: body_html is rendered by pulldown-cmark on the Rust side with raw HTML shown as text and unsafe link targets dropped (#143, D25); the CSP blocks inline script.
          dangerouslySetInnerHTML={{ __html: note.body_html }}
        />
      )}
    </section>
  );
}
