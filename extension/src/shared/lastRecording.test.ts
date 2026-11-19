import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import {
  CONTINUE_WINDOW_MS,
  canContinue,
  clearLastRecording,
  folderOf,
  LAST_RECORDING_KEY,
  type LastRecording,
  lastRecordingExists,
  readLastRecording,
  writeLastRecording,
} from './lastRecording';
import { RECORDING_CAP_MS } from './recordingState';

const flag = vi.hoisted(() => ({ on: true }));
vi.mock('./flags', () => ({ continueRecordingEnabled: () => flag.on }));

const STOPPED = new Date('2026-11-20T10:00:00Z').getTime();
const MIN = 60_000;

const last: LastRecording = {
  download_id: 5,
  path: '/Users/me/Downloads/Meetings/meeting-2026-11-20_1000/recording.webm',
  folder: 'meeting-2026-11-20_1000',
  ext: 'webm',
  recorded_ms: 30 * MIN,
  language: 'en',
  stopped_at: STOPPED,
  stop_reason: 'user',
  parts: 1,
};

const rule = (over: Partial<Parameters<typeof canContinue>[0]> = {}) =>
  canContinue({ last, phase: 'idle', now: STOPPED + MIN, exists: true, ...over });

describe('canContinue', () => {
  it('allows 1 minute after the stop', () => {
    expect(rule()).toBe(true);
  });

  it('allows exactly 2 hours after the stop and refuses at 2 h 1 min', () => {
    expect(rule({ now: STOPPED + CONTINUE_WINDOW_MS })).toBe(true);
    expect(rule({ now: STOPPED + CONTINUE_WINDOW_MS + MIN })).toBe(false);
  });

  it.each(['recording', 'paused', 'saving'] as const)('refuses while %s', (phase) => {
    expect(rule({ phase })).toBe(false);
  });

  it('refuses when the saved file no longer exists', () => {
    expect(rule({ exists: false })).toBe(false);
  });

  it('refuses at 3 hours recorded and allows at 2:59:59', () => {
    expect(rule({ last: { ...last, recorded_ms: RECORDING_CAP_MS } })).toBe(false);
    expect(rule({ last: { ...last, recorded_ms: RECORDING_CAP_MS - 1000 } })).toBe(true);
  });

  it('refuses when there is no last recording', () => {
    expect(rule({ last: null })).toBe(false);
  });

  it('applies to a later part, counted from its latest stop', () => {
    expect(rule({ last: { ...last, parts: 3 }, now: STOPPED + CONTINUE_WINDOW_MS })).toBe(true);
  });
});

describe('folderOf', () => {
  it('takes the folder name on either separator', () => {
    expect(folderOf(last.path)).toBe('meeting-2026-11-20_1000');
    expect(folderOf('C:\\d\\Meetings\\a-2\\recording.webm')).toBe('a-2');
  });
});

describe('storage', () => {
  let local: Record<string, unknown>;
  const get = vi.fn();
  const set = vi.fn();
  const remove = vi.fn();
  const search = vi.fn();

  beforeEach(() => {
    flag.on = true;
    local = {};
    get.mockReset().mockImplementation(async (key: string) => ({ [key]: local[key] }));
    set.mockReset().mockImplementation(async (items: Record<string, unknown>) => void Object.assign(local, items));
    remove.mockReset().mockImplementation(async (key: string) => void delete local[key]);
    search.mockReset();
    vi.stubGlobal('chrome', { storage: { local: { get, set, remove } }, downloads: { search } });
  });

  afterEach(() => vi.unstubAllGlobals());

  it('writes, reads back and clears the last recording', async () => {
    await writeLastRecording(last);
    expect(set).toHaveBeenCalledWith({ [LAST_RECORDING_KEY]: last });
    expect(await readLastRecording()).toEqual(last);
    await clearLastRecording();
    expect(await readLastRecording()).toBeNull();
  });

  it('reads a recovered recording with no stop reason', async () => {
    local[LAST_RECORDING_KEY] = { ...last, stop_reason: null };
    expect(await readLastRecording()).toEqual({ ...last, stop_reason: null });
  });

  it.each([
    ['nothing', undefined],
    ['a string', 'x'],
    ['a missing field', { ...last, download_id: undefined }],
    ['another extension', { ...last, ext: 'mp4' }],
    ['an unknown language', { ...last, language: 'fr' }],
    ['an unknown stop reason', { ...last, stop_reason: 'crash' }],
  ])('reads %s as null', async (_label, value) => {
    local[LAST_RECORDING_KEY] = value;
    expect(await readLastRecording()).toBeNull();
  });

  it('does not touch chrome.storage with the flag off', async () => {
    flag.on = false;
    local[LAST_RECORDING_KEY] = last;
    await writeLastRecording(last);
    await clearLastRecording();
    expect(await readLastRecording()).toBeNull();
    expect(get).not.toHaveBeenCalled();
    expect(set).not.toHaveBeenCalled();
    expect(remove).not.toHaveBeenCalled();
  });

  it('asks Chrome whether the download still exists', async () => {
    search.mockResolvedValueOnce([{ id: 5 }]).mockResolvedValueOnce([]);
    expect(await lastRecordingExists(last)).toBe(true);
    expect(await lastRecordingExists(last)).toBe(false);
    expect(search).toHaveBeenCalledWith({ id: 5, exists: true });
  });
});
