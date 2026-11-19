import { useEffect, useState } from 'react';
import type { ArchiveRequest, JobSummary, OpenRequest, RetryRequest, Stage, TrashRequest } from '../shared/native';
import { sendNative } from '../shared/nativeClient';
import styles from './JobsTab.module.css';
import { useJobs } from './useJobs';

const CONFIRM_MS = 3000;

type Tone = 'queued' | 'working' | 'done' | 'failed';

// A Record, so a new stage without a tone fails tsc.
const TONE: Record<Stage, Tone> = {
  queued: 'queued',
  joining: 'working',
  extracting: 'working',
  transcribing: 'working',
  summarising: 'working',
  writing: 'working',
  done: 'done',
  failed: 'failed',
};

const label = (path: string) => {
  const parts = path.split('/');
  const file = parts[parts.length - 1];
  return file.startsWith('recording.') && parts.length > 1 ? parts[parts.length - 2] : file;
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
      {view.kind === 'loading' && <p className={styles.note}>Loading…</p>}
      {view.kind === 'error' && (
        <p role="alert" className={styles.problem}>
          {view.message}
        </p>
      )}
      {view.kind === 'ready' && view.jobs.length === 0 && <p className={styles.note}>No meetings processed yet</p>}
      {view.kind === 'ready' && view.jobs.length > 0 && (
        <ul aria-label="Jobs" className={styles.list}>
          {view.jobs.map((job) => (
            <JobRow key={job.job_id} job={job} run={run} />
          ))}
        </ul>
      )}
      <p role="status" className={styles.status}>
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
      <div className={styles.head}>
        <strong className={styles.title}>{job.title ?? label(job.recording)}</strong>
        {/* An older tool may send a stage this build does not know: show it as working. */}
        <span className={styles.chip} data-tone={TONE[job.stage] ?? 'working'}>
          {job.stage}
        </span>
      </div>
      <span className={styles.meta}>{new Date(job.started_at).toLocaleString()}</span>
      {job.stage === 'failed' && job.error && <span className={styles.error}>{job.error}</span>}
      <div className={styles.actions}>
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
          <button type="button" className={styles.primary} onClick={() => run({ type: 'retry', job_id: job.job_id })}>
            Retry
          </button>
        )}
        {(job.stage === 'done' || job.stage === 'failed') && (
          <>
            <button type="button" onClick={() => run({ type: 'archive', job_id: job.job_id })}>
              Archive
            </button>
            <button type="button" className={armed ? styles.armed : undefined} onClick={trash}>
              {armed ? 'Confirm trash' : 'Trash'}
            </button>
          </>
        )}
      </div>
    </li>
  );
}
