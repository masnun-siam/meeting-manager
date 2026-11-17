import { useEffect, useId, useRef, useState } from 'react';
import type { Note } from '../../api';
import styles from './NoteView.module.css';
import type { Autosave } from './useAutosave';

/**
 * Links in the summary are not followed: the app window would leave the app for a remote page,
 * and the window has no way to open the system browser yet.
 */
function stayInApp(e: Event) {
  if (e.target instanceof Element && e.target.closest('a')) e.preventDefault();
}

/** The summary: `body_html` in read mode, the markdown in a textarea in edit mode (`done` notes only). */
export function Summary({ note, autosave, editable }: { note: Note; autosave: Autosave; editable: boolean }) {
  const [editing, setEditing] = useState(false);
  const textarea = useId();
  const body = useRef<HTMLDivElement>(null);

  // Enter on a focused link fires the same click event, so keyboard users are covered too.
  useEffect(() => {
    const element = body.current;
    if (!element) return;
    element.addEventListener('click', stayInApp);
    element.addEventListener('auxclick', stayInApp);
    return () => {
      element.removeEventListener('click', stayInApp);
      element.removeEventListener('auxclick', stayInApp);
    };
  });

  const toggle = () => {
    if (editing) autosave.blur('body_md');
    setEditing(!editing);
  };

  return (
    <section aria-labelledby={`${textarea}-heading`} className={styles.section}>
      {/* A right-aligned first row of the panel: the bar's right side is the player's (FR-11). */}
      <div className={styles.sectionHead}>
        <h3 id={`${textarea}-heading`} className="sr-only">
          Summary
        </h3>
        {editable ? (
          <button type="button" data-variant={editing ? 'primary' : undefined} onClick={toggle} aria-pressed={editing}>
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
          ref={body}
          className={styles.prose}
          // biome-ignore lint/security/noDangerouslySetInnerHtml: body_html is rendered by pulldown-cmark on the Rust side with raw HTML shown as text and unsafe link targets dropped (#143, D25); the CSP blocks inline script.
          dangerouslySetInnerHTML={{ __html: note.body_html }}
        />
      )}
    </section>
  );
}
