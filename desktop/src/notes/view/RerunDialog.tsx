import { type KeyboardEvent, useEffect, useRef } from 'react';
import styles from './NoteView.module.css';

/** FR-19: a rerun of a note with edited fields asks whether to keep them. */
export function RerunDialog(props: { onChoose: (replaceEdits: boolean) => void; onCancel: () => void }) {
  const first = useRef<HTMLButtonElement>(null);
  const dialog = useRef<HTMLDivElement>(null);

  const cancel = useRef(props.onCancel);
  cancel.current = props.onCancel;

  useEffect(() => {
    first.current?.focus();
    // Escape closes it wherever focus is, also after a click on the backdrop.
    const onEscape = (e: globalThis.KeyboardEvent) => {
      if (e.key === 'Escape') cancel.current();
    };
    document.addEventListener('keydown', onEscape);
    return () => document.removeEventListener('keydown', onEscape);
  }, []);

  const onKeyDown = (e: KeyboardEvent) => {
    if (e.key !== 'Tab') return;
    // Keep focus inside the dialog while it is open.
    const buttons = [...(dialog.current?.querySelectorAll('button') ?? [])];
    const at = buttons.indexOf(document.activeElement as HTMLButtonElement);
    const to = e.shiftKey ? (at <= 0 ? buttons.length - 1 : at - 1) : (at + 1) % buttons.length;
    e.preventDefault();
    buttons[to]?.focus();
  };

  return (
    <div className={styles.backdrop}>
      <div
        ref={dialog}
        role="dialog"
        aria-modal="true"
        aria-labelledby="rerun-title"
        aria-describedby="rerun-text"
        className={styles.dialog}
        onKeyDown={onKeyDown}
      >
        <h3 id="rerun-title">Rerun summary</h3>
        <p id="rerun-text">You edited this note. Keep your edits in the new summary, or replace them?</p>
        <div className={styles.dialogButtons}>
          <button ref={first} type="button" data-variant="primary" onClick={() => props.onChoose(false)}>
            Keep my edits
          </button>
          <button type="button" onClick={() => props.onChoose(true)}>
            Replace with new summary
          </button>
          <button type="button" data-variant="plain" onClick={props.onCancel}>
            Cancel
          </button>
        </div>
      </div>
    </div>
  );
}
