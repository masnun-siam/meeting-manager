import type { Note, Speaker } from '../../api';

// PRD desktop-app FR-6, FR-18 and the edge cases. Wording the PRD leaves open is ours.
export const TOOL_MISSING = 'Processing tool not found — run `meeting install`';
export const TOO_LARGE = '"My notes" is over 1 MB, so it was not saved. Shorten it and try again.';
export const MY_NOTES_LIMIT = 1_048_576;
export const RECORDING_MISSING = 'Recording missing';
export const NOT_WHILE_PROCESSING = 'Not available while the note is processing';
export const RECORDING_DELETED = 'Recording deleted';
export const LOADING_RECORDING = 'Loading recording…';
export const UNPLAYABLE = "This recording can't be played in Meeting.";

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

/** A failed Open recording (FR-7, FR-8): an `open_failed` reason is prefixed, other codes as `problemText`. */
export function recordingOpenProblem(code: string | null, text: string): string {
  return code === 'open_failed' ? `Couldn't open the recording: ${text}` : problemText(code, text);
}

/** A player time as `h:mm:ss` (FR-5): `0:00:12`, `3:00:00`. Not a number (no length yet) reads as 0. */
export function formatClock(seconds: number): string {
  const total = Number.isFinite(seconds) ? Math.max(0, Math.floor(seconds)) : 0;
  const pad = (n: number) => String(n).padStart(2, '0');
  return `${Math.floor(total / 3600)}:${pad(Math.floor((total % 3600) / 60))}:${pad(total % 60)}`;
}

const STAMP = /^\[(\d{2}):(\d{2}):(\d{2})\]$/;

/** A transcript line's `[hh:mm:ss]` in seconds (FR-6): `[00:12:30]` is 750. Anything else is null. */
export function stampSeconds(stamp: string): number | null {
  const parts = STAMP.exec(stamp);
  if (!parts) return null;
  const [, h, m, s] = parts;
  return Number(h) * 3600 + Number(m) * 60 + Number(s);
}
