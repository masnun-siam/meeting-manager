import type { Language, NoteSource, NotesListRow } from '../../api';

const RECORDED_AT = /^(\d{4})-(\d{2})-(\d{2})T(\d{2}):(\d{2})$/;

/** `YYYY-MM-DDTHH:MM` is local wall-clock time; `new Date(string)` could read it as UTC, so it is split by hand. */
export function formatRecorded(recordedAt: string): string {
  const parts = RECORDED_AT.exec(recordedAt);
  if (!parts) return recordedAt;
  const [year, month, day, hour, minute] = parts.slice(1).map(Number);
  return new Date(year, month - 1, day, hour, minute).toLocaleString(undefined, {
    dateStyle: 'medium',
    timeStyle: 'short',
  });
}

/** `H:MM:SS` from an hour up, `M:SS` below it, `null` when the length is unknown. */
export function formatDuration(seconds: number | null): string | null {
  if (seconds === null) return null;
  const total = Math.max(0, Math.round(seconds));
  const h = Math.floor(total / 3600);
  const m = Math.floor((total % 3600) / 60);
  const s = String(total % 60).padStart(2, '0');
  return h > 0 ? `${h}:${String(m).padStart(2, '0')}:${s}` : `${m}:${s}`;
}

const LANGUAGES: Record<Language, string> = { bn: 'Bengali', en: 'English' };
const SOURCES: Record<NoteSource, string> = { browser: 'Browser', import: 'Import', desktop: 'Desktop' };

export const languageLabel = (language: Language | null): string | null =>
  language === null ? null : LANGUAGES[language];

export const sourceLabel = (source: NoteSource): string => SOURCES[source];

/** The row's status word; a failed row's reason is shown beside it, not in it. */
export function statusText(note: Pick<NotesListRow, 'status' | 'stage'>): string {
  switch (note.status) {
    case 'done':
      return 'Done';
    case 'processing':
      return note.stage ?? 'processing';
    case 'failed':
      return 'Failed';
    case 'no_speech':
      return 'No speech';
  }
}
