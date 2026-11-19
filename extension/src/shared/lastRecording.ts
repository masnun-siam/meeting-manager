// The last recording this extension saved, kept in chrome.storage.local so it survives a browser restart,
// and the pure rule saying whether it can still be continued. Every helper does nothing with the flag off.

import { continueRecordingEnabled } from './flags';
import type { Language } from './messages';
import { RECORDING_CAP_MS, type RecordingState, type StopReason } from './recordingState';

export const LAST_RECORDING_KEY = 'last_recording';

export const CONTINUE_WINDOW_MS = 2 * 60 * 60 * 1000;

export interface LastRecording {
  download_id: number;
  /** Absolute path of `recording.webm`, kept for every later part. */
  path: string;
  /** Meeting folder name, the second-to-last segment of `path`. */
  folder: string;
  ext: 'webm';
  recorded_ms: number;
  language: Language;
  /** Epoch ms of the stop, or of the recovery for a recovered recording. */
  stopped_at: number;
  /** `null` for a recovered recording, which no stop button ended. */
  stop_reason: StopReason | null;
  parts: number;
}

/** Pure: no Chrome API and no clock read. `exists` is whether Chrome still finds the saved file. */
export function canContinue({
  last,
  phase,
  now,
  exists,
}: {
  last: LastRecording | null;
  phase: RecordingState['phase'];
  now: number;
  exists: boolean;
}): boolean {
  if (last === null || phase !== 'idle' || !exists) return false;
  return now - last.stopped_at <= CONTINUE_WINDOW_MS && last.recorded_ms < RECORDING_CAP_MS;
}

/** The folder name of a saved file's absolute path, on either separator. */
export function folderOf(path: string): string {
  return path.split(/[\\/]/).at(-2) ?? '';
}

const STOP_REASONS: readonly (StopReason | null)[] = ['user', 'cap', 'tab_closed', 'capture_ended', null];

function isLastRecording(value: unknown): value is LastRecording {
  if (typeof value !== 'object' || value === null) return false;
  const v = value as Record<string, unknown>;
  return (
    typeof v.download_id === 'number' &&
    typeof v.path === 'string' &&
    typeof v.folder === 'string' &&
    v.ext === 'webm' &&
    typeof v.recorded_ms === 'number' &&
    (v.language === 'bn' || v.language === 'en') &&
    typeof v.stopped_at === 'number' &&
    STOP_REASONS.includes(v.stop_reason as StopReason | null) &&
    typeof v.parts === 'number'
  );
}

/** The stored last recording, or null when there is none, it is malformed, or the flag is off. */
export async function readLastRecording(): Promise<LastRecording | null> {
  if (!continueRecordingEnabled()) return null;
  const stored = await chrome.storage.local.get(LAST_RECORDING_KEY);
  const value = stored[LAST_RECORDING_KEY];
  return isLastRecording(value) ? value : null;
}

export async function writeLastRecording(last: LastRecording): Promise<void> {
  if (!continueRecordingEnabled()) return;
  await chrome.storage.local.set({ [LAST_RECORDING_KEY]: last });
}

export async function clearLastRecording(): Promise<void> {
  if (!continueRecordingEnabled()) return;
  await chrome.storage.local.remove(LAST_RECORDING_KEY);
}

/** False once the file is moved, deleted or removed from Chrome's download history. */
export async function lastRecordingExists(last: LastRecording): Promise<boolean> {
  return (await chrome.downloads.search({ id: last.download_id, exists: true })).length > 0;
}
