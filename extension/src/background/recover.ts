// Saves recordings left in origin-private storage by a crash, an extension reload or a failed save.

import { forgetContinuingPartsWithout, readContinuingPart } from '../shared/continuingParts';
import { readLastRecording } from '../shared/lastRecording';
import type { Language, Notice } from '../shared/messages';
import { RECORDINGS_DIR } from '../shared/recordingFile';
import { RECORDING_CAP_MS } from '../shared/recordingState';
import { closeOffscreen, ensureOffscreen, readStatus, writeStatus } from './recordingControl';
import { nextFreeName, recordingName, saveRecording, takenNames } from './save';

export function recoveredName(startedAt: number): string {
  return `${recordingName(startedAt)}-recovered`;
}

type Leftover = { startedAt: number; lastModified: number };

async function leftovers(): Promise<{ startedAt: number; file: File }[]> {
  const root = await navigator.storage.getDirectory();
  const dir = await root.getDirectoryHandle(RECORDINGS_DIR, { create: true });
  const found: { startedAt: number; file: File }[] = [];
  for await (const [name, handle] of dir.entries()) {
    const match = /^(\d+)\.webm$/.exec(name);
    if (match && handle.kind === 'file') found.push({ startedAt: Number(match[1]), file: await handle.getFile() });
  }
  return found;
}

async function lastLanguage(): Promise<Language> {
  const stored = await chrome.storage.local.get('language');
  return stored.language === 'en' ? 'en' : 'bn';
}

async function showNotice(notice: Notice): Promise<void> {
  await writeStatus({ ...(await readStatus()), notice });
}

/** A leftover has no stored duration: the time to its last write, paused time included, capped at 3 h. */
export function estimatedRecordedMs(startedAt: number, lastModified: number): number {
  return Math.min(RECORDING_CAP_MS, Math.max(0, lastModified - startedAt));
}

function escapeRegex(text: string): string {
  return text.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
}

/** The meeting's `recording.webm` as Chrome reports it, or null once the folder is moved, deleted or forgotten. */
async function findMeeting(folder: string): Promise<{ path: string; downloadId: number } | null> {
  const [item] = await chrome.downloads.search({
    filenameRegex: `/Meetings/${escapeRegex(folder)}/recording\\.webm$`,
    exists: true,
  });
  return item ? { path: item.filename, downloadId: item.id } : null;
}

/** The earlier meeting total, known only while `last_recording` is still that meeting. */
async function earlierTotal(folder: string): Promise<number> {
  const last = await readLastRecording();
  return last?.folder === folder ? last.recorded_ms : 0;
}

/** With the flag on and a `continuing_parts` entry whose folder Chrome still finds, the leftover is saved as its part. */
async function recoverOne({ startedAt, lastModified }: Leftover, language: Language): Promise<Notice> {
  try {
    const entry = await readContinuingPart(startedAt);
    const base = recoveredName(startedAt);
    const partMs = estimatedRecordedMs(startedAt, lastModified);
    const recording = entry && (await findMeeting(entry.folder));
    const continuing = entry && recording && { folder: entry.folder, part: entry.part, partMs, recording };
    return await saveRecording({
      startedAt,
      // Still the recovered name: the part becomes a meeting of its own if its folder moves before the save.
      name: nextFreeName(base, await takenNames(base)),
      language: continuing ? entry.language : language,
      process: true,
      // Counted from the recovery, not the original stop; no stop button ended it.
      remember: {
        recordedMs: continuing ? Math.min(RECORDING_CAP_MS, (await earlierTotal(entry.folder)) + partMs) : partMs,
        stoppedAt: Date.now(),
        stopReason: null,
      },
      ...(continuing ? { continuing } : {}),
    });
  } catch (error) {
    return {
      kind: 'error',
      message: `Could not recover a recording (${error instanceof Error ? error.message : error}).`,
    };
  }
}

/** Run inside the service worker's serial queue so it cannot interleave with a Start. */
export async function recoverLeftovers(): Promise<void> {
  const status = await readStatus();
  const recording = status.state.phase === 'idle' ? null : status.state.startedAt;
  const candidates = (await leftovers()).filter(({ startedAt }) => startedAt !== recording);
  const root = await navigator.storage.getDirectory();
  const dir = await root.getDirectoryHandle(RECORDINGS_DIR);
  const pending: Leftover[] = [];
  for (const { startedAt, file } of candidates) {
    if (file.size === 0) await dir.removeEntry(`${startedAt}.webm`);
    else pending.push({ startedAt, lastModified: file.lastModified });
  }
  // Deleted empty files lose their entry here too; the current recording keeps its own.
  const withFile = new Set(pending.map(({ startedAt }) => startedAt));
  if (recording !== null) withFile.add(recording);
  // Housekeeping only: a storage error must not stop the leftovers from being saved.
  await forgetContinuingPartsWithout(withFile).catch(() => console.error('Could not drop stale continued parts'));
  if (pending.length === 0) return;
  const language = await lastLanguage();
  await ensureOffscreen(chrome.offscreen.Reason.BLOBS, 'Save recordings left by a crash or reload');
  try {
    for (const leftover of pending) await showNotice(await recoverOne(leftover, language));
  } finally {
    // A Start may have happened meanwhile, and it needs this document.
    if ((await readStatus()).state.phase === 'idle') await closeOffscreen();
  }
}
