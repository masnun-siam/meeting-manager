import { useEffect, useState } from 'react';
import type { ArchiveRequest, JobSummary, OpenRequest, RetryRequest, TrashRequest } from '../shared/native';
import { sendNative } from '../shared/nativeClient';
import styles from './JobsTab.module.css';
import { useJobs } from './useJobs';

const RECORDING_FILE = 'recording.webm';
const CONFIRM_MS = 3000;

const label = (path: string) => {
  const parts = path.split('/');
  const file = parts[parts.length - 1];
  return file === RECORDING_FILE && parts.length > 1 ? parts[parts.length - 2] : file;
};

type JobAction = OpenRequest | RetryRequest | ArchiveRequest | TrashRequest;

export function JobsTab() {
  const { view, refresh } = useJobs();
  const [status, setStatus] = useState<string | null>(null);

  async function run(request: JobAction) {
    setStatus(null);
    try {
      const reply = await sendNative(request);
      if (!reply.ok) setStatus(reply.message);
      else if (request.type !== 'open') refresh();
    } catch (error) {
      setStatus(`Could not reach the meeting tool: ${error instanceof Error ? error.message : String(error)}`);
    }
  }

  return (
    <div className={styles.tab}>
      {view.kind === 'loading' && <p>Loading…</p>}
      {view.kind === 'error' && (
        <p role="alert" className={styles.problem}>
          {view.message}
        </p>
      )}
      {view.kind === 'ready' && view.jobs.length === 0 && <p>No meetings processed yet</p>}
      {view.kind === 'ready' && view.jobs.length > 0 && (
        <ul aria-label="Jobs" className={styles.list}>
          {view.jobs.map((job) => (
            <JobRow key={job.job_id} job={job} run={run} />
          ))}
        </ul>
      )}
      <p role="status" className={styles.problem}>
        {status}
      </p>
    </div>
  );
}

function JobRow({ job, run }: { job: JobSummary; run: (request: JobAction) => void }) {
  const [armed, setArmed] = useState(false);

  useEffect(() => {
    if (!armed) return;
    const timer = setTimeout(() => setArmed(false), CONFIRM_MS);
    return () => clearTimeout(timer);
  }, [armed]);

  function trash() {
    setArmed(!armed);
    if (armed) run({ type: 'trash', job_id: job.job_id });
  }

  return (
    <li className={styles.job}>
      <strong>{job.title ?? label(job.recording)}</strong>
      <span className={styles.meta}>
        {new Date(job.started_at).toLocaleString()} · {job.stage}
      </span>
      {job.stage === 'failed' && job.error && <span className={styles.problem}>{job.error}</span>}
      <div className={styles.row}>
        {job.stage === 'done' && job.summary_path !== null && (
          <button type="button" onClick={() => run({ type: 'open', job_id: job.job_id, what: 'summary' })}>
            Open summary
          </button>
        )}
        {job.stage === 'done' && (
          <button type="button" onClick={() => run({ type: 'open', job_id: job.job_id, what: 'folder' })}>
            Open folder
          </button>
        )}
        {job.stage === 'failed' && (
          <button type="button" onClick={() => run({ type: 'retry', job_id: job.job_id })}>
            Retry
          </button>
        )}
        {(job.stage === 'done' || job.stage === 'failed') && (
          <>
            <button type="button" onClick={() => run({ type: 'archive', job_id: job.job_id })}>
              Archive
            </button>
            <button type="button" onClick={trash}>
              {armed ? 'Confirm trash' : 'Trash'}
            </button>
          </>
        )}
      </div>
    </li>
  );
}
