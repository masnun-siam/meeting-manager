import { useEffect, useRef } from 'react';
import styles from './NoteView.module.css';
import type { Autosave } from './useAutosave';

/**
 * "My notes", the third tab (FR-12): editable whatever the note's status (FR-18). The editor is as
 * wide as the tab and grows with its text, so the page scrolls, not the editor. CSS
 * `field-sizing: content` would do this, but WKWebView on macOS 14.4 lacks it.
 */
export function MyNotes({ autosave }: { autosave: Autosave }) {
  const ref = useRef<HTMLTextAreaElement>(null);

  // Typing measures in `onChange` (once per input event). Being shown (a hidden panel measures 0)
  // and a window that rewraps the text change the editor's width: measure again then.
  useEffect(() => {
    const el = ref.current;
    if (!el || typeof ResizeObserver === 'undefined') return;
    let width = -1;
    const observer = new ResizeObserver(() => {
      if (el.clientWidth === width) return;
      width = el.clientWidth;
      grow(el);
    });
    observer.observe(el);
    return () => observer.disconnect();
  }, []);

  return (
    <textarea
      ref={ref}
      placeholder="Your own notes. Saved as you type and kept when the summary is rerun."
      aria-label="My notes"
      className={styles.myNotes}
      value={autosave.value('my_notes')}
      onChange={(e) => {
        autosave.change('my_notes', e.target.value);
        grow(e.target);
      }}
      onFocus={() => autosave.focus('my_notes')}
      onBlur={() => autosave.blur('my_notes')}
    />
  );
}

/**
 * Sets the height to the text's. Collapsing to `auto` first lets it shrink, but briefly shortens
 * the page, which would pull a scrolled-down page up: the scroll positions are put back.
 */
function grow(el: HTMLTextAreaElement) {
  if (el.closest('[hidden]')) return; // nothing to measure in a hidden panel
  const scrolled: [Element, number][] = [];
  for (let node = el.parentElement; node; node = node.parentElement) {
    if (node.scrollTop > 0) scrolled.push([node, node.scrollTop]);
  }
  el.style.height = 'auto';
  // `border-box` sizing: the height includes the borders, which `scrollHeight` leaves out.
  el.style.height = `${el.scrollHeight + el.offsetHeight - el.clientHeight}px`;
  for (const [node, top] of scrolled) node.scrollTop = top;
}
