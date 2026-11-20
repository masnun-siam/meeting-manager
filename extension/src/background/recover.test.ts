import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { CONTINUING_PARTS_KEY } from '../shared/continuingParts';
import { LAST_RECORDING_KEY } from '../shared/lastRecording';
import { RECORDER_STATUS_KEY, type RecorderStatus } from '../shared/messages';
import { RECORDING_CAP_MS } from '../shared/recordingState';
import { estimatedRecordedMs, recoveredName, recoverLeftovers } from './recover';
import { saveRecording } from './save';

const flag = vi.hoisted(() => ({ continueOn: false }));
vi.mock('../shared/flags', () => ({ continueRecordingEnabled: () => flag.continueOn }));

vi.mock('./save', async (importOriginal) => ({
  ...(await importOriginal<typeof import('./save')>()),
  saveRecording: vi.fn(async () => ({ kind: 'saved' })),
  takenNames: vi.fn(async () => new Set<string>()),
}));

let stored: Record<string, unknown>;
let local: Record<string, unknown>;
let files: Map<string, number>;
let modified: Map<string, number>;
let hasDocument: boolean;
let meetings: { id: number; filename: string }[];
const search = vi.fn();
const localGet = vi.fn();
const sessionSet = vi.fn();
const createDocument = vi.fn();
const closeDocument = vi.fn();

const status = () => stored[RECORDER_STATUS_KEY] as RecorderStatus;
const savedStarts = () => vi.mocked(saveRecording).mock.calls.map(([options]) => options.startedAt);

beforeEach(() => {
  flag.continueOn = false;
  stored = {};
  local = {};
  meetings = [];
  // Chrome matches the regex against each item's absolute filename.
  search
    .mockReset()
    .mockImplementation(async ({ filenameRegex }: { filenameRegex: string }) =>
      meetings.filter((item) => new RegExp(filenameRegex).test(item.filename)),
    );
  sessionSet.mockReset().mockImplementation(async (items: Record<string, unknown>) => {
    Object.assign(stored, items);
  });
  localGet.mockReset().mockImplementation(async (key: string) => ({ [key]: local[key] }));
  hasDocument = false;
  files = new Map();
  modified = new Map();
  vi.mocked(saveRecording).mockClear();
  createDocument.mockReset().mockImplementation(async () => {
    hasDocument = true;
  });
  closeDocument.mockReset().mockImplementation(async () => {
    hasDocument = false;
  });
  const dir = {
    async *entries() {
      for (const [name, size] of files)
        yield [name, { kind: 'file', getFile: async () => ({ size, lastModified: modified.get(name) ?? 0 }) }];
    },
    removeEntry: async (name: string) => void files.delete(name),
  };
  vi.stubGlobal('navigator', {
    storage: { getDirectory: async () => ({ getDirectoryHandle: async () => dir }) },
  });
  vi.stubGlobal('chrome', {
    storage: {
      session: {
        get: async (key: string) => ({ [key]: stored[key] }),
        set: sessionSet,
      },
      local: {
        get: localGet,
        set: async (items: Record<string, unknown>) => void Object.assign(local, items),
      },
    },
    downloads: { search },
    offscreen: {
      hasDocument: async () => hasDocument,
      createDocument,
      closeDocument,
      Reason: { USER_MEDIA: 'USER_MEDIA', BLOBS: 'BLOBS' },
    },
  });
});

afterEach(() => {
  vi.useRealTimers();
  vi.unstubAllGlobals();
});

describe('recoveredName', () => {
  it('uses the local time of the start', () => {
    expect(recoveredName(new Date(2026, 9, 19, 14, 30).getTime())).toBe('meeting-2026-10-19_1430-recovered');
  });
});

describe('estimatedRecordedMs', () => {
  it('is the time to the last write, never negative and never above the cap', () => {
    expect(estimatedRecordedMs(1000, 61_000)).toBe(60_000);
    expect(estimatedRecordedMs(1000, 500)).toBe(0);
    expect(estimatedRecordedMs(0, RECORDING_CAP_MS + 1)).toBe(RECORDING_CAP_MS);
  });
});

describe('recoverLeftovers', () => {
  it('asks the save to remember it as stopped at the recovery time, with no stop reason', async () => {
    const NOW = new Date('2026-11-20T10:00:00Z').getTime();
    vi.useFakeTimers();
    vi.setSystemTime(NOW);
    files.set('1000.webm', 5);
    modified.set('1000.webm', 1000 + 90_000);
    await recoverLeftovers();

    expect(saveRecording).toHaveBeenCalledWith(
      expect.objectContaining({
        startedAt: 1000,
        remember: { recordedMs: 90_000, stoppedAt: NOW, stopReason: null },
      }),
    );
  });

  it('saves each leftover in order with the stored language, then closes the offscreen document', async () => {
    local.language = 'en';
    files.set('1000.webm', 5);
    files.set('2000.webm', 5);
    await recoverLeftovers();

    expect(savedStarts()).toEqual([1000, 2000]);
    expect(saveRecording).toHaveBeenCalledWith(
      expect.objectContaining({ startedAt: 1000, language: 'en', process: true }),
    );
    expect(createDocument).toHaveBeenCalledWith(expect.objectContaining({ reasons: ['BLOBS'] }));
    expect(closeDocument).toHaveBeenCalled();
  });

  it('falls back to Bengali and writes the notice to the status', async () => {
    vi.mocked(saveRecording).mockResolvedValueOnce({ kind: 'setup', message: 'install the host' });
    files.set('1000.webm', 5);
    await recoverLeftovers();

    expect(vi.mocked(saveRecording).mock.calls[0][0].language).toBe('bn');
    expect(status().notice).toEqual({ kind: 'setup', message: 'install the host' });
  });

  it('skips the current recording', async () => {
    stored[RECORDER_STATUS_KEY] = {
      state: { phase: 'recording', startedAt: 1000, recordedMs: 0, resumedAt: 1000 },
      language: 'bn',
      micIncluded: true,
      notice: null,
      tabId: 1,
      continuing: null,
    };
    files.set('1000.webm', 5);
    files.set('2000.webm', 5);
    await recoverLeftovers();

    expect(savedStarts()).toEqual([2000]);
    expect(closeDocument).not.toHaveBeenCalled();
  });

  it('deletes an empty file without saving, and ignores other names', async () => {
    files.set('1000.webm', 0);
    files.set('notes.txt', 5);
    await recoverLeftovers();

    expect([...files.keys()]).toEqual(['notes.txt']);
    expect(saveRecording).not.toHaveBeenCalled();
    expect(createDocument).not.toHaveBeenCalled();
  });
});

describe('recoverLeftovers of a continued part', () => {
  const FOLDER = 'meeting-2026-01-01_0959';
  const RECORDING = `/Users/me/Downloads/Meetings/${FOLDER}/recording.webm`;
  const entry = { folder: FOLDER, part: 2, language: 'en' as const };
  const NOW = new Date('2026-11-20T10:00:00Z').getTime();
  const options = (call = 0) => vi.mocked(saveRecording).mock.calls[call][0];

  beforeEach(() => {
    flag.continueOn = true;
    vi.useFakeTimers();
    vi.setSystemTime(NOW);
    local.language = 'bn';
    local[CONTINUING_PARTS_KEY] = { 1000: entry };
    files.set('1000.webm', 5);
    modified.set('1000.webm', 1000 + 20_000);
  });

  it('saves the leftover as the next part of its meeting, in the part language, from the Chrome-reported file', async () => {
    meetings = [{ id: 7, filename: RECORDING }];
    local[LAST_RECORDING_KEY] = validLast();
    await recoverLeftovers();

    expect(saveRecording).toHaveBeenCalledTimes(1);
    expect(options()).toEqual({
      startedAt: 1000,
      name: recoveredName(1000),
      language: 'en',
      process: true,
      remember: { recordedMs: 80_000, stoppedAt: NOW, stopReason: null },
      continuing: { folder: FOLDER, part: 2, partMs: 20_000, recording: { path: RECORDING, downloadId: 7 } },
    });
    expect(search).toHaveBeenCalledWith({ filenameRegex: `Meetings/${FOLDER}/recording\\.webm$`, exists: true });
  });

  it('counts from zero when last_recording is another meeting, and never above the cap', async () => {
    meetings = [{ id: 7, filename: RECORDING }];
    local[LAST_RECORDING_KEY] = { ...validLast(), folder: 'other', recorded_ms: RECORDING_CAP_MS };
    await recoverLeftovers();
    expect(options().remember?.recordedMs).toBe(20_000);

    vi.mocked(saveRecording).mockClear();
    files.set('1000.webm', 5);
    local[LAST_RECORDING_KEY] = { ...validLast(), recorded_ms: RECORDING_CAP_MS - 5_000 };
    await recoverLeftovers();
    expect(options().remember?.recordedMs).toBe(RECORDING_CAP_MS);
  });

  it('treats a folder name as text, not a pattern', async () => {
    local[CONTINUING_PARTS_KEY] = { 1000: { ...entry, folder: 'meeting.(x)' } };
    meetings = [{ id: 7, filename: '/d/Meetings/meetingA(x)/recording.webm' }];
    await recoverLeftovers();
    expect(options().continuing).toBeUndefined();
  });

  it('recovers as today when the folder is gone; the save then drops the entry', async () => {
    meetings = [{ id: 8, filename: '/d/Meetings/meeting-2026-01-01_0959-2/recording.webm' }];
    await recoverLeftovers();

    expect(options()).toEqual(expect.objectContaining({ name: recoveredName(1000), language: 'bn', process: true }));
    expect(options().continuing).toBeUndefined();
  });

  it('handles a part and a plain leftover in order, one notice each', async () => {
    meetings = [{ id: 7, filename: RECORDING }];
    files.set('2000.webm', 5);
    vi.mocked(saveRecording)
      .mockResolvedValueOnce({ kind: 'error', message: 'first' })
      .mockResolvedValueOnce({ kind: 'too_short' });
    await recoverLeftovers();

    const notices = sessionSet.mock.calls.map(([items]) => (items[RECORDER_STATUS_KEY] as RecorderStatus).notice);
    expect(savedStarts()).toEqual([1000, 2000]);
    expect(options(0).continuing?.part).toBe(2);
    expect(options(1).continuing).toBeUndefined();
    expect(options(1).language).toBe('bn');
    expect(notices).toEqual([{ kind: 'error', message: 'first' }, { kind: 'too_short' }]);
  });

  it('drops the entry of a deleted empty file and of a part with no file, but keeps the current recording', async () => {
    files.set('1000.webm', 0);
    files.set('3000.webm', 5);
    local[CONTINUING_PARTS_KEY] = { 1000: entry, 2000: entry, 3000: entry, 4000: entry };
    stored[RECORDER_STATUS_KEY] = {
      state: { phase: 'recording', startedAt: 4000, recordedMs: 0, resumedAt: 4000 },
      language: 'bn',
      micIncluded: true,
      notice: null,
      tabId: 1,
      continuing: { folder: FOLDER, part: 2, base_ms: 0 },
    };
    files.set('4000.webm', 5);
    await recoverLeftovers();

    expect(local[CONTINUING_PARTS_KEY]).toEqual({ 3000: entry, 4000: entry });
    expect(savedStarts()).toEqual([3000]);
  });

  it('keeps the file and the entry when the save fails, for the next start', async () => {
    meetings = [{ id: 7, filename: RECORDING }];
    vi.mocked(saveRecording).mockResolvedValueOnce({ kind: 'error', message: 'Could not save' });
    await recoverLeftovers();

    expect(files.has('1000.webm')).toBe(true);
    expect(local[CONTINUING_PARTS_KEY]).toEqual({ 1000: entry });
  });

  it('with the flag off reads no entry and recovers as today', async () => {
    flag.continueOn = false;
    meetings = [{ id: 7, filename: RECORDING }];
    await recoverLeftovers();

    expect(localGet.mock.calls.map(([key]) => key)).toEqual(['language']);
    expect(search).not.toHaveBeenCalled();
    expect(options()).toEqual(expect.objectContaining({ name: recoveredName(1000), language: 'bn' }));
    expect(options().continuing).toBeUndefined();
    expect(local[CONTINUING_PARTS_KEY]).toEqual({ 1000: entry });
  });

  function validLast() {
    return {
      download_id: 7,
      path: RECORDING,
      folder: FOLDER,
      ext: 'webm',
      recorded_ms: 60_000,
      language: 'en',
      stopped_at: 0,
      stop_reason: 'user',
      parts: 1,
    };
  }
});
