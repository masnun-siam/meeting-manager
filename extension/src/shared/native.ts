// Native-messaging contract with the `meeting` host. Field names are snake_case, as on the wire.
import type { Language } from './messages';

export const NATIVE_HOST = 'com.meeting_manager.host';

export type Stage = 'queued' | 'extracting' | 'transcribing' | 'summarising' | 'writing' | 'done' | 'failed';

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

export interface ProcessRequest {
  type: 'process';
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

export interface HealthRequest {
  type: 'health';
}

export type NativeRequest = ProcessRequest | RetryRequest | ListJobsRequest | OpenRequest | HealthRequest;

export type NativeErrorCode = 'not_configured' | 'invalid_path' | 'already_processing' | 'unknown_job' | 'missing_file';

export interface NativeError {
  ok: false;
  code: NativeErrorCode;
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
  retry: RetrySuccess;
  list_jobs: ListJobsSuccess;
  open: OpenSuccess;
  health: HealthSuccess;
}

export type NativeResponse<R extends NativeRequest> = NativeSuccessByType[R['type']] | NativeError;
