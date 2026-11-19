// A stand-in for `./api` in component tests:
//
//   vi.mock('./api', async () => (await import('./api.mock')).mockApi());
//
// Every function answers a minimal `ok` reply (idle recorder, empty lists). Pass overrides for the
// calls a test cares about: `mockApi({ notesList: vi.fn().mockResolvedValue({ ok: true, notes }) })`.
import { vi } from 'vitest';
import type { CalendarEventDetail, Note } from './api';

export type Api = typeof import('./api');

const event: CalendarEventDetail = {
  occurrence_id: 'occurrence-1',
  series_id: 'series-1',
  title: 'Weekly sync',
  start: '2026-11-12T10:00:00+06:00',
  end: '2026-11-12T10:30:00+06:00',
  organizer: null,
  calendar: null,
  attendees: [],
  description: '',
};

const note: Note = {
  id: 1,
  recording_path: '/recordings/meeting-2026-11-12_1000/recording.m4a',
  source: 'desktop',
  recorded_at: '2026-11-12T10:00',
  duration_s: 1800,
  language: 'en',
  status: 'done',
  failed_stage: null,
  error: null,
  title: 'Weekly sync',
  title_edited: false,
  body_md: '',
  body_edited: false,
  speakers: [],
  my_notes: '',
  transcript: '',
  event: null,
  vault_note_path: null,
  created_at: '2026-11-12T04:30:00Z',
  updated_at: '2026-11-12T04:30:00Z',
  body_html: '',
  recording_missing: false,
  attendees: [],
  recording_deleted: false,
  media: { kind: 'audio', playable_hint: true },
};

export function mockApi(overrides: Partial<Api> = {}): Api {
  const state = { ok: true as const, state: 'idle' as const };
  return {
    recorderStatus: vi.fn(async () => ({
      ok: true as const,
      state: 'idle' as const,
      elapsed_s: 0,
      language: 'en' as const,
      folder: null,
      notice: null,
      start_error: null,
      continuable: null,
      part: null,
    })),
    recorderStart: vi.fn(async () => ({ ok: true as const, folder: '/recordings/meeting-2026-11-12_1000' })),
    recorderContinue: vi.fn(async () => ({ ok: true as const, folder: '/recordings/meeting-2026-11-12_1000' })),
    recorderPause: vi.fn(async () => ({ ok: true as const, state: 'paused' as const })),
    recorderResume: vi.fn(async () => ({ ok: true as const, state: 'recording' as const })),
    recorderStop: vi.fn(async () => ({ ok: true as const, state: 'saving' as const })),
    recorderRetry: vi.fn(async () => state),
    calendarStatus: vi.fn(async () => ({ ok: true as const, access: 'granted' as const })),
    calendarEvents: vi.fn(async () => ({ ok: true as const, days: [] })),
    calendarEvent: vi.fn(async () => ({ ok: true as const, event, earlier: [] })),
    calendars: vi.fn(async () => ({ ok: true as const, calendars: [] })),
    setHiddenCalendars: vi.fn(async () => ({ ok: true as const, calendars: [] })),
    notesList: vi.fn(async () => ({ ok: true as const, notes: [] })),
    noteGet: vi.fn(async () => ({ ok: true as const, note })),
    noteUpdate: vi.fn(async () => ({ ok: true as const, note, vault: 'ok' as const, vault_error: null })),
    noteRerun: vi.fn(async () => ({ ok: true as const })),
    noteOpen: vi.fn(async () => ({ ok: true as const })),
    noteCopy: vi.fn(async () => ({ ok: true as const })),
    speakerSuggestions: vi.fn(async () => ({ ok: true as const, attendees: [], earlier: [] })),
    noteDeleteRecording: vi.fn(async () => ({ ok: true as const, note })),
    // A plain function, not `vi.fn`: `vi.resetAllMocks()` would wipe a mock's implementation.
    recordingUrl: (id: number) => `recording://localhost/${id}`,
    rescan: vi.fn(async () => ({ ok: true as const, started: true as const })),
    rescanStatus: vi.fn(async () => ({ ok: true as const, running: false, done: 0, total: 0 })),
    health: vi.fn(async () => ({ ok: true as const, checks: [] })),
    noteArchive: vi.fn(async () => ({ ok: true as const })),
    noteTrash: vi.fn(async () => ({ ok: true as const })),
    settingsInfo: vi.fn(async () => ({
      ok: true as const,
      recordings_dir: '/recordings',
      vault_dir: '/vault',
      vault_folder: 'Meetings',
      store: { ok: true as const },
    })),
    updateStatus: vi.fn(async () => ({
      ok: true as const,
      version: '0.1.0',
      auto_update: false,
      state: 'idle' as const,
      progress: null,
      available: null,
      error: null,
      tool_error: null,
    })),
    updateCheck: vi.fn(async () => ({ ok: true as const, started: true as const })),
    updateInstall: vi.fn(async () => ({ ok: true as const, started: true as const })),
    setAutoUpdate: vi.fn(async () => ({ ok: true as const, auto_update: false })),
    recorderDismissStartError: vi.fn(async () => ({ ok: true as const })),
    recorderDismissNotice: vi.fn(async () => ({ ok: true as const })),
    indicatorSettings: vi.fn(async () => ({ ok: true as const, enabled: true })),
    setIndicatorEnabled: vi.fn(async () => ({ ok: true as const, enabled: true })),
    indicatorDismiss: vi.fn(async () => ({ ok: true as const })),
    windowOpenMain: vi.fn(async () => ({ ok: true as const })),
    ...overrides,
  };
}
