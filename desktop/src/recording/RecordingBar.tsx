import { useState } from 'react';
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

  async function run<T>(call: () => Promise<Reply<T>>, onOk: (reply: T) => void) {
    setMessage(null);
    setBusy(true);
    const result = await attempt(call);
    setBusy(false);
    if (result.ok) onOk(result.reply);
    else setMessage(result.text);
  }

  const start = () => run(() => recorderStart({ language, event_occurrence_id: null }), refresh);
  const follow = (reply: StateReply) => setState(reply.state);
  const control = (call: () => Promise<Reply<StateReply>>) => run(call, follow);
  const retry = (folder: string) => run(() => recorderRetry({ folder }), follow);

  return (
    <header className={styles.bar}>
      <h1 className={styles.title}>Meeting</h1>

      <LanguageSelect
        className={styles.field}
        value={language}
        disabled={!idle || status === null}
        onChange={setChosen}
      />

      {running ? (
        <p className={styles.timer}>
          <span className={state === 'recording' ? styles.rec : styles.paused}>
            {state === 'recording' ? '● REC' : 'II Paused'}
          </span>{' '}
          <span role="timer" aria-label="Recorded time">
            {formatElapsed(status?.elapsed_s ?? 0)}
          </span>
        </p>
      ) : null}

      <div className={styles.controls}>
        <button type="button" onClick={start} disabled={!idle || status === null || busy}>
          Start recording
        </button>
        {idle ? null : (
          <>
            {state === 'paused' ? (
              <button type="button" onClick={() => control(recorderResume)} disabled={busy}>
                Resume
              </button>
            ) : (
              <button type="button" onClick={() => control(recorderPause)} disabled={!running || busy}>
                Pause
              </button>
            )}
            <button type="button" onClick={() => control(recorderStop)} disabled={!running || busy}>
              Stop
            </button>
          </>
        )}
        {canRetry(notice) ? (
          <button type="button" onClick={() => retry(notice.folder)} disabled={busy}>
            Retry
          </button>
        ) : null}
      </div>

      <div role="status" className={styles.messages}>
        {state === 'saving' ? <p>Saving the recording…</p> : null}
        {message ? <p className={styles.problem}>{message}</p> : null}
        {error ? <p className={styles.problem}>{error}</p> : null}
        {notice ? <p className={notice.kind === 'error' ? styles.problem : undefined}>{noticeText(notice)}</p> : null}
      </div>
    </header>
  );
}
