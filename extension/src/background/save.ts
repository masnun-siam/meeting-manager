// Saves a finished recording through chrome.downloads and hands it to the native host.
import { forgetContinuingPart } from '../shared/continuingParts';
import { folderOf, type LastRecording, writeLastRecording } from '../shared/lastRecording';
import type { Language, Notice } from '../shared/messages';
import type { ContextPayload, ProcessPartRequest, ProcessRequest } from '../shared/native';
import { sendNative } from '../shared/nativeClient';
import { deleteRecordingFile } from '../shared/recordingFile';
import type { StopReason } from '../shared/recordingState';

const DISK_FULL = 'FILE_NO_SPACE';

/** Recorded time below which a recording is saved but not handed off. */
export const MIN_PROCESS_MS = 10_000;

export interface SaveOptions {
  startedAt: number;
  name: string;
  language: Language;
  process: boolean;
  context?: ContextPayload;
  /** Set to store the saved recording as `last_recording` (with FEATURE_CONTINUE_RECORDING). */
  remember?: { recordedMs: number; stoppedAt: number; stopReason: StopReason | null };
  /**
   * Set for a continued part: it is saved as `recording-part-<part>.webm` beside the meeting's `recording.webm`
   * (`recording`, null when no longer known) and handed off with `process_part`. `process` and
   * `remember.recordedMs` count the meeting total; `partMs` is this part alone, for the new-meeting fallback.
   */
  continuing?: {
    folder: string;
    part: number;
    partMs: number;
    recording: { path: string; downloadId: number } | null;
  };
}

/** The notice when a part's meeting folder was moved or deleted and the part became a meeting of its own. */
export function movedNotice(folder: string): Notice {
  return { kind: 'error', message: `Could not add to ${folder}: it was moved; saved as a new meeting.` };
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

type NativeReply = Awaited<ReturnType<typeof sendNative<ProcessRequest | ProcessPartRequest>>>;

function noticeFor(reply: NativeReply): Notice {
  if (reply.ok) return { kind: 'saved' };
  if (reply.code === 'host_missing' || reply.code === 'not_configured')
    return { kind: 'setup', message: reply.message };
  return { kind: 'error', message: reply.message };
}

async function handOff(request: ProcessRequest | ProcessPartRequest): Promise<Notice> {
  try {
    return noticeFor(await sendNative(request));
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    return { kind: 'error', message: `Could not reach the meeting tool: ${message}` };
  }
}

/** Never fails the save: a write error is logged with a fixed text and nothing else. */
async function remember(last: LastRecording): Promise<void> {
  try {
    await writeLastRecording(last);
  } catch {
    console.error('Could not store the last recording');
  }
}

/** The saved file is safe on disk: a failed cleanup must not stop processing. Recovery (#28) tolerates a leftover. */
async function cleanUp(startedAt: number): Promise<void> {
  await deleteRecordingFile(startedAt).catch(() => undefined);
  await forgetContinuingPart(startedAt).catch(() => console.error('Could not forget the continued part'));
}

type FileUrl = { url: string } | { error: string } | undefined;

/** Never throws: every outcome is a notice, and the origin-private file is deleted only after a save succeeds. */
export async function saveRecording(options: SaveOptions): Promise<Notice> {
  const file = (await chrome.runtime.sendMessage({
    type: 'offscreen_file_url',
    startedAt: options.startedAt,
  })) as FileUrl;
  if (!file || 'error' in file) return errorNotice(file?.error ?? 'no recording file');
  if (!options.continuing) return (await saveMeeting(file.url, options)).notice;
  const partNotice = await savePart(file.url, options, options.continuing);
  return partNotice ?? savePartFallback(file.url, options, options.continuing);
}

/** Saves a new meeting folder holding `recording.webm`; `saved` is false when the download failed. */
async function saveMeeting(
  url: string,
  { startedAt, name, language, process, context, remember: toRemember }: SaveOptions,
): Promise<{ notice: Notice; saved: boolean }> {
  const saved = await download(url, `Meetings/${name}/recording.webm`);
  if ('error' in saved) return { notice: errorNotice(saved.error), saved: false };
  if (toRemember) {
    await remember({
      download_id: saved.id,
      path: saved.path,
      folder: folderOf(saved.path),
      ext: 'webm',
      recorded_ms: toRemember.recordedMs,
      language,
      stopped_at: toRemember.stoppedAt,
      stop_reason: toRemember.stopReason,
      parts: 1,
    });
  }
  await cleanUp(startedAt);
  if (!process) return { notice: { kind: 'too_short' }, saved: true };
  return {
    notice: await handOff({ type: 'process', path: saved.path, language, ...(context ? { context } : {}) }),
    saved: true,
  };
}

/** The expected absolute path of the part: beside the meeting's own `recording.webm`. */
function partPath(recordingPath: string, part: number): string {
  return recordingPath.replace(/[^\\/]+$/, `recording-part-${part}.webm`);
}

/** Saves a part into its meeting folder. Null means the folder was moved: the caller saves a new meeting instead. */
async function savePart(
  url: string,
  { startedAt, language, process, remember: toRemember }: SaveOptions,
  { folder, part, recording }: NonNullable<SaveOptions['continuing']>,
): Promise<Notice | null> {
  // Chrome recreates a missing folder, so a moved one is found by asking for the earlier file first.
  if (recording === null) return null;
  if ((await chrome.downloads.search({ id: recording.downloadId, exists: true })).length === 0) return null;
  const saved = await download(url, `Meetings/${folder}/recording-part-${part}.webm`);
  if ('error' in saved) return errorNotice(saved.error);
  // A uniquified name or another directory: the stray file stays, the part is saved again as a new meeting.
  if (saved.path !== partPath(recording.path, part)) return null;
  if (toRemember) {
    await remember({
      download_id: recording.downloadId,
      path: recording.path,
      folder,
      ext: 'webm',
      recorded_ms: toRemember.recordedMs,
      language,
      stopped_at: toRemember.stoppedAt,
      stop_reason: toRemember.stopReason,
      parts: part,
    });
  }
  await cleanUp(startedAt);
  if (!process) return { kind: 'too_short' };
  return handOff({ type: 'process_part', path: saved.path, language });
}

/** The part as a meeting of its own, as a Start would have saved it, with the moved-folder notice. */
async function savePartFallback(
  url: string,
  options: SaveOptions,
  { folder, partMs }: NonNullable<SaveOptions['continuing']>,
): Promise<Notice> {
  const { notice, saved } = await saveMeeting(url, {
    ...options,
    process: partMs >= MIN_PROCESS_MS,
    context: undefined,
    remember: options.remember && { ...options.remember, recordedMs: partMs },
  });
  // A failed download outranks the move: it says the recording is kept and saved later.
  return saved ? movedNotice(folder) : notice;
}
