import { useState } from 'react';
import type { JobSummary, OpenRequest, RetryRequest } from '../shared/native';
import { sendNative } from '../shared/nativeClient';
import styles from './JobsTab.module.css';
import { useJobs } from './useJobs';

const fileName = (path: string) => path.slice(path.lastIndexOf('/') + 1);

export function JobsTab() {
  const { view, refresh } = useJobs();
  const [status, setStatus] = useState<string | null>(null);

  async function run(
    request: { type: 'open'; job_id: string; what: 'summary' | 'folder' } | { type: 'retry'; job_id: string },
  ) {
    setStatus(null);
    try {
      const reply = await sendNative(request);
      if (!reply.ok) setStatus(reply.message);
      else if (request.type === 'retry') refresh();
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

function JobRow({ job, run }: { job: JobSummary; run: (request: OpenRequest | RetryRequest) => void }) {
  return (
    <li className={styles.job}>
      <strong>{job.title ?? fileName(job.recording)}</strong>
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
      </div>
    </li>
  );
}
