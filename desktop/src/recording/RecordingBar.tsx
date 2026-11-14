import { useEffect, useState } from 'react';
import {
  type Language,
  type RecorderState,
  type Reply,
  recorderPause,
  recorderResume,
  recorderRetry,
  recorderStart,
  recorderStop,
} from '../api';
import { MicIcon, PauseIcon, PlayIcon, RecordIcon, RetryIcon, StopIcon } from '../icons';
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

  const state = status?.state ?? 'idle';
  const idle = state === 'idle';
  const running = state === 'recording' || state === 'paused';
  const language = idle ? (chosen ?? status?.language ?? 'en') : (status?.language ?? 'en');
  const notice = status?.notice ?? null;

  // FR-1: once a recording runs, the idle default is the language it used, not an older pick.
  useEffect(() => {
    if (!idle) setChosen(null);
  }, [idle]);

  async function run<T>(call: () => Promise<Reply<T>>, onOk: (reply: T) => void) {
    setMessage(null);
    setBusy(true);
    const result = await attempt(call);
    setBusy(false);
    if (result.ok) onOk(result.reply);
    else setMessage(result.text);
  }

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

  const noticeIsProblem = notice !== null && (notice.kind === 'error' || notice.kind === 'setup');

  return (
    <header className={styles.bar} data-state={state}>
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

        <div className={styles.controls}>
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

      <div role="status" className={styles.messages}>
        {state === 'saving' ? <p className={styles.line}>Saving the recording…</p> : null}
        {message ? <p className={`${styles.line} ${styles.problem}`}>{message}</p> : null}
        {error ? <p className={`${styles.line} ${styles.problem}`}>{error}</p> : null}
        {notice ? (
          <p className={`${styles.line} ${noticeIsProblem ? styles.problem : styles.info}`}>{noticeText(notice)}</p>
        ) : null}
      </div>
    </header>
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
