import type { Note, Speaker } from '../../api';

// PRD desktop-app FR-6, FR-18 and the edge cases. Wording the PRD leaves open is ours.
export const TOOL_MISSING = 'Processing tool not found — run `meeting install`';
export const TOO_LARGE = '"My notes" is over 1 MB, so it was not saved. Shorten it and try again.';
export const MY_NOTES_LIMIT = 1_048_576;
export const RECORDING_MISSING = 'Recording missing';
export const NOT_WHILE_PROCESSING = 'Not available while the note is processing';

export function eventStart(iso: string): string {
  return new Date(iso).toLocaleString(undefined, { dateStyle: 'medium', timeStyle: 'short' });
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
