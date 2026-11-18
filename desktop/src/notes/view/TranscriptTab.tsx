import { type KeyboardEvent, type ReactNode, useId, useRef, useState } from 'react';
import { stampSeconds } from './format';
import styles from './NoteView.module.css';

type Tab = 'summary' | 'transcript' | 'mynotes';
const TABS: { id: Tab; label: string }[] = [
  { id: 'summary', label: 'Summary' },
  { id: 'transcript', label: 'Transcript' },
  { id: 'mynotes', label: 'My notes' },
];

/**
 * The note's body (FR-11): a bar holding the Summary / Transcript / My notes tabs and the player
 * slot, which sticks to the top of the page, then the selected panel beside the side column (under
 * it below 900 px). Without an `aside` there is no side column and the panel takes the full width.
 * The page has one scroll (D6): nothing here scrolls on its own.
 *
 * The My notes tab shows a dot when `hasMyNotes` (FR-12); its name then ends in ", has text".
 * Hidden panels stay mounted, so a draft typed in one survives switching tabs.
 *
 * Tabs follow the ARIA tabs pattern (arrow keys, Home and End move between tabs). The selected tab
 * lives here, so it is kept while the note is open: polls and reloads re-render without unmounting.
 * The transcript arrives with each label already replaced by its speaker's shown name (`note_get`),
 * so it is shown as it is, as text. With `onSeek` (a ready player, FR-6) each line's time is a
 * button that moves the player there; without it the times are plain text.
 */
export function Tabs({
  summary,
  transcript,
  myNotes,
  hasMyNotes,
  aside,
  player = null,
  onSeek,
}: {
  summary: ReactNode;
  transcript: string | null;
  myNotes: ReactNode;
  hasMyNotes: boolean;
  aside: ReactNode;
  player?: ReactNode;
  onSeek?: (seconds: number) => void;
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
          {TABS.map((tab) => {
            const dot = tab.id === 'mynotes' && hasMyNotes;
            return (
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
                // An aria-label, not hidden text: a visually hidden span adds a space to the name.
                aria-label={dot ? `${tab.label}, has text` : undefined}
                tabIndex={selected === tab.id ? 0 : -1}
                onClick={() => setSelected(tab.id)}
              >
                {tab.label}
                {dot && <span className={styles.dot} aria-hidden="true" />}
              </button>
            );
          })}
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
            {transcript ? <Transcript text={transcript} onSeek={onSeek} /> : <p>No transcript.</p>}
          </div>
          <div
            role="tabpanel"
            id={`${base}-mynotes-panel`}
            // Named on its own: the tab's name gains ", has text", which is not the panel's.
            aria-label="My notes"
            hidden={selected !== 'mynotes'}
          >
            {myNotes}
          </div>
        </div>
        {/* After the panels in the DOM, so a single column shows it under the content. */}
        {aside && <div className={styles.aside}>{aside}</div>}
      </div>
    </div>
  );
}

const LINE = /^(\[\d{2}:\d{2}:\d{2}\]) ([^:]+): (.*)$/;
const COLOURS = 6;

/**
 * One row per `[hh:mm:ss] Name: text` line, each speaker in their own colour (by order of first
 * appearance). Other lines are shown as they are. The text stays text: nothing is parsed as HTML.
 * With `onSeek`, each time is a button (named for its action, D7) calling it with the time in seconds.
 */
function Transcript({ text, onSeek }: { text: string; onSeek?: (seconds: number) => void }) {
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
        const seconds = stampSeconds(stamp);
        return (
          <p key={key} className={styles.utterance}>
            {onSeek && seconds !== null ? (
              <span className={styles.stamp}>
                <button
                  type="button"
                  className={styles.stampButton}
                  aria-label={`Jump to ${stamp.slice(1, -1)}`}
                  onClick={() => onSeek(seconds)}
                >
                  {stamp}
                </button>{' '}
              </span>
            ) : (
              <span className={styles.stamp}>{stamp} </span>
            )}
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
