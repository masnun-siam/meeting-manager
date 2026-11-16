import { beforeEach, describe, expect, it, vi } from 'vitest';

const invoke = vi.fn();
vi.mock('@tauri-apps/api/core', () => ({
  invoke: (command: string, args?: unknown) => invoke(command, args),
}));

import * as api from './api';

type Case = [name: string, run: () => Promise<unknown>, command: string, args: Record<string, unknown> | undefined];

const cases: Case[] = [
  ['recorderStatus', () => api.recorderStatus(), 'recorder_status', undefined],
  [
    'recorderStart',
    () => api.recorderStart({ language: 'bn', event_occurrence_id: 'occurrence-1' }),
    'recorder_start',
    { language: 'bn', event_occurrence_id: 'occurrence-1' },
  ],
  ['recorderPause', () => api.recorderPause(), 'recorder_pause', undefined],
  ['recorderResume', () => api.recorderResume(), 'recorder_resume', undefined],
  ['recorderStop', () => api.recorderStop(), 'recorder_stop', undefined],
  [
    'recorderRetry',
    () => api.recorderRetry({ folder: '/recordings/m' }),
    'recorder_retry',
    { folder: '/recordings/m' },
  ],
  ['calendarStatus', () => api.calendarStatus(), 'calendar_status', undefined],
  ['calendarEvents', () => api.calendarEvents(), 'calendar_events', undefined],
  [
    'calendarEvent',
    () => api.calendarEvent({ occurrence_id: 'occurrence-1' }),
    'calendar_event',
    { occurrence_id: 'occurrence-1' },
  ],
  ['calendars', () => api.calendars(), 'calendars', undefined],
  [
    'setHiddenCalendars',
    () => api.setHiddenCalendars({ ids: ['a', 'b'] }),
    'set_hidden_calendars',
    { ids: ['a', 'b'] },
  ],
  ['notesList', () => api.notesList({ query: null }), 'notes_list', { query: null }],
  ['noteGet', () => api.noteGet({ id: 7 }), 'note_get', { id: 7 }],
  [
    'noteUpdate',
    () =>
      api.noteUpdate({
        id: 7,
        title: 'T',
        body_md: 'B',
        my_notes: 'N',
        speaker: { label: 'Speaker 1', name: 'Rahim' },
      }),
    'note_update',
    { id: 7, title: 'T', body_md: 'B', my_notes: 'N', speaker: { label: 'Speaker 1', name: 'Rahim' } },
  ],
  ['noteRerun', () => api.noteRerun({ id: 7, replace_edits: true }), 'note_rerun', { id: 7, replace_edits: true }],
  ['noteOpen', () => api.noteOpen({ id: 7, what: 'folder' }), 'note_open', { id: 7, what: 'folder' }],
  ['rescan', () => api.rescan(), 'rescan', undefined],
  ['rescanStatus', () => api.rescanStatus(), 'rescan_status', undefined],
  ['health', () => api.health(), 'health', undefined],
  ['noteArchive', () => api.noteArchive({ id: 7 }), 'note_archive', { id: 7 }],
  ['noteTrash', () => api.noteTrash({ id: 7 }), 'note_trash', { id: 7 }],
  ['settingsInfo', () => api.settingsInfo(), 'settings_info', undefined],
  ['updateStatus', () => api.updateStatus(), 'update_status', undefined],
  ['updateCheck', () => api.updateCheck(), 'update_check', undefined],
  ['updateInstall', () => api.updateInstall(), 'update_install', undefined],
  ['setAutoUpdate', () => api.setAutoUpdate({ enabled: true }), 'set_auto_update', { enabled: true }],
];

beforeEach(() => {
  invoke.mockReset();
});

describe('api', () => {
  it('has one case per exported command', () => {
    expect(cases.map(([name]) => name).sort()).toEqual(Object.keys(api).sort());
    expect(cases).toHaveLength(26);
  });

  it.each(cases)('%s invokes its command with snake_case arguments', async (_name, run, command, args) => {
    invoke.mockResolvedValue({ ok: true });

    await expect(run()).resolves.toEqual({ ok: true });

    expect(invoke).toHaveBeenCalledTimes(1);
    expect(invoke).toHaveBeenCalledWith(command, args);
  });

  it('returns an ok reply with its fields', async () => {
    const reply = { ok: true, state: 'idle', elapsed_s: 0, language: 'en', folder: null, notice: null };
    invoke.mockResolvedValue(reply);

    await expect(api.recorderStatus()).resolves.toBe(reply);
  });

  it('returns an error reply unchanged', async () => {
    const reply = { ok: false, code: 'not_implemented', message: 'recorder_status is not implemented yet' };
    invoke.mockResolvedValue(reply);

    await expect(api.recorderStatus()).resolves.toBe(reply);
  });

  it.each([
    ['a string', 'nope'],
    ['null', null],
    ['an object without ok', { state: 'idle' }],
    ['a non-boolean ok', { ok: 'yes' }],
    ['an error without a code', { ok: false, message: 'm' }],
    ['an error without a message', { ok: false, code: 'c' }],
  ])('throws for %s', async (_name, reply) => {
    invoke.mockResolvedValue(reply);

    await expect(api.recorderStatus()).rejects.toThrow('Invalid reply from the Meeting app');
  });

  it('lets an invoke failure through', async () => {
    invoke.mockRejectedValue(new Error('Command recorder_status not found'));

    await expect(api.recorderStatus()).rejects.toThrow('Command recorder_status not found');
  });
});
