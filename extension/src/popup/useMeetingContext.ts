import { useEffect, useState } from 'react';
import { meetingContextEnabled } from '../shared/flags';
import { MEETING_CONTEXT_KEY, type MeetingContextState } from '../shared/messages';

function isMeetingContext(value: unknown): value is MeetingContextState {
  if (typeof value !== 'object' || value === null) return false;
  if (!('capture' in value) || !('identity' in value) || !('participants' in value)) return false;
  const { capture, identity, participants } = value;
  return (
    typeof capture === 'string' && typeof identity === 'object' && identity !== null && Array.isArray(participants)
  );
}

function toContext(value: unknown): MeetingContextState | null {
  return isMeetingContext(value) ? value : null;
}

/** The meeting context from `chrome.storage.session`, live; null when the flag is off or none is stored. */
export function useMeetingContext(): MeetingContextState | null {
  const [context, setContext] = useState<MeetingContextState | null>(null);

  useEffect(() => {
    if (!meetingContextEnabled()) return;
    let active = true;
    const onChanged = (changes: Record<string, chrome.storage.StorageChange>) => {
      if (MEETING_CONTEXT_KEY in changes) setContext(toContext(changes[MEETING_CONTEXT_KEY].newValue));
    };
    chrome.storage.session.get(MEETING_CONTEXT_KEY).then((stored) => {
      if (active) setContext(toContext(stored[MEETING_CONTEXT_KEY]));
    });
    chrome.storage.session.onChanged.addListener(onChanged);
    return () => {
      active = false;
      chrome.storage.session.onChanged.removeListener(onChanged);
    };
  }, []);

  return context;
}
