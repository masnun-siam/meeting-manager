import { type KeyboardEvent, type ReactNode, useId, useRef, useState } from 'react';
import styles from './NoteView.module.css';

type Tab = 'summary' | 'transcript';
const TABS: { id: Tab; label: string }[] = [
  { id: 'summary', label: 'Summary' },
  { id: 'transcript', label: 'Transcript' },
];

/**
 * The Summary / Transcript tabs (ARIA tabs pattern: arrow keys, Home and End move between tabs).
 * The transcript arrives with each label already replaced by its speaker's shown name (`note_get`),
 * so it is shown as it is, as text.
 */
export function Tabs({ summary, transcript }: { summary: ReactNode; transcript: string | null }) {
  const [selected, setSelected] = useState<Tab>('summary');
  const base = useId();
  const refs = useRef(new Map<Tab, HTMLButtonElement>());

  const select = (tab: Tab) => {
    setSelected(tab);
    refs.current.get(tab)?.focus();
  };

  const onKeyDown = (e: KeyboardEvent) => {
    const index = TABS.findIndex((tab) => tab.id === selected);
    const next: Record<string, number> = {
      ArrowRight: (index + 1) % TABS.length,
      ArrowLeft: (index - 1 + TABS.length) % TABS.length,
      Home: 0,
      End: TABS.length - 1,
    };
    if (!(e.key in next)) return;
    e.preventDefault();
    select(TABS[next[e.key]].id);
  };

  return (
    <div className={styles.tabs}>
      <div role="tablist" aria-label="Note" className={styles.tablist} onKeyDown={onKeyDown}>
        {TABS.map((tab) => (
          <button
            key={tab.id}
            ref={(el) => {
              if (el) refs.current.set(tab.id, el);
            }}
            type="button"
            role="tab"
            id={`${base}-${tab.id}-tab`}
            aria-selected={selected === tab.id}
            aria-controls={`${base}-${tab.id}-panel`}
            tabIndex={selected === tab.id ? 0 : -1}
            onClick={() => setSelected(tab.id)}
          >
            {tab.label}
          </button>
        ))}
      </div>
      <div
        role="tabpanel"
        id={`${base}-summary-panel`}
        aria-labelledby={`${base}-summary-tab`}
        hidden={selected !== 'summary'}
      >
        {summary}
      </div>
      <div
        role="tabpanel"
        id={`${base}-transcript-panel`}
        aria-labelledby={`${base}-transcript-tab`}
        hidden={selected !== 'transcript'}
      >
        {transcript ? <pre className={styles.transcript}>{transcript}</pre> : <p>No transcript.</p>}
      </div>
    </div>
  );
}
