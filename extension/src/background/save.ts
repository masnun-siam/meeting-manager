// Saves a finished recording through chrome.downloads and hands it to the native host.
import { folderOf, writeLastRecording } from '../shared/lastRecording';
import type { Language, Notice } from '../shared/messages';
import type { ContextPayload, ProcessRequest } from '../shared/native';
import { sendNative } from '../shared/nativeClient';
import { deleteRecordingFile } from '../shared/recordingFile';
import type { StopReason } from '../shared/recordingState';

const DISK_FULL = 'FILE_NO_SPACE';

export interface SaveOptions {
  startedAt: number;
  name: string;
  language: Language;
  process: boolean;
  context?: ContextPayload;
  /** Set to store the saved recording as `last_recording` (with FEATURE_CONTINUE_RECORDING). */
  remember?: { recordedMs: number; stoppedAt: number; stopReason: StopReason | null };
}

export function recordingName(startedAt: number): string {
  const d = new Date(startedAt);
  const p = (n: number) => String(n).padStart(2, '0');
  return `meeting-${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())}_${p(d.getHours())}${p(d.getMinutes())}`;
}

export function nextFreeName(base: string, taken: Set<string>): string {
  if (!taken.has(base)) return base;
  let n = 2;
  while (taken.has(`${base}-${n}`)) n++;
  return `${base}-${n}`;
}

/** Folder names already saved under `base`, from Chrome's download history. */
// ponytail: files Chrome did not download are invisible here, so `conflictAction: 'uniquify'`
// then gives `recording (1).webm` inside the existing folder instead of a `-2` folder. Scan the folder through the host if that matters.
export async function takenNames(base: string): Promise<Set<string>> {
  const items = await chrome.downloads.search({
    filenameRegex: `Meetings/${base}(-\\d+)?/recording\\.webm$`,
    exists: true,
  });
  return new Set(items.map((item) => folderOf(item.filename)));
}

function errorNotice(error: string): Notice {
  const message =
    error === DISK_FULL
      ? 'Disk full: the recording could not be saved. It is kept in the extension and will be saved the next time Chrome starts.'
      : `Could not save the recording (${error}). It is kept in the extension and will be saved the next time Chrome starts.`;
  return { kind: 'error', message };
}

type Downloaded = { path: string; id: number } | { error: string };

/** Starts the download and resolves with the saved file's absolute path and download id, or the interruption error. */
function download(url: string, filename: string): Promise<Downloaded> {
  return new Promise((resolve) => {
    let id: number | undefined;
    const onChanged = (delta: chrome.downloads.DownloadDelta) => {
      if (delta.id !== id) return;
      if (delta.state?.current === 'complete') {
        done(async () => {
          const [item] = await chrome.downloads.search({ id });
          return item ? { path: item.filename, id: delta.id } : { error: 'FILE_MISSING' };
        });
      } else if (delta.state?.current === 'interrupted') {
        done(async () => ({ error: delta.error?.current ?? 'UNKNOWN' }));
      }
    };
    const done = (result: () => Promise<Downloaded>) => {
      chrome.downloads.onChanged.removeListener(onChanged);
      result().then(resolve);
    };
    chrome.downloads.onChanged.addListener(onChanged);
    chrome.downloads
      .download({ url, filename, saveAs: false, conflictAction: 'uniquify' })
      .then((downloadId) => {
        id = downloadId;
      })
      .catch((error) => done(async () => ({ error: error instanceof Error ? error.message : String(error) })));
  });
}

function noticeFor(reply: Awaited<ReturnType<typeof sendNative<ProcessRequest>>>): Notice {
  if (reply.ok) return { kind: 'saved' };
  if (reply.code === 'host_missing' || reply.code === 'not_configured')
    return { kind: 'setup', message: reply.message };
  return { kind: 'error', message: reply.message };
}

/** Never fails the save: a write error is logged with a fixed text and nothing else. */
async function remember(
  saved: { path: string; id: number },
  language: Language,
  { recordedMs, stoppedAt, stopReason }: NonNullable<SaveOptions['remember']>,
): Promise<void> {
  try {
    await writeLastRecording({
      download_id: saved.id,
      path: saved.path,
      folder: folderOf(saved.path),
      ext: 'webm',
      recorded_ms: recordedMs,
      language,
      stopped_at: stoppedAt,
      stop_reason: stopReason,
      parts: 1,
    });
  } catch {
    console.error('Could not store the last recording');
  }
}

/** Never throws: every outcome is a notice, and the origin-private file is deleted only after a save succeeds. */
export async function saveRecording({
  startedAt,
  name,
  language,
  process,
  context,
  remember: toRemember,
}: SaveOptions): Promise<Notice> {
  const file = (await chrome.runtime.sendMessage({ type: 'offscreen_file_url', startedAt })) as
    | { url: string }
    | { error: string }
    | undefined;
  if (!file || 'error' in file) return errorNotice(file?.error ?? 'no recording file');
  const saved = await download(file.url, `Meetings/${name}/recording.webm`);
  if ('error' in saved) return errorNotice(saved.error);
  if (toRemember) await remember(saved, language, toRemember);
  // The file is saved; a failed cleanup must not stop processing. Recovery (#28) tolerates a leftover.
  await deleteRecordingFile(startedAt).catch(() => undefined);
  if (!process) return { kind: 'too_short' };
  try {
    return noticeFor(
      await sendNative({ type: 'process', path: saved.path, language, ...(context ? { context } : {}) }),
    );
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    return { kind: 'error', message: `Could not reach the meeting tool: ${message}` };
  }
}
