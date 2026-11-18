import { describe, expect, it, vi } from 'vitest';
import * as api from './api';
import { mockApi } from './api.mock';

describe('mockApi', () => {
  it('has a function for every export of api.ts', () => {
    const mock = mockApi();
    expect(Object.keys(mock).sort()).toEqual(Object.keys(api).sort());
    for (const value of Object.values(mock)) {
      expect(value).toBeTypeOf('function');
    }
  });

  it('answers the note view calls with ok replies of their shape', async () => {
    const mock = mockApi();

    await expect(mock.noteCopy({ id: 1, format: 'markdown' })).resolves.toEqual({ ok: true });
    await expect(mock.speakerSuggestions({ id: 1 })).resolves.toEqual({ ok: true, attendees: [], earlier: [] });
    const deleted = await mock.noteDeleteRecording({ id: 1 });
    expect(deleted.ok && deleted.note).toMatchObject({ id: 1, recording_deleted: false, attendees: [] });
  });

  it('keeps recordingUrl working after vi.resetAllMocks()', () => {
    const mock = mockApi();
    vi.resetAllMocks();

    expect(mock.recordingUrl(7)).toBe(api.recordingUrl(7));
  });
});
