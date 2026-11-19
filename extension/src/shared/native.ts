// Native-messaging contract with the `meeting` host. Field names are snake_case, as on the wire.
import type { Language } from './messages';

export const NATIVE_HOST = 'com.meeting_manager.host';

export type Stage =
  | 'queued'
  | 'joining'
  | 'extracting'
  | 'transcribing'
  | 'summarising'
  | 'writing'
  | 'done'
  | 'failed';

export interface JobSummary {
  job_id: string;
  recording: string;
  title: string | null;
  stage: Stage;
  error: string | null;
  started_at: string;
  updated_at: string;
  summary_path: string | null;
}

export interface HealthCheck {
  name: string;
  ok: boolean;
  fix: string;
}

/** `context.json` on the wire; the host rejects any other key. */
export interface ContextPayload {
  version: 1;
  platform: 'meet' | 'zoom' | 'teams' | 'other';
  url: string | null;
  meeting_id: string | null;
  tab_title: string | null;
  meeting_title: string | null;
  started_at_ms: number;
  utc_offset_min: number;
  time_zone: string | null;
  participants: { name: string; self: boolean }[];
  speaking: { from: string; to: string; names: string[] }[];
  truncated: string[];
}

export type ContextIdentity = Pick<
  ContextPayload,
  'platform' | 'url' | 'meeting_id' | 'tab_title' | 'started_at_ms' | 'utc_offset_min' | 'time_zone'
>;

export interface ProcessRequest {
  type: 'process';
  path: string;
  language: Language;
  context?: ContextPayload;
}

/** Hands a saved `recording-part-N.<ext>` to the tool, which joins it onto its meeting. */
export interface ProcessPartRequest {
  type: 'process_part';
  path: string;
  language: Language;
}

export interface RetryRequest {
  type: 'retry';
  job_id: string;
}

export interface ListJobsRequest {
  type: 'list_jobs';
  limit: number;
}

export interface OpenRequest {
  type: 'open';
  job_id: string;
  what: 'summary' | 'folder';
}

export interface ArchiveRequest {
  type: 'archive';
  job_id: string;
}

export interface TrashRequest {
  type: 'trash';
  job_id: string;
}

export interface HealthRequest {
  type: 'health';
}

export type NativeRequest =
  | ProcessRequest
  | ProcessPartRequest
  | RetryRequest
  | ListJobsRequest
  | OpenRequest
  | ArchiveRequest
  | TrashRequest
  | HealthRequest;

export type NativeErrorCode =
  | 'not_configured'
  | 'invalid_path'
  | 'already_processing'
  | 'unknown_job'
  | 'missing_file'
  | 'archive_failed'
  | 'trash_failed'
  | 'bad_request';

export interface NativeError {
  ok: false;
  code: NativeErrorCode;
  message: string;
}

/** Chrome could not find the host: `meeting install` has not been run (FR-10). Not a wire code. */
export interface HostMissing {
  ok: false;
  code: 'host_missing';
  message: string;
}

export interface ProcessSuccess {
  ok: true;
  job_id: string;
}

/** `job_id` is the new job's id, not the retried one's. */
export interface RetrySuccess {
  ok: true;
  job_id: string;
}

export interface ListJobsSuccess {
  ok: true;
  jobs: JobSummary[];
}

export interface OpenSuccess {
  ok: true;
}

export interface HealthSuccess {
  ok: true;
  checks: HealthCheck[];
}

interface NativeSuccessByType {
  process: ProcessSuccess;
  process_part: ProcessSuccess;
  retry: RetrySuccess;
  list_jobs: ListJobsSuccess;
  open: OpenSuccess;
  archive: OpenSuccess;
  trash: OpenSuccess;
  health: HealthSuccess;
}

export type NativeResponse<R extends NativeRequest> = NativeSuccessByType[R['type']] | NativeError;
