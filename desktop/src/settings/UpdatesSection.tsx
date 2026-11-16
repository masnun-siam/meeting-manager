import { useEffect, useRef, useState } from 'react';
import { setAutoUpdate, type UpdateStatus, updateCheck, updateInstall } from '../api';
import { RetryIcon } from '../icons';
import { attempt } from '../recording/attempt';
import { useRecorderStatus } from '../recording/useRecorderStatus';
import styles from './Settings.module.css';
import { isBusy, useUpdateStatus } from './useUpdateStatus';

export const FINISH_RECORDING = 'Finish the recording to install';

/** A check this view started has ended with nothing new and no error. */
const upToDate = (status: UpdateStatus) =>
  status.state === 'idle' && status.available === null && status.error === null;

/**
 * FR-25: the app's version, Check for updates, Install updates automatically, the release found with Install and
 * relaunch, and download progress; FR-27: the tool-swap error. The Rust side writes every error sentence (`error`,
 * `tool_error`, error replies); this view shows them verbatim.
 */
export function UpdatesSection() {
  const { status, problem: readProblem, reload } = useUpdateStatus();
  const recorder = useRecorderStatus().status;
  const [starting, setStarting] = useState(false);
  // From a check pressed here until its status is seen ending: only then may "Meeting is up to date" show.
  const [watching, setWatching] = useState(false);
  const [finishedHere, setFinishedHere] = useState(false);
  const [installing, setInstalling] = useState(false);
  const [problem, setProblem] = useState<string | null>(null);
  const active = useRef(true);

  useEffect(() => {
    active.current = true;
    return () => {
      active.current = false;
    };
  }, []);

  useEffect(() => {
    if (!watching || status === null || status.state === 'checking') return;
    setWatching(false);
    setFinishedHere(upToDate(status));
  }, [watching, status]);

  async function check() {
    setStarting(true);
    setFinishedHere(false);
    setProblem(null);
    const reply = await attempt(updateCheck);
    if (!active.current) return;
    if (!reply.ok) {
      setStarting(false);
      setProblem(reply.text);
      return;
    }
    // A status read after the check started: `checking` while it runs, or its result if it already ended.
    const next = await reload();
    if (!active.current) return;
    setStarting(false);
    if (next !== null) setWatching(true);
  }

  async function install() {
    setInstalling(true);
    setFinishedHere(false);
    setProblem(null);
    const reply = await attempt(updateInstall);
    if (!active.current) return;
    if (!reply.ok) setProblem(reply.text);
    await reload();
    if (active.current) setInstalling(false);
  }

  const state = status?.state;
  const recording = recorder !== null && recorder.state !== 'idle';

  // A reply such as "Finish the recording to install" is stale once the recording ends.
  useEffect(() => {
    if (!recording) setProblem(null);
  }, [recording]);
  const checkingText = starting || watching || state === 'checking';
  const progressText =
    state === 'downloading'
      ? `Downloading… ${status?.progress ?? 0}%`
      : checkingText
        ? 'Checking…'
        : finishedHere
          ? 'Meeting is up to date'
          : null;

  return (
    <section aria-labelledby="settings-updates" className={styles.section}>
      <h3 id="settings-updates">Updates</h3>
      {status !== null && <p>Version {status.version}</p>}
      <AutoUpdateBox initial={status?.auto_update ?? null} />
      <div className={styles.actionRow}>
        <button type="button" disabled={status === null || starting || isBusy(state)} onClick={check}>
          <RetryIcon size={14} />
          Check for updates
        </button>
      </div>
      {status?.available && (
        <div className={styles.callout}>
          <p className={styles.release}>Version {status.available.version} is available</p>
          {status.available.notes && <p className={styles.notes}>{status.available.notes}</p>}
          <button
            type="button"
            data-variant="primary"
            disabled={recording || installing || state !== 'idle'}
            aria-describedby={recording ? 'updates-finish-recording' : undefined}
            onClick={install}
          >
            Install and relaunch
          </button>
          {recording && <p id="updates-finish-recording">{FINISH_RECORDING}</p>}
        </div>
      )}
      <p role="status" className={styles.muted}>
        {progressText}
      </p>
      <Alert text={readProblem} />
      <Alert text={problem} />
      <Alert text={status?.error ?? null} />
      <Alert text={status?.tool_error ?? null} />
    </section>
  );
}

function Alert({ text }: { text: string | null }) {
  return (
    text !== null && (
      <p role="alert" className={styles.problem}>
        {text}
      </p>
    )
  );
}

/**
 * Install updates automatically: shows a toggle at once, saves the latest one, and on a failed save returns to the
 * last value the app confirmed with the reason. `initial` is `update_status`'s `auto_update`; once toggled here, the box
 * shows this view's value.
 */
function AutoUpdateBox({ initial }: { initial: boolean | null }) {
  // Null until toggled here: until then the box follows `initial`.
  const [own, setOwn] = useState<boolean | null>(null);
  const [problem, setProblem] = useState<string | null>(null);
  const wanted = useRef<boolean | null>(null);
  const confirmed = useRef<boolean | null>(null);
  const queue = useRef<Promise<void>>(Promise.resolve());
  const latest = useRef(0);
  const active = useRef(true);

  useEffect(() => {
    active.current = true;
    return () => {
      active.current = false;
    };
  }, []);

  const show = (value: boolean) => {
    wanted.current = value;
    setOwn(value);
  };

  function toggle() {
    const next = !(wanted.current ?? initial);
    show(next);
    setProblem(null);
    const request = ++latest.current;
    queue.current = queue.current.then(async () => {
      const reply = await attempt(() => setAutoUpdate({ enabled: next }));
      if (!active.current) return;
      if (reply.ok) confirmed.current = reply.reply.auto_update;
      if (request !== latest.current) return;
      if (reply.ok) {
        show(reply.reply.auto_update);
      } else {
        show(confirmed.current ?? initial === true);
        setProblem(reply.text);
      }
    });
  }

  return (
    <>
      <label className={styles.toggle}>
        <input type="checkbox" checked={own ?? initial === true} disabled={initial === null} onChange={toggle} />{' '}
        Install updates automatically
      </label>
      <p role="status" className={styles.problem}>
        {problem}
      </p>
    </>
  );
}
