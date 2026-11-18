import { useEffect, useRef, useState } from 'react';
import {
  indicatorDismiss,
  type Notice,
  type RecorderState,
  type Reply,
  recorderPause,
  recorderResume,
  recorderStop,
  windowOpenMain,
} from '../api';
import { PauseIcon, PlayIcon, StopIcon } from '../icons';
import { formatClock } from '../notes/view/format';
import { attempt } from '../recording/attempt';
import { noticeText } from '../recording/notices';
import { useRecorderStatus } from '../recording/useRecorderStatus';
import styles from './Pill.module.css';

/** How long a `saved`/`too_short` outcome and an error reply stay in the pill (FR-8). */
export const SHOW_MS = 4000;

/** A click that raced the recorder: the next poll shows what really happened. */
const IGNORED = new Set(['not_recording', 'already_recording']);

/** `setup` and `error` wait for Open Meeting or Dismiss; every other notice hides on its own. */
const stays = (notice: Notice) => notice.kind === 'setup' || notice.kind === 'error';

type StateReply = { state: RecorderState };

/** The floating indicator's content (quick-record FR-7, FR-8). The Rust side shows and hides its window. */
export function Pill() {
  const { status, setState } = useRecorderStatus();
  const [outcome, setOutcome] = useState<Notice | null>(null);
  // An object, so the same text twice restarts its timer.
  const [problem, setProblem] = useState<{ text: string } | null>(null);
  const [busy, setBusy] = useState(false);
  const sawSession = useRef(false);

  const state = status?.state ?? 'idle';
  const notice = status?.notice ?? null;

  // The outcome is the notice of the first idle status after this page saw a session; an older notice
  // (one already there when the page loaded) is never shown. A new session replaces the outcome.
  useEffect(() => {
    if (state !== 'idle') {
      sawSession.current = true;
      setOutcome(null);
    } else if (sawSession.current && notice) {
      sawSession.current = false;
      setOutcome(notice);
    }
  }, [state, notice]);

  /** Runs a command; an error reply shows for 4 s unless it is ignored. Resolves to whether it succeeded. */
  async function run<T>(call: () => Promise<Reply<T>>, onOk?: (reply: T) => void): Promise<boolean> {
    setBusy(true);
    const result = await attempt(call);
    setBusy(false);
    if (result.ok) onOk?.(result.reply);
    else if (result.code === null || !IGNORED.has(result.code)) setProblem({ text: result.text });
    return result.ok;
  }

  const dismiss = () => {
    setOutcome(null);
    return run(indicatorDismiss);
  };
  const openMain = () => run(windowOpenMain);
  const control = (call: () => Promise<Reply<StateReply>>) => run(call, (reply) => setState(reply.state));

  useEffect(() => {
    if (!outcome || stays(outcome)) return;
    const timer = setTimeout(() => {
      setOutcome(null);
      attempt(indicatorDismiss).then((result) => {
        if (!result.ok) setProblem({ text: result.text });
      });
    }, SHOW_MS);
    return () => clearTimeout(timer);
  }, [outcome]);

  useEffect(() => {
    if (!problem) return;
    const timer = setTimeout(() => setProblem(null), SHOW_MS);
    return () => clearTimeout(timer);
  }, [problem]);

  let view: 'outcome' | 'live' | 'saving' | null = null;
  if (outcome) view = 'outcome';
  else if (state === 'recording' || state === 'paused') view = 'live';
  else if (state === 'saving') view = 'saving';

  if (view === null && problem === null) return null;

  return (
    // The whole pill moves the window; its buttons do not (Tauri skips clickable elements).
    <div className={styles.pill} data-state={view === 'live' ? state : view} data-tauri-drag-region="deep">
      {problem ? (
        // In place of the dot and time for its 4 s, so a narrow pill can still show it.
        <p role="alert" className={`${styles.text} ${styles.bad}`} title={problem.text}>
          {problem.text}
        </p>
      ) : null}

      {view === 'live' ? (
        <>
          {problem ? null : (
            <>
              <span className={styles.dot} aria-hidden="true" />
              {state === 'paused' ? <span className={styles.label}>Paused</span> : null}
              <button type="button" className={styles.time} aria-label="Open Meeting" onClick={openMain}>
                <span role="timer" aria-label="Recorded time">
                  {formatClock(status?.elapsed_s ?? 0)}
                </span>
              </button>
            </>
          )}
          <span className={styles.controls}>
            {state === 'paused' ? (
              <button
                type="button"
                className={styles.icon}
                aria-label="Resume recording"
                title="Resume recording"
                onClick={() => control(recorderResume)}
                disabled={busy}
              >
                <PlayIcon size={14} />
              </button>
            ) : (
              <button
                type="button"
                className={styles.icon}
                aria-label="Pause recording"
                title="Pause recording"
                onClick={() => control(recorderPause)}
                disabled={busy}
              >
                <PauseIcon size={14} />
              </button>
            )}
            <button
              type="button"
              data-variant="record"
              className={styles.icon}
              aria-label="Stop recording"
              title="Stop recording"
              onClick={() => control(recorderStop)}
              disabled={busy}
            >
              <StopIcon size={14} />
            </button>
          </span>
        </>
      ) : null}

      {view === 'saving' ? (
        <>
          <span className={styles.dot} aria-hidden="true" />
          <p className={styles.text}>Saving…</p>
        </>
      ) : null}

      {view === 'outcome' && outcome ? (
        <>
          <p
            role="status"
            className={stays(outcome) ? `${styles.text} ${styles.bad}` : styles.text}
            title={noticeText(outcome)}
          >
            {noticeText(outcome)}
          </p>
          {stays(outcome) ? (
            <span className={styles.controls}>
              <button
                type="button"
                data-variant="plain"
                onClick={async () => {
                  // If the main window did not open, the pill stays to say why.
                  if (await openMain()) await dismiss();
                }}
              >
                Open Meeting
              </button>
              <button type="button" data-variant="plain" onClick={dismiss}>
                Dismiss
              </button>
            </span>
          ) : null}
        </>
      ) : null}
    </div>
  );
}
