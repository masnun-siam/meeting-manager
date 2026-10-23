import { useEffect, useState } from 'react';
import type { Language, MeetingContextState, Notice, PopupCommand, RecorderStatus } from '../shared/messages';
import { elapsedMs, initialRecordingState, type StopReason } from '../shared/recordingState';
import styles from './RecordTab.module.css';
import { useMeetingContext } from './useMeetingContext';
import { useRecorderStatus } from './useRecorderStatus';

const LANGUAGES: { value: Language; label: string }[] = [
  { value: 'bn', label: 'Bengali' },
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

function formatElapsed(ms: number): string {
  const total = Math.floor(ms / 1000);
  return `${pad(Math.floor(total / 3600))}:${pad(Math.floor((total % 3600) / 60))}:${pad(total % 60)}`;
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

interface Line {
  text: string;
  problem: boolean;
}

function messageLines(status: RecorderStatus | null, sendError: string | null): Line[] {
  const lines: Line[] = [];
  if (sendError) lines.push({ text: sendError, problem: true });
  if (status?.notice) {
    lines.push({
      text: noticeText(status.notice),
      problem: status.notice.kind === 'setup' || status.notice.kind === 'error',
    });
  }
  if (status?.micIncluded === false) lines.push({ text: MIC_WARNING, problem: false });
  if (status?.state.phase === 'idle' && status.state.lastStop) {
    const stop = STOP_MESSAGES[status.state.lastStop];
    if (stop) lines.push({ text: stop, problem: false });
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

  async function send(command: PopupCommand) {
    setSendError(null);
    try {
      if (command.type === 'start') await chrome.storage.local.set({ language: command.language });
      await chrome.runtime.sendMessage(command);
    } catch (error) {
      setSendError(`Could not reach the recorder: ${error instanceof Error ? error.message : String(error)}`);
    }
  }

  const lines = messageLines(status, sendError);
  const running = phase === 'recording' || phase === 'paused';
  const line = running && context ? contextLine(context) : null;

  return (
    <div className={styles.tab}>
      <label className={styles.row}>
        Language
        <select
          value={phase === 'idle' ? language : (status?.language ?? language)}
          disabled={phase !== 'idle'}
          onChange={(event) => isLanguage(event.target.value) && setLanguage(event.target.value)}
        >
          {LANGUAGES.map(({ value, label }) => (
            <option key={value} value={value}>
              {label}
            </option>
          ))}
        </select>
      </label>

      {phase === 'recording' || phase === 'paused' ? (
        <p role="timer" aria-label="Recorded time" className={styles.timer}>
          <span className={phase === 'recording' ? styles.rec : undefined}>
            {phase === 'recording' ? '● REC' : 'II Paused'}
          </span>{' '}
          {formatElapsed(elapsedMs(state, now))}
        </p>
      ) : null}
      {phase === 'saving' ? <p>Saving…</p> : null}

      <div className={styles.row}>
        {phase === 'idle' ? (
          <button type="button" onClick={() => send({ type: 'start', language })}>
            Start
          </button>
        ) : null}
        {phase === 'recording' ? (
          <button type="button" onClick={() => send({ type: 'pause' })}>
            Pause
          </button>
        ) : null}
        {phase === 'paused' ? (
          <button type="button" onClick={() => send({ type: 'resume' })}>
            Resume
          </button>
        ) : null}
        {phase === 'recording' || phase === 'paused' ? (
          <button type="button" onClick={() => send({ type: 'stop' })}>
            Stop & save
          </button>
        ) : null}
      </div>

      <div role="status" className={styles.messages}>
        {line ? (
          <>
            <p aria-hidden="true">{line.text}</p>
            <span className={styles.srOnly}>{line.announce}</span>
          </>
        ) : null}
        {lines.map(({ text, problem }) => (
          <p key={text} className={problem ? styles.problem : undefined}>
            {text}
          </p>
        ))}
      </div>
    </div>
  );
}
