// Links a continued part's origin-private recording to its meeting folder, in chrome.storage.local so the link
// survives a crash for recovery (#277). Keyed by the part's `startedAt`. Every helper does nothing with the flag off.
// Callers run inside the service worker's serial queue, so the read-modify-write of the one object cannot race.

import { continueRecordingEnabled } from './flags';
import type { Language } from './messages';

export const CONTINUING_PARTS_KEY = 'continuing_parts';

export interface ContinuingPart {
  folder: string;
  part: number;
  language: Language;
}

function isContinuingPart(value: unknown): value is ContinuingPart {
  if (typeof value !== 'object' || value === null) return false;
  const v = value as Record<string, unknown>;
  return typeof v.folder === 'string' && typeof v.part === 'number' && (v.language === 'bn' || v.language === 'en');
}

async function readAll(): Promise<Record<string, unknown>> {
  const stored = await chrome.storage.local.get(CONTINUING_PARTS_KEY);
  const value = stored[CONTINUING_PARTS_KEY];
  return typeof value === 'object' && value !== null ? (value as Record<string, unknown>) : {};
}

export async function rememberContinuingPart(startedAt: number, part: ContinuingPart): Promise<void> {
  if (!continueRecordingEnabled()) return;
  const all = await readAll();
  await chrome.storage.local.set({ [CONTINUING_PARTS_KEY]: { ...all, [startedAt]: part } });
}

/** The entry for `startedAt`, or null when there is none, it is malformed, or the flag is off. */
export async function readContinuingPart(startedAt: number): Promise<ContinuingPart | null> {
  if (!continueRecordingEnabled()) return null;
  const value = (await readAll())[startedAt];
  return isContinuingPart(value) ? value : null;
}

export async function forgetContinuingPart(startedAt: number): Promise<void> {
  if (!continueRecordingEnabled()) return;
  const all = await readAll();
  if (!(startedAt in all)) return;
  const { [startedAt]: _removed, ...rest } = all;
  await chrome.storage.local.set({ [CONTINUING_PARTS_KEY]: rest });
}

/** Drops every entry whose part has no origin-private file left (a Start that failed after the entry was written). */
export async function forgetContinuingPartsWithout(startedAts: ReadonlySet<number>): Promise<void> {
  if (!continueRecordingEnabled()) return;
  const all = await readAll();
  const kept = Object.fromEntries(Object.entries(all).filter(([key]) => startedAts.has(Number(key))));
  if (Object.keys(kept).length === Object.keys(all).length) return;
  await chrome.storage.local.set({ [CONTINUING_PARTS_KEY]: kept });
}
