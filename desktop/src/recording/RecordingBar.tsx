import { useEffect, useLayoutEffect, useRef, useState } from 'react';
import {
  type Language,
  type RecorderState,
  type Reply,
  recorderDismissNotice,
  recorderDismissStartError,
  recorderPause,
  recorderResume,
  recorderRetry,
  recorderStart,
  recorderStop,
} from '../api';
import { CloseIcon, MicIcon, PauseIcon, PlayIcon, RecordIcon, RetryIcon, StopIcon } from '../icons';
import { attempt } from './attempt';
import { LanguageSelect } from './LanguageSelect';
import { canRetry, noticeText } from './notices';
import styles from './RecordingBar.module.css';
import { useRecorderStatus } from './useRecorderStatus';

function pad(n: number): string {
  return String(n).padStart(2, '0');
}

export function formatElapsed(seconds: number): string {
  const total = Math.max(0, Math.floor(seconds));
  return `${pad(Math.floor(total / 3600))}:${pad(Math.floor((total % 3600) / 60))}:${pad(total % 60)}`;
}

type StateReply = { state: RecorderState };

export function RecordingBar() {
  const { status, error, refresh, setState } = useRecorderStatus();
  const [chosen, setChosen] = useState<Language | null>(null);
  const [message, setMessage] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  // FR-1: a hidden connection error shows again only after a poll succeeded (error back to null).
  const [errorHidden, setErrorHidden] = useState(false);
  const bar = useRef<HTMLElement>(null);
  const focusAfter = useRef<PendingFocus | null>(null);

  const state = status?.state ?? 'idle';
  const idle = state === 'idle';
  const running = state === 'recording' || state === 'paused';
  const language = idle ? (chosen ?? status?.language ?? 'en') : (status?.language ?? 'en');
  const notice = status?.notice ?? null;
  const startError = status?.start_error ?? null;

  // FR-1: once a recording runs, the idle default is the language it used, not an older pick.
  useEffect(() => {
    if (!idle) setChosen(null);
  }, [idle]);

  useEffect(() => {
    if (error === null) setErrorHidden(false);
  }, [error]);

  // FR-3: once the closed line has left the DOM, focus the next ×, else the main record control.
  useLayoutEffect(() => {
    const pending = focusAfter.current;
    if (!pending || pending.button.isConnected || !bar.current) return;
    focusAfter.current = null;
    focusNext(bar.current, pending.index);
  });

  async function run<T>(call: () => Promise<Reply<T>>, onOk: (reply: T) => void): Promise<boolean> {
    setMessage(null);
    setBusy(true);
    const result = await attempt(call);
    setBusy(false);
    if (result.ok) onOk(result.reply);
    else setMessage(result.text);
    return result.ok;
  }

  /** Wraps a ×'s action so focus moves on when its line goes; a failed command keeps the line and focus. */
  const closing = (action: () => Promise<boolean> | undefined) => (button: HTMLButtonElement) => {
    if (!bar.current) return;
    focusAfter.current = { button, index: dismissButtons(bar.current).indexOf(button) };
    const done = action();
    done?.then((ok) => {
      if (!ok && focusAfter.current?.button === button) focusAfter.current = null;
    });
  };

  // Recording at once, so Start can't be pressed again before the next poll; the poll confirms it.
  const start = () =>
    run(
      () => recorderStart({ language, event_occurrence_id: null }),
      () => {
        setState('recording');
        refresh();
      },
    );
  const follow = (reply: StateReply) => setState(reply.state);
  const control = (call: () => Promise<Reply<StateReply>>) => run(call, follow);
  const retry = (folder: string) => run(() => recorderRetry({ folder }), follow);
  // Quick-record FR-5: the poll keeps the alert until it reports the error cleared, so read the status again now.
  const dismissStartError = closing(() => (busy ? Promise.resolve(false) : run(recorderDismissStartError, refresh)));
  // Continue-recording FR-1: the notice (and its Retry) goes in the recorder, so the menu bar and pill follow.
  const dismissNotice = closing(() => (busy ? Promise.resolve(false) : run(recorderDismissNotice, refresh)));
  const hideMessage = closing(() => {
    setMessage(null);
    return undefined;
  });
  const hideError = closing(() => {
    setErrorHidden(true);
    return undefined;
  });

  const noticeIsProblem = notice !== null && (notice.kind === 'error' || notice.kind === 'setup');

  return (
    // tabIndex -1: the last place focus can go after a × when every control is disabled (saving).
    <header ref={bar} className={styles.bar} data-state={state} tabIndex={-1}>
      <h1 className="sr-only">Meeting</h1>

      <div className={styles.deck}>
        <div className={styles.readout}>
          {running ? (
            <>
              <span className={state === 'recording' ? styles.rec : styles.paused}>
                {state === 'recording' ? '● REC' : 'II Paused'}
              </span>
              <span role="timer" aria-label="Recorded time" className={styles.timer}>
                {formatElapsed(status?.elapsed_s ?? 0)}
              </span>
              <Wave live={state === 'recording'} />
            </>
          ) : (
            <span className={styles.idle}>
              <MicIcon />
              {state === 'saving' ? 'Saving…' : 'Ready to record'}
            </span>
          )}
        </div>

        <div className={styles.controls} data-controls="">
          <LanguageSelect
            className={styles.field}
            value={language}
            disabled={!idle || status === null}
            onChange={setChosen}
          />
          {/* Kept while recording, disabled and visually hidden, so it is never pressed twice. */}
          <button
            type="button"
            data-variant="record"
            className={idle ? styles.start : 'sr-only'}
            onClick={start}
            disabled={!idle || status === null || busy}
          >
            <RecordIcon />
            Start recording
          </button>
          {idle ? null : (
            <>
              {state === 'paused' ? (
                <button type="button" onClick={() => control(recorderResume)} disabled={busy}>
                  <PlayIcon size={14} />
                  Resume
                </button>
              ) : (
                <button type="button" onClick={() => control(recorderPause)} disabled={!running || busy}>
                  <PauseIcon size={14} />
                  Pause
                </button>
              )}
              <button
                type="button"
                data-variant="record"
                onClick={() => control(recorderStop)}
                disabled={!running || busy}
              >
                <StopIcon size={14} />
                Stop
              </button>
            </>
          )}
          {canRetry(notice) ? (
            <button type="button" onClick={() => retry(notice.folder)} disabled={busy}>
              <RetryIcon size={14} />
              Retry
            </button>
          ) : null}
        </div>
      </div>

      {startError ? (
        <Message
          role="alert"
          text={startError.message}
          tone={`${styles.problem} ${styles.startError}`}
          onDismiss={dismissStartError}
        />
      ) : null}

      <div role="status" className={styles.messages}>
        {state === 'saving' ? <p className={styles.line}>Saving the recording…</p> : null}
        {message ? <Message text={message} tone={styles.problem} onDismiss={hideMessage} /> : null}
        {error && !errorHidden ? <Message text={error} tone={styles.problem} onDismiss={hideError} /> : null}
        {notice ? (
          <Message
            text={noticeText(notice)}
            tone={noticeIsProblem ? styles.problem : styles.info}
            onDismiss={dismissNotice}
          />
        ) : null}
      </div>
    </header>
  );
}

type PendingFocus = { button: HTMLButtonElement; index: number };

const dismissButtons = (bar: HTMLElement) =>
  Array.from(bar.querySelectorAll<HTMLButtonElement>('button[data-dismiss]'));

/**
 * Focus after a line closed (FR-3): the × now at the closed one's place (the next message), else the
 * last ×, else the main record control (the first enabled control button: Start when idle, else Pause or
 * Resume, else Stop), else the bar itself, so focus never falls to `<body>`.
 */
function focusNext(bar: HTMLElement, index: number) {
  const left = dismissButtons(bar);
  const next = left[Math.min(Math.max(index, 0), left.length - 1)];
  const control = bar.querySelector<HTMLButtonElement>('[data-controls] button:not(:disabled)');
  (next ?? control ?? bar).focus();
}

/** One message line with its × at the end, named "Dismiss message" (continue-recording FR-1, FR-3). */
function Message({
  text,
  tone,
  role,
  onDismiss,
}: {
  text: string;
  tone: string;
  role?: 'alert';
  onDismiss: (button: HTMLButtonElement) => void;
}) {
  return (
    <div role={role} className={`${styles.line} ${styles.message} ${tone}`}>
      <p>{text}</p>
      <button
        type="button"
        className={styles.dismiss}
        aria-label="Dismiss message"
        data-dismiss=""
        onClick={(event) => onDismiss(event.currentTarget)}
      >
        <CloseIcon size={14} />
      </button>
    </div>
  );
}

/** A level-meter look while recording; still when paused. Decorative only. */
function Wave({ live }: { live: boolean }) {
  return (
    <span className={live ? `${styles.wave} ${styles.live}` : styles.wave} aria-hidden="true">
      {Array.from({ length: 14 }, (_, i) => (
        // biome-ignore lint/suspicious/noArrayIndexKey: a fixed row of identical bars.
        <i key={i} />
      ))}
    </span>
  );
}
