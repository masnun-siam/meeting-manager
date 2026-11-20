import { useEffect, useRef, useState } from 'react';
import type {
  DismissTarget,
  Language,
  MeetingContextState,
  Notice,
  PopupCommand,
  RecorderStatus,
} from '../shared/messages';
import { elapsedMs, initialRecordingState, type StopReason } from '../shared/recordingState';
import styles from './RecordTab.module.css';
import { useMeetingContext } from './useMeetingContext';
import { useRecorderStatus } from './useRecorderStatus';

const LANGUAGES: { value: Language; label: string }[] = [
  { value: 'bn', label: 'বাংলা' },
  { value: 'en', label: 'English' },
];

const STOP_MESSAGES: Partial<Record<StopReason, string>> = {
  cap: 'Stopped at the 3-hour limit.',
  tab_closed: 'Stopped because the recorded tab was closed.',
  capture_ended: 'Stopped because tab capture ended.',
};

const MIC_WARNING = 'Microphone not included: recording tab audio only.';

const CONTEXT_READING = 'Meet: reading participants…';
const CONTEXT_UNAVAILABLE = "Meet details unavailable — names won't be captured. Recording is fine.";
const CONTEXT_RELOAD_HINT = ' Reload the Meet tab next time.';
const CONTEXT_PANEL_HINT = 'open the people panel to capture names';
const CONTEXT_OTHER = 'names are captured on Google Meet only';

function isLanguage(value: unknown): value is Language {
  return LANGUAGES.some((option) => option.value === value);
}

function pad(n: number): string {
  return String(n).padStart(2, '0');
}

/** `H:MM:SS`, e.g. `0:04:01`. */
export function formatElapsed(ms: number): string {
  const total = Math.floor(ms / 1000);
  return `${Math.floor(total / 3600)}:${pad(Math.floor((total % 3600) / 60))}:${pad(total % 60)}`;
}

function noticeText(notice: Notice): string {
  switch (notice.kind) {
    case 'saved':
      return 'Saved, sent for processing';
    case 'too_short':
      return 'Under 10 seconds, so it was saved but not processed.';
    default:
      return notice.message;
  }
}

type Tone = 'info' | 'problem';

/** `send` is the popup-local send error; the others are cleared in the recorder status. */
type LineKind = 'send' | DismissTarget;

interface Line {
  kind: LineKind;
  text: string;
  tone: Tone;
}

function messageLines(status: RecorderStatus | null, sendError: string | null): Line[] {
  const lines: Line[] = [];
  if (sendError) lines.push({ kind: 'send', text: sendError, tone: 'problem' });
  if (status?.notice) {
    lines.push({
      kind: 'notice',
      text: noticeText(status.notice),
      tone: status.notice.kind === 'setup' || status.notice.kind === 'error' ? 'problem' : 'info',
    });
  }
  if (status?.micIncluded === false && !status.micWarningDismissed) {
    lines.push({ kind: 'mic', text: MIC_WARNING, tone: 'info' });
  }
  if (status?.state.phase === 'idle' && status.state.lastStop) {
    const stop = STOP_MESSAGES[status.state.lastStop];
    if (stop) lines.push({ kind: 'stop', text: stop, tone: 'info' });
  }
  return lines;
}

/** The visible context line, and `announce`: state-only text that changes only when the state does. */
function contextLine(context: MeetingContextState): { text: string; announce: string } {
  const title = context.identity.tab_title;
  if (context.identity.platform !== 'meet') {
    const text = title ? `${title} · ${CONTEXT_OTHER}` : CONTEXT_OTHER;
    return { text, announce: text };
  }
  switch (context.capture) {
    case 'reading':
      return { text: CONTEXT_READING, announce: CONTEXT_READING };
    case 'captured': {
      const name = context.meetingTitle ?? title;
      const seen = `${context.participants.length} seen`;
      const text = name ? `${name} · ${seen}` : seen;
      if (context.participants.length === 0) {
        return { text: `${text} · ${CONTEXT_PANEL_HINT}`, announce: `Meet details captured · ${CONTEXT_PANEL_HINT}` };
      }
      return { text, announce: 'Meet details captured' };
    }
    case 'unavailable': {
      // reloadHint is not cleared when capture later recovers, so it only counts while unavailable.
      const text = context.reloadHint
        ? CONTEXT_UNAVAILABLE + CONTEXT_RELOAD_HINT
        : `${CONTEXT_UNAVAILABLE} ${CONTEXT_PANEL_HINT[0].toUpperCase()}${CONTEXT_PANEL_HINT.slice(1)}.`;
      return { text, announce: text };
    }
  }
}

function useStoredLanguage(): [Language, (language: Language) => void] {
  const [language, setLanguage] = useState<Language>('bn');

  useEffect(() => {
    let active = true;
    chrome.storage.local.get('language').then((stored) => {
      if (active && isLanguage(stored.language)) setLanguage(stored.language);
    });
    return () => {
      active = false;
    };
  }, []);

  return [language, setLanguage];
}

interface MessageProps {
  text: string;
  tone: Tone;
  onDismiss: () => void;
  dismissRef: (button: HTMLButtonElement | null) => void;
}

function Message({ text, tone, onDismiss, dismissRef }: MessageProps) {
  return (
    <div className={styles.line}>
      <p className={tone === 'problem' ? styles.problem : styles.info}>{text}</p>
      <button
        ref={dismissRef}
        type="button"
        aria-label="Dismiss message"
        className={styles.dismiss}
        onClick={onDismiss}
      >
        ×
      </button>
    </div>
  );
}

export function RecordTab() {
  const status = useRecorderStatus();
  const context = useMeetingContext();
  const [language, setLanguage] = useStoredLanguage();
  const [sendError, setSendError] = useState<string | null>(null);
  const [now, setNow] = useState(() => Date.now());

  const state = status?.state ?? initialRecordingState;
  const { phase } = state;

  useEffect(() => {
    if (phase !== 'recording') return;
    setNow(Date.now());
    const id = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(id);
  }, [phase]);

  /** Sends without clearing a shown send error; true when it reached the service worker. */
  async function post(command: PopupCommand): Promise<boolean> {
    try {
      if (command.type === 'start') await chrome.storage.local.set({ language: command.language });
      await chrome.runtime.sendMessage(command);
      return true;
    } catch (error) {
      setSendError(`Could not reach the recorder: ${error instanceof Error ? error.message : String(error)}`);
      return false;
    }
  }

  async function send(command: PopupCommand) {
    pendingFocus.current = null;
    setSendError(null);
    await post(command);
  }

  const lines = messageLines(status, sendError);
  const running = phase === 'recording' || phase === 'paused';

  const startRef = useRef<HTMLButtonElement>(null);
  const pauseResumeRef = useRef<HTMLButtonElement>(null);
  const dismissRefs = useRef(new Map<LineKind, HTMLButtonElement>());
  /** The closed line and its index; focus moves once that line is gone. Any other command drops it. */
  const pendingFocus = useRef<{ kind: LineKind; index: number } | null>(null);

  // After every render: a status-backed line disappears only when storage changes, so focus cannot move in the click.
  useEffect(() => {
    const pending = pendingFocus.current;
    if (!pending || lines.some((message) => message.kind === pending.kind)) return;
    pendingFocus.current = null;
    if (lines.length > 0) {
      dismissRefs.current.get(lines[Math.min(pending.index, lines.length - 1)].kind)?.focus();
      return;
    }
    // The main record control: Start when idle, Pause or Resume while running, the Record tab while saving.
    const main = startRef.current ?? pauseResumeRef.current ?? document.getElementById('tab-record');
    main?.focus();
  });

  async function dismiss(kind: LineKind, index: number) {
    pendingFocus.current = { kind, index };
    if (kind === 'send') {
      setSendError(null);
      return;
    }
    if (!(await post({ type: 'dismiss', what: kind }))) pendingFocus.current = null;
  }
  const line = running && context ? contextLine(context) : null;

  const shownLanguage = phase === 'idle' ? language : (status?.language ?? language);

  return (
    <div className={styles.tab}>
      <div className={styles.stage}>
        {running ? (
          <>
            <p className={phase === 'recording' ? styles.rec : styles.paused}>
              {phase === 'recording' ? 'REC' : 'Paused'}
            </p>
            <p role="timer" aria-label="Recorded time" className={styles.timer}>
              {formatElapsed(elapsedMs(state, now))}
            </p>
          </>
        ) : null}
        {phase === 'idle' ? (
          <p aria-hidden="true" className={`${styles.timer} ${styles.dim}`}>
            {formatElapsed(0)}
          </p>
        ) : null}
        {phase === 'saving' ? <p className={styles.saving}>Saving…</p> : null}

        {phase === 'idle' ? (
          <button
            type="button"
            ref={startRef}
            aria-label="Start recording"
            className={styles.record}
            onClick={() => send({ type: 'start', language })}
          />
        ) : null}
        {running ? (
          <div className={styles.actions}>
            {phase === 'recording' ? (
              <button
                ref={pauseResumeRef}
                type="button"
                className={styles.secondary}
                onClick={() => send({ type: 'pause' })}
              >
                Pause
              </button>
            ) : (
              <button
                ref={pauseResumeRef}
                type="button"
                className={styles.secondary}
                onClick={() => send({ type: 'resume' })}
              >
                Resume
              </button>
            )}
            <button type="button" className={styles.stop} onClick={() => send({ type: 'stop' })}>
              Stop & save
            </button>
          </div>
        ) : null}

        <div role="radiogroup" aria-label="Language" className={styles.language}>
          {LANGUAGES.map(({ value, label }) => (
            <label key={value} className={styles.choice} lang={value}>
              <input
                type="radio"
                name="language"
                value={value}
                checked={shownLanguage === value}
                disabled={phase !== 'idle'}
                onChange={() => setLanguage(value)}
              />
              {label}
            </label>
          ))}
        </div>
      </div>

      <div role="status" className={styles.messages}>
        {line ? (
          <>
            <p aria-hidden="true">{line.text}</p>
            <span className={styles.srOnly}>{line.announce}</span>
          </>
        ) : null}
        {lines.map((message, index) => (
          <Message
            key={message.kind}
            text={message.text}
            tone={message.tone}
            onDismiss={() => dismiss(message.kind, index)}
            dismissRef={(button) => {
              if (button) dismissRefs.current.set(message.kind, button);
              else dismissRefs.current.delete(message.kind);
            }}
          />
        ))}
      </div>
    </div>
  );
}
