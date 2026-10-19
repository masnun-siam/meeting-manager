import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { RECORDER_STATUS_KEY, type RecorderStatus } from '../shared/messages';
import { recoveredName, recoverLeftovers } from './recover';
import { saveRecording } from './save';

vi.mock('./save', async (importOriginal) => ({
  ...(await importOriginal<typeof import('./save')>()),
  saveRecording: vi.fn(async () => ({ kind: 'saved' })),
  takenNames: vi.fn(async () => new Set<string>()),
}));

let stored: Record<string, unknown>;
let local: Record<string, unknown>;
let files: Map<string, number>;
let hasDocument: boolean;
const createDocument = vi.fn();
const closeDocument = vi.fn();

const status = () => stored[RECORDER_STATUS_KEY] as RecorderStatus;
const savedStarts = () => vi.mocked(saveRecording).mock.calls.map(([options]) => options.startedAt);

beforeEach(() => {
  stored = {};
  local = {};
  hasDocument = false;
  files = new Map();
  vi.mocked(saveRecording).mockClear();
  createDocument.mockReset().mockImplementation(async () => {
    hasDocument = true;
  });
  closeDocument.mockReset().mockImplementation(async () => {
    hasDocument = false;
  });
  const dir = {
    async *entries() {
      for (const [name, size] of files) yield [name, { kind: 'file', getFile: async () => ({ size }) }];
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
        set: async (items: Record<string, unknown>) => void Object.assign(stored, items),
      },
      local: { get: async (key: string) => ({ [key]: local[key] }) },
    },
    offscreen: {
      hasDocument: async () => hasDocument,
      createDocument,
      closeDocument,
      Reason: { USER_MEDIA: 'USER_MEDIA', BLOBS: 'BLOBS' },
    },
  });
});

afterEach(() => vi.unstubAllGlobals());

describe('recoveredName', () => {
  it('uses the local time of the start', () => {
    expect(recoveredName(new Date(2026, 9, 19, 14, 30).getTime())).toBe('meeting-2026-10-19_1430-recovered');
  });
});

describe('recoverLeftovers', () => {
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
