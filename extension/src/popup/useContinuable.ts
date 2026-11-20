import { useEffect, useState } from 'react';
import { continueRecordingEnabled } from '../shared/flags';
import {
  canContinue,
  LAST_RECORDING_KEY,
  type LastRecording,
  lastRecordingExists,
  readLastRecording,
} from '../shared/lastRecording';
import type { RecorderStatus } from '../shared/messages';

/** How often an open popup re-applies the 2 h window. */
export const RECHECK_MS = 30_000;

export interface Continuable {
  last: LastRecording;
  /** The clock the rule was applied with, for "stopped N min ago". */
  now: number;
}

/** The last recording to offer for Continue, live; null when the flag is off or the rule refuses. */
export function useContinuable(status: RecorderStatus | null): Continuable | null {
  const phase = status?.state.phase ?? 'idle';
  const [last, setLast] = useState<LastRecording | null>(null);
  /** The existence result belongs to one `last`, so a newer one is never shown unchecked. */
  const [checked, setChecked] = useState<{ last: LastRecording; exists: boolean } | null>(null);
  const [now, setNow] = useState(() => Date.now());

  useEffect(() => {
    if (!continueRecordingEnabled()) return;
    let active = true;
    const load = () =>
      readLastRecording().then((value) => {
        if (active) setLast(value);
      });
    const onChanged = (changes: Record<string, chrome.storage.StorageChange>) => {
      if (LAST_RECORDING_KEY in changes) load();
    };
    load();
    chrome.storage.local.onChanged.addListener(onChanged);
    return () => {
      active = false;
      chrome.storage.local.onChanged.removeListener(onChanged);
    };
  }, []);

  // Re-checked on every status write too: a failed Continue writes its notice after finding the file gone.
  useEffect(() => {
    if (last === null || (status?.state.phase ?? 'idle') !== 'idle') return;
    let active = true;
    lastRecordingExists(last)
      .catch(() => false)
      .then((exists) => {
        if (!active) return;
        setChecked({ last, exists });
        setNow(Date.now());
      });
    return () => {
      active = false;
    };
  }, [last, status]);

  useEffect(() => {
    if (last === null) return;
    const id = setInterval(() => setNow(Date.now()), RECHECK_MS);
    return () => clearInterval(id);
  }, [last]);

  if (last === null || checked?.last !== last) return null;
  return canContinue({ last, phase, now, exists: checked.exists }) ? { last, now } : null;
}
