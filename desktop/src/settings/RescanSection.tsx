import { useCallback, useEffect, useRef, useState } from 'react';
import { rescan } from '../api';
import { useRescanProgress } from '../notes/list/useRescanProgress';
import { attempt } from '../recording/attempt';
import styles from './Settings.module.css';

/**
 * FR-14's Rescan folders: progress as "Loading meetings… N of M" while a run (this one or the first launch's backfill)
 * goes on; not cancellable. `storeProblem` is the reason the notes database can't be opened, shown above the button
 * that rebuilds it (D33).
 */
export function RescanSection({ storeProblem, onFinished }: { storeProblem: string | null; onFinished: () => void }) {
  const [starting, setStarting] = useState(false);
  const [finished, setFinished] = useState(false);
  const [problem, setProblem] = useState<string | null>(null);
  const finish = useCallback(() => {
    setFinished(true);
    onFinished();
  }, [onFinished]);
  const { progress, watch } = useRescanProgress(finish);
  const running = progress?.running === true;
  const active = useRef(true);

  useEffect(() => {
    active.current = true;
    return () => {
      active.current = false;
    };
  }, []);

  async function rescanFolders() {
    setStarting(true);
    setProblem(null);
    setFinished(false);
    const reply = await attempt(rescan);
    if (!active.current) return;
    setStarting(false);
    if (reply.ok) watch();
    else setProblem(reply.text);
  }

  return (
    <section aria-labelledby="settings-rescan" className={styles.section}>
      <h3 id="settings-rescan">Meetings</h3>
      {storeProblem !== null && <p role="alert">Notes database can't be opened: {storeProblem}</p>}
      <button type="button" disabled={running || starting} onClick={rescanFolders}>
        Rescan folders
      </button>
      <p role="status">
        {progress?.running ? `Loading meetings… ${progress.done} of ${progress.total}` : finished && 'Rescan finished'}
      </p>
      {problem !== null && (
        <p role="alert" className={styles.problem}>
          {problem}
        </p>
      )}
    </section>
  );
}
