// Reads the Meet page: pure functions over a Document, no Chrome APIs. Selectors: docs/extension.md "Google Meet page reading".
import { cleanText, MEET_CODE } from '../shared/meetingContext';
import type { SpeakingChange } from '../shared/messages';

const MAX_NAME = 100;
const MAX_TITLE = 300;
const ROSTER_ITEMS = '[role="list"][aria-label="Participants"] > [role="listitem"]';
const TITLE_PREFIX = 'Meet - ';
// Obfuscated: expect breakage on a Meet release, which reads as nobody speaking.
const SPEAKING_MARK = '.kssMZb.v5h6Xc';
const TILE = '[data-tile-media-id]';

export interface Participant {
  name: string;
  self: boolean;
}

const nameOf = (item: Element) => cleanText(item.getAttribute('aria-label') ?? '', MAX_NAME);
const isSelf = (item: Element) => [...item.querySelectorAll('span')].some((s) => s.textContent?.trim() === '(You)');

/** Who the people panel lists now, cleaned and de-duplicated, at most one `self`. Empty when the panel is closed. */
export function readRoster(doc: Document): Participant[] {
  const byName = new Map<string, Participant>();
  let selfFound = false;
  for (const item of doc.querySelectorAll(ROSTER_ITEMS)) {
    const name = nameOf(item);
    const self: boolean = !selfFound && isSelf(item);
    // A bare "You" would mislead Claude; leave the self entry out instead.
    if (!name || (self && name === 'You')) continue;
    selfFound ||= self;
    const known = byName.get(name);
    if (known) known.self ||= self;
    else byName.set(name, { name, self });
  }
  return [...byName.values()];
}

/** The event name from `Meet - <name>`; null for an ad-hoc call, where Meet shows the meeting code instead. */
export function readTitle(doc: Document): string | null {
  if (!doc.title.startsWith(TITLE_PREFIX)) return null;
  const title = cleanText(doc.title.slice(TITLE_PREFIX.length), MAX_TITLE);
  return title && !MEET_CODE.test(title) ? title : null;
}

/** Who Meet flags as speaking now, named as `readRoster` names them. Empty when nobody, without the panel, or on any error. */
export function readSpeaking(doc: Document): string[] {
  try {
    const ids = new Set<string | null>();
    for (const mark of doc.querySelectorAll(SPEAKING_MARK)) {
      ids.add(mark.closest(TILE)?.getAttribute('data-participant-id') ?? null);
    }
    ids.delete(null);
    if (ids.size === 0) return [];
    const known = new Set(readRoster(doc).map((p) => p.name));
    const names = new Set<string>();
    for (const item of doc.querySelectorAll(ROSTER_ITEMS)) {
      const name = nameOf(item);
      if (ids.has(item.getAttribute('data-participant-id')) && known.has(name)) names.add(name);
    }
    return [...names];
  } catch {
    return [];
  }
}

/** The change from `prev` to `next` at `at`, or null when the set is the same (order ignored). */
export function diffSpeaking(prev: string[], next: string[], at: number): SpeakingChange | null {
  const names = [...new Set(next)].sort();
  const before = [...new Set(prev)].sort();
  return names.join('\n') === before.join('\n') ? null : { at, names };
}
