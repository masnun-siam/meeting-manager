// Reads the Meet page: pure functions over a Document, no Chrome APIs. Selectors: docs/extension.md "Google Meet page reading".
import { cleanText, MEET_CODE } from '../shared/meetingContext';

const MAX_NAME = 100;
const MAX_TITLE = 300;
const ROSTER_ITEMS = '[role="list"][aria-label="Participants"] > [role="listitem"]';
const TITLE_PREFIX = 'Meet - ';

export interface Participant {
  name: string;
  self: boolean;
}

const isSelf = (item: Element) => [...item.querySelectorAll('span')].some((s) => s.textContent?.trim() === '(You)');

/** Who the people panel lists now, cleaned and de-duplicated, at most one `self`. Empty when the panel is closed. */
export function readRoster(doc: Document): Participant[] {
  const byName = new Map<string, Participant>();
  let selfFound = false;
  for (const item of doc.querySelectorAll(ROSTER_ITEMS)) {
    const name = cleanText(item.getAttribute('aria-label') ?? '', MAX_NAME);
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
