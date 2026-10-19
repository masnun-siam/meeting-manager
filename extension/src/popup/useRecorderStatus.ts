import { useEffect, useState } from 'react';
import { RECORDER_STATUS_KEY, type RecorderStatus } from '../shared/messages';

function isRecorderStatus(value: unknown): value is RecorderStatus {
  if (typeof value !== 'object' || value === null || !('state' in value)) return false;
  const { state } = value;
  return typeof state === 'object' && state !== null && 'phase' in state && typeof state.phase === 'string';
}

function toStatus(value: unknown): RecorderStatus | null {
  return isRecorderStatus(value) ? value : null;
}

/** The service worker's status from `chrome.storage.session`, live; null until it has written one. */
export function useRecorderStatus(): RecorderStatus | null {
  const [status, setStatus] = useState<RecorderStatus | null>(null);

  useEffect(() => {
    let active = true;
    const onChanged = (changes: Record<string, chrome.storage.StorageChange>) => {
      if (RECORDER_STATUS_KEY in changes) setStatus(toStatus(changes[RECORDER_STATUS_KEY].newValue));
    };
    chrome.storage.session.get(RECORDER_STATUS_KEY).then((stored) => {
      if (active) setStatus(toStatus(stored[RECORDER_STATUS_KEY]));
    });
    chrome.storage.session.onChanged.addListener(onChanged);
    return () => {
      active = false;
      chrome.storage.session.onChanged.removeListener(onChanged);
    };
  }, []);

  return status;
}
