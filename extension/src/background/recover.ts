// Saves recordings left in origin-private storage by a crash, an extension reload or a failed save.

import type { Language, Notice } from '../shared/messages';
import { RECORDINGS_DIR } from '../shared/recordingFile';
import { closeOffscreen, ensureOffscreen, readStatus, writeStatus } from './recordingControl';
import { nextFreeName, recordingName, saveRecording, takenNames } from './save';

export function recoveredName(startedAt: number): string {
  return `${recordingName(startedAt)}-recovered`;
}

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

async function recoverOne(startedAt: number, language: Language): Promise<Notice> {
  try {
    const base = recoveredName(startedAt);
    return await saveRecording({
      startedAt,
      name: nextFreeName(base, await takenNames(base)),
      language,
      process: true,
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
  const pending: number[] = [];
  for (const { startedAt, file } of candidates) {
    if (file.size === 0) await dir.removeEntry(`${startedAt}.webm`);
    else pending.push(startedAt);
  }
  if (pending.length === 0) return;
  const language = await lastLanguage();
  await ensureOffscreen(chrome.offscreen.Reason.BLOBS, 'Save recordings left by a crash or reload');
  try {
    for (const startedAt of pending) await showNotice(await recoverOne(startedAt, language));
  } finally {
    // A Start may have happened meanwhile, and it needs this document.
    if ((await readStatus()).state.phase === 'idle') await closeOffscreen();
  }
}
