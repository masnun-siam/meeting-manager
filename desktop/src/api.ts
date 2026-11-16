// The command contract between the UI and the app's Rust side (PRD desktop-app, Technical design › API).
// Argument keys are snake_case on purpose: they are the names the Rust commands take.
import { invoke } from '@tauri-apps/api/core';

export type Ok<T> = { ok: true } & T;
export type Err<Code extends string = string> = { ok: false; code: Code; message: string };
export type Reply<T, Code extends string = string> = Ok<T> | Err<Code>;

export type Language = 'bn' | 'en';
export type RecorderState = 'idle' | 'recording' | 'paused' | 'saving';
export type Notice = { kind: string; message: string; folder: string | null };
export type RecorderStatus = {
  state: RecorderState;
  elapsed_s: number;
  language: Language;
  folder: string | null;
  notice: Notice | null;
};
export type RecorderStartError =
  | 'permission_mic'
  | 'permission_audio'
  | 'already_recording'
  | 'not_configured'
  | 'event_too_far'
  | 'unknown_event'
  | 'calendar_denied'
  | 'capture_failed';
export type RecorderControlError = 'not_recording';
export type RecorderRetryError = 'not_found' | 'already_recording';

export type CalendarAccess = 'granted' | 'denied' | 'not_asked';
export type CalendarListEvent = {
  occurrence_id: string;
  series_id: string;
  title: string;
  start: string;
  end: string;
  calendar: string | null;
  has_notes: boolean;
};
export type CalendarDay = { date: string; events: CalendarListEvent[] };
export type AttendeeResponse = 'accepted' | 'declined' | 'tentative' | 'needs_action';
export type CalendarEventDetail = {
  occurrence_id: string;
  series_id: string;
  title: string;
  start: string;
  end: string;
  organizer: string | null;
  calendar: string | null;
  attendees: { name: string; response: AttendeeResponse }[];
  description: string | null;
};
export type EarlierNote = { id: number; title: string | null; recorded_at: string };
export type CalendarInfo = { id: string; title: string; account: string; hidden: boolean };
export type CalendarEventsError = 'calendar_denied' | 'calendar_failed';
export type CalendarEventError = 'calendar_denied' | 'unknown_event' | 'calendar_failed';
export type SetHiddenCalendarsError = 'store_unavailable';

export type NoteSource = 'browser' | 'desktop' | 'import';
export type NoteStatus = 'processing' | 'done' | 'no_speech' | 'failed';
export type NotesListRow = {
  id: number;
  title: string | null;
  folder_name: string;
  recorded_at: string;
  duration_s: number | null;
  language: Language | null;
  source: NoteSource;
  status: NoteStatus;
  stage: string | null;
  error: string | null;
  recording_missing: boolean;
};
export type Speaker = { label: string; name: string | null; evidence: string; name_edited: boolean };
export type EventLink = {
  occurrence_id: string;
  series_id: string;
  title: string;
  start: string;
  end: string;
  calendar: string | null;
};
export type Note = {
  id: number;
  recording_path: string;
  source: NoteSource;
  recorded_at: string;
  duration_s: number | null;
  language: Language | null;
  status: NoteStatus;
  failed_stage: string | null;
  error: string | null;
  title: string | null;
  title_edited: boolean;
  body_md: string | null;
  body_edited: boolean;
  speakers: Speaker[];
  my_notes: string;
  transcript: string | null;
  event: EventLink | null;
  vault_note_path: string | null;
  created_at: string;
  updated_at: string;
  body_html: string;
  recording_missing: boolean;
};
export type NoteUpdate = {
  id: number;
  title?: string;
  body_md?: string;
  my_notes?: string;
  speaker?: { label: string; name: string };
};
export type NotesListError = 'store_unavailable';
export type NoteGetError = 'unknown_note' | 'store_unavailable';
export type NoteUpdateError = 'unknown_note' | 'not_editable' | 'too_large' | 'store_unavailable';
export type NoteRerunError =
  | 'unknown_note'
  | 'already_processing'
  | 'tool_missing'
  | 'recording_missing'
  | 'store_unavailable';
type NoteFileError = 'already_processing' | 'invalid_path' | 'unknown_note' | 'store_unavailable';
export type NoteArchiveError = NoteFileError | 'archive_failed';
export type NoteTrashError = NoteFileError | 'trash_failed';
export type NoteOpenError = 'missing_file' | 'open_failed' | 'store_unavailable';

export type RescanError = 'not_configured';
export type RescanStatus = { running: boolean; done: number; total: number };
export type HealthCheck = { name: string; ok: boolean; fix: string | null };
export type SettingsInfo = {
  recordings_dir: string;
  vault_dir: string;
  vault_folder: string;
  store: { ok: true } | { ok: false; message: string };
};
export type SettingsInfoError = 'not_configured';

export type UpdateState = 'idle' | 'checking' | 'downloading' | 'ready' | 'installing';
export type UpdateStatus = {
  version: string;
  auto_update: boolean;
  state: UpdateState;
  progress: number | null;
  available: { version: string; notes: string | null } | null;
  error: string | null;
  tool_error: string | null;
};
export type UpdateInstallError = 'recording_active' | 'no_update';
export type SetAutoUpdateError = 'store_unavailable';

function isReply(value: unknown): value is { ok: boolean } {
  if (typeof value !== 'object' || value === null || !('ok' in value) || typeof value.ok !== 'boolean') {
    return false;
  }
  if (value.ok) {
    return true;
  }
  return 'code' in value && typeof value.code === 'string' && 'message' in value && typeof value.message === 'string';
}

async function call<T, Code extends string = string>(
  command: string,
  args?: Record<string, unknown>,
): Promise<Reply<T, Code>> {
  const reply: unknown = await invoke(command, args);
  if (!isReply(reply)) {
    throw new Error('Invalid reply from the Meeting app');
  }
  // The Rust side is trusted to answer with the shape of the command it was asked.
  return reply as Reply<T, Code>;
}

export const recorderStatus = () => call<RecorderStatus>('recorder_status');
export const recorderStart = (req: { language: Language; event_occurrence_id: string | null }) =>
  call<{ folder: string }, RecorderStartError>('recorder_start', req);
export const recorderPause = () => call<{ state: RecorderState }, RecorderControlError>('recorder_pause');
export const recorderResume = () => call<{ state: RecorderState }, RecorderControlError>('recorder_resume');
export const recorderStop = () => call<{ state: RecorderState }, RecorderControlError>('recorder_stop');
export const recorderRetry = (req: { folder: string }) =>
  call<{ state: RecorderState }, RecorderRetryError>('recorder_retry', req);

export const calendarStatus = () => call<{ access: CalendarAccess }>('calendar_status');
export const calendarEvents = () => call<{ days: CalendarDay[] }, CalendarEventsError>('calendar_events');
export const calendarEvent = (req: { occurrence_id: string }) =>
  call<{ event: CalendarEventDetail; earlier: EarlierNote[] }, CalendarEventError>('calendar_event', req);
export const calendars = () => call<{ calendars: CalendarInfo[] }>('calendars');
export const setHiddenCalendars = (req: { ids: string[] }) =>
  call<{ calendars: CalendarInfo[] }, SetHiddenCalendarsError>('set_hidden_calendars', req);

export const notesList = (req: { query: string | null }) =>
  call<{ notes: NotesListRow[] }, NotesListError>('notes_list', req);
export const noteGet = (req: { id: number }) => call<{ note: Note }, NoteGetError>('note_get', req);
export const noteUpdate = (req: NoteUpdate) =>
  call<{ note: Note; vault: 'ok' | 'failed'; vault_error: string | null }, NoteUpdateError>('note_update', req);
export const noteRerun = (req: { id: number; replace_edits: boolean }) =>
  call<object, NoteRerunError>('note_rerun', req);
export const noteOpen = (req: { id: number; what: 'obsidian' | 'folder' }) =>
  call<object, NoteOpenError>('note_open', req);

export const rescan = () => call<{ started: true }, RescanError>('rescan');
export const rescanStatus = () => call<RescanStatus>('rescan_status');
export const health = () => call<{ checks: HealthCheck[] }>('health');
export const noteArchive = (req: { id: number }) => call<object, NoteArchiveError>('note_archive', req);
export const noteTrash = (req: { id: number }) => call<object, NoteTrashError>('note_trash', req);
export const settingsInfo = () => call<SettingsInfo, SettingsInfoError>('settings_info');

export const updateStatus = () => call<UpdateStatus>('update_status');
export const updateCheck = () => call<{ started: true }>('update_check');
export const updateInstall = () => call<{ started: true }, UpdateInstallError>('update_install');
export const setAutoUpdate = (req: { enabled: boolean }) =>
  call<{ auto_update: boolean }, SetAutoUpdateError>('set_auto_update', req);
