import { type KeyboardEvent, type ReactNode, useId, useRef, useState } from 'react';
import styles from './NoteView.module.css';

type Tab = 'summary' | 'transcript';
const TABS: { id: Tab; label: string }[] = [
  { id: 'summary', label: 'Summary' },
  { id: 'transcript', label: 'Transcript' },
];

/**
 * The note's body (FR-11): a bar holding the Summary / Transcript tabs and the player slot, which
 * sticks to the top of the page, then the selected panel beside the side column (under it below
 * 900 px). The page has one scroll (D6): nothing here scrolls on its own.
 *
 * Tabs follow the ARIA tabs pattern (arrow keys, Home and End move between tabs). The selected tab
 * lives here, so it is kept while the note is open: polls and reloads re-render without unmounting.
 * The transcript arrives with each label already replaced by its speaker's shown name (`note_get`),
 * so it is shown as it is, as text.
 */
export function Tabs({
  summary,
  transcript,
  aside,
  player = null,
}: {
  summary: ReactNode;
  transcript: string | null;
  aside: ReactNode;
  player?: ReactNode;
}) {
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
      <div className={styles.bar}>
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
        <div className={styles.playerSlot}>{player}</div>
      </div>
      <div className={styles.layout}>
        <div className={styles.primary}>
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
            {transcript ? <Transcript text={transcript} /> : <p>No transcript.</p>}
          </div>
        </div>
        {/* After the panels in the DOM, so a single column shows it under the content. */}
        <div className={styles.aside}>{aside}</div>
      </div>
    </div>
  );
}

const LINE = /^(\[\d{2}:\d{2}:\d{2}\]) ([^:]+): (.*)$/;
const COLOURS = 6;

/**
 * One row per `[hh:mm:ss] Name: text` line, each speaker in their own colour (by order of first
 * appearance). Other lines are shown as they are. The text stays text: nothing is parsed as HTML.
 */
function Transcript({ text }: { text: string }) {
  const speakers = new Map<string, number>();
  const lines = text.split('\n').filter((line) => line.trim() !== '');
  return (
    <div className={styles.transcript}>
      {lines.map((line, i) => {
        const parts = LINE.exec(line);
        const key = `${i}:${line.slice(0, 24)}`;
        if (!parts) return <p key={key}>{line}</p>;
        const [, stamp, who, said] = parts;
        if (!speakers.has(who)) speakers.set(who, speakers.size % COLOURS);
        return (
          <p key={key} className={styles.utterance}>
            <span className={styles.stamp}>{stamp} </span>
            <span>
              <span className={styles.who} data-speaker={speakers.get(who)}>
                {who}:{' '}
              </span>
              {said}
            </span>
          </p>
        );
      })}
    </div>
  );
}
