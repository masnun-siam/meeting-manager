import type { Language, Note, NoteSource, Speaker } from '../../api';

// PRD desktop-app FR-6, FR-18 and the edge cases. Wording the PRD leaves open is ours.
export const TOOL_MISSING = 'Processing tool not found — run `meeting install`';
export const TOO_LARGE = '"My notes" is over 1 MB, so it was not saved. Shorten it and try again.';
export const MY_NOTES_LIMIT = 1_048_576;
export const RECORDING_MISSING = 'Recording missing';
export const NOT_WHILE_PROCESSING = 'Not available while the note is processing';

const LANGUAGES: Record<Language, string> = { bn: 'Bengali', en: 'English' };
const SOURCES: Record<NoteSource, string> = { browser: 'Browser', import: 'Import', desktop: 'Desktop' };

export const languageName = (language: Language | null) => (language ? LANGUAGES[language] : null);
export const sourceName = (source: NoteSource) => SOURCES[source];

/** A store `YYYY-MM-DDTHH:MM` local time as a local Date (no time zone is attached to it). */
export function localDateTime(value: string): Date {
  const [date, time = '00:00'] = value.split('T');
  const [y, m, d] = date.split('-').map(Number);
  const [h, min] = time.split(':').map(Number);
  return new Date(y, m - 1, d, h, min);
}

export function recordedAt(value: string): string {
  return localDateTime(value).toLocaleString(undefined, { dateStyle: 'medium', timeStyle: 'short' });
}

export function eventStart(iso: string): string {
  return new Date(iso).toLocaleString(undefined, { dateStyle: 'medium', timeStyle: 'short' });
}

export function duration(seconds: number): string {
  const total = Math.round(seconds);
  if (total < 60) return `${total} s`;
  const h = Math.floor(total / 3600);
  const min = Math.round((total % 3600) / 60);
  return h > 0 ? `${h} h ${min} min` : `${min} min`;
}

/** A calendar name is shown only when it is not an email address (the Rust side's rule). */
export const calendarName = (calendar: string | null) => (calendar && !calendar.includes('@') ? calendar : null);

const INFERRED = ' (inferred)';

/** `store::shown_name`: an edited name as is, an inferred one marked once, no name the label. */
export function shownName(speaker: Speaker): string {
  if (speaker.name === null) return speaker.label;
  return speaker.name_edited || speaker.name.endsWith(INFERRED) ? speaker.name : `${speaker.name}${INFERRED}`;
}

/** Whether any field a rerun would overwrite was edited ("My notes" never is). */
export const hasEdits = (note: Note) =>
  note.title_edited || note.body_edited || note.speakers.some((speaker) => speaker.name_edited);

export const tooLarge = (text: string) => new TextEncoder().encode(text).length > MY_NOTES_LIMIT;

/** The text the user sees for a failed command, from `attempt`'s code and text. */
export function problemText(code: string | null, text: string): string {
  switch (code) {
    case 'store_unavailable':
      return `Notes database can't be opened: ${text}`;
    case 'tool_missing':
      return TOOL_MISSING;
    case 'too_large':
      return TOO_LARGE;
    default:
      return text;
  }
}
