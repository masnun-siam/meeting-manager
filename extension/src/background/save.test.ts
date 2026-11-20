import { beforeEach, describe, expect, it, vi } from 'vitest';
import { CONTINUING_PARTS_KEY } from '../shared/continuingParts';
import { LAST_RECORDING_KEY } from '../shared/lastRecording';
import { sendNative } from '../shared/nativeClient';
import { movedNotice, nextFreeName, recordingName, saveRecording, takenNames } from './save';

vi.mock('../shared/nativeClient', () => ({ sendNative: vi.fn() }));
const flag = vi.hoisted(() => ({ continueOn: false }));
vi.mock('../shared/flags', () => ({ continueRecordingEnabled: () => flag.continueOn }));
const removeEntry = vi.fn();
vi.mock('../shared/recordingFile', () => ({ deleteRecordingFile: (...a: unknown[]) => removeEntry(...a) }));

const STARTED = new Date(2026, 9, 19, 14, 30, 59).getTime();
const opts = { startedAt: STARTED, name: 'meeting-2026-10-19_1430', language: 'en' as const, process: true };

let listener: (delta: unknown) => void;
const download = vi.fn();
const search = vi.fn();
const sendMessage = vi.fn();
const localSet = vi.fn();
let local: Record<string, unknown>;

/** `download` resolves with id 5, then Chrome reports `delta` for it. */
function finishWith(delta: Record<string, unknown>) {
  download.mockImplementation(async () => {
    setTimeout(() => listener({ id: 5, ...delta }), 0);
    return 5;
  });
}

beforeEach(() => {
  for (const m of [download, search, sendMessage, removeEntry, localSet, vi.mocked(sendNative)]) m.mockReset();
  flag.continueOn = false;
  local = {};
  localSet.mockImplementation(async (items: Record<string, unknown>) => {
    Object.assign(local, items);
  });
  sendMessage.mockResolvedValue({ url: 'blob:x' });
  removeEntry.mockResolvedValue(undefined);
  search.mockResolvedValue([{ filename: '/Users/me/Downloads/Meetings/meeting-2026-10-19_1430/recording.webm' }]);
  vi.stubGlobal('chrome', {
    runtime: { sendMessage },
    storage: { local: { set: localSet, get: async (key: string) => ({ [key]: local[key] }) } },
    downloads: {
      download,
      search,
      onChanged: {
        addListener: (fn: typeof listener) => {
          listener = fn;
        },
        removeListener: vi.fn(),
      },
    },
  });
});

describe('naming', () => {
  it('formats the local start time with zero padding', () => {
    expect(recordingName(STARTED)).toBe('meeting-2026-10-19_1430');
    expect(recordingName(new Date(2026, 0, 2, 3, 4).getTime())).toBe('meeting-2026-01-02_0304');
  });

  it('skips taken names', () => {
    expect(nextFreeName('a', new Set())).toBe('a');
    expect(nextFreeName('a', new Set(['a']))).toBe('a-2');
    expect(nextFreeName('a', new Set(['a', 'a-2']))).toBe('a-3');
  });

  it('reads taken names from the download history', async () => {
    search.mockResolvedValue([
      { filename: '/d/Meetings/a/recording.webm' },
      { filename: 'C:\\d\\Meetings\\a-2\\recording.webm' },
    ]);
    expect(await takenNames('a')).toEqual(new Set(['a', 'a-2']));
    expect(search).toHaveBeenCalledWith({ filenameRegex: 'Meetings/a(-\\d+)?/recording\\.webm$', exists: true });
  });
});

describe('saveRecording', () => {
  it('downloads, deletes the origin-private file and sends process', async () => {
    finishWith({ state: { current: 'complete' } });
    vi.mocked(sendNative).mockResolvedValue({ ok: true, job_id: 'j1' });
    expect(await saveRecording(opts)).toEqual({ kind: 'saved' });
    expect(download).toHaveBeenCalledWith({
      url: 'blob:x',
      filename: 'Meetings/meeting-2026-10-19_1430/recording.webm',
      saveAs: false,
      conflictAction: 'uniquify',
    });
    expect(removeEntry).toHaveBeenCalledWith(STARTED);
    expect(sendNative).toHaveBeenCalledWith({
      type: 'process',
      path: '/Users/me/Downloads/Meetings/meeting-2026-10-19_1430/recording.webm',
      language: 'en',
    });
  });

  it('sends the context with process when given', async () => {
    finishWith({ state: { current: 'complete' } });
    vi.mocked(sendNative).mockResolvedValue({ ok: true, job_id: 'j1' });
    const context = {
      version: 1 as const,
      platform: 'other' as const,
      url: null,
      meeting_id: null,
      tab_title: null,
      meeting_title: null,
      started_at_ms: 1,
      utc_offset_min: 0,
      time_zone: null,
      participants: [],
      speaking: [],
      truncated: [],
    };
    await saveRecording({ ...opts, context });
    expect(sendNative).toHaveBeenCalledWith(expect.objectContaining({ type: 'process', context }));
  });

  it('saves but does not process a short recording', async () => {
    finishWith({ state: { current: 'complete' } });
    expect(await saveRecording({ ...opts, process: false })).toEqual({ kind: 'too_short' });
    expect(sendNative).not.toHaveBeenCalled();
    expect(removeEntry).toHaveBeenCalled();
  });

  it.each([
    ['host_missing', 'setup'],
    ['not_configured', 'setup'],
    ['invalid_path', 'error'],
  ] as const)('maps %s to a %s notice', async (code, kind) => {
    finishWith({ state: { current: 'complete' } });
    vi.mocked(sendNative).mockResolvedValue({ ok: false, code, message: 'msg' });
    expect(await saveRecording(opts)).toEqual({ kind, message: 'msg' });
  });

  it('reports a thrown native error', async () => {
    finishWith({ state: { current: 'complete' } });
    vi.mocked(sendNative).mockRejectedValue(new Error('boom'));
    expect(await saveRecording(opts)).toEqual({
      kind: 'error',
      message: 'Could not reach the meeting tool: boom',
    });
  });

  it('keeps the file and says the disk is full when the download is interrupted', async () => {
    finishWith({ state: { current: 'interrupted' }, error: { current: 'FILE_NO_SPACE' } });
    const notice = await saveRecording(opts);
    expect(notice).toMatchObject({ kind: 'error', message: expect.stringContaining('Disk full') });
    expect(removeEntry).not.toHaveBeenCalled();
    expect(sendNative).not.toHaveBeenCalled();
  });

  it('still processes when deleting the origin-private file fails', async () => {
    finishWith({ state: { current: 'complete' } });
    removeEntry.mockRejectedValue(new Error('locked'));
    vi.mocked(sendNative).mockResolvedValue({ ok: true, job_id: 'j1' });
    expect(await saveRecording(opts)).toEqual({ kind: 'saved' });
  });

  it('names other interruptions', async () => {
    finishWith({ state: { current: 'interrupted' }, error: { current: 'NETWORK_FAILED' } });
    expect(await saveRecording(opts)).toMatchObject({ message: expect.stringContaining('(NETWORK_FAILED)') });
  });

  it('keeps the file when the offscreen document has none', async () => {
    sendMessage.mockResolvedValue({ error: 'gone' });
    expect(await saveRecording(opts)).toMatchObject({ kind: 'error' });
    expect(download).not.toHaveBeenCalled();
  });
});

describe('saveRecording remembers the last recording', () => {
  const PATH = '/Users/me/Downloads/Meetings/meeting-2026-10-19_1430/recording.webm';
  const remember = { recordedMs: 5_000, stoppedAt: STARTED + 5_000, stopReason: 'user' as const };

  beforeEach(() => {
    flag.continueOn = true;
  });

  it('stores it after a complete download, even when too short to process', async () => {
    finishWith({ state: { current: 'complete' } });
    expect(await saveRecording({ ...opts, process: false, remember })).toEqual({ kind: 'too_short' });
    expect(localSet).toHaveBeenCalledWith({
      [LAST_RECORDING_KEY]: {
        download_id: 5,
        path: PATH,
        folder: 'meeting-2026-10-19_1430',
        ext: 'webm',
        recorded_ms: 5_000,
        language: 'en',
        stopped_at: STARTED + 5_000,
        stop_reason: 'user',
        parts: 1,
      },
    });
    expect(localSet.mock.invocationCallOrder[0]).toBeLessThan(removeEntry.mock.invocationCallOrder[0]);
  });

  it('stores nothing when the download is interrupted', async () => {
    finishWith({ state: { current: 'interrupted' }, error: { current: 'FILE_NO_SPACE' } });
    await saveRecording({ ...opts, remember });
    expect(localSet).not.toHaveBeenCalled();
  });

  it('stores nothing with the flag off', async () => {
    flag.continueOn = false;
    finishWith({ state: { current: 'complete' } });
    await saveRecording({ ...opts, process: false, remember });
    expect(localSet).not.toHaveBeenCalled();
  });

  it('stores nothing without remember', async () => {
    finishWith({ state: { current: 'complete' } });
    await saveRecording({ ...opts, process: false });
    expect(localSet).not.toHaveBeenCalled();
  });

  it('still saves and processes when the write fails, logging a fixed text', async () => {
    const error = vi.spyOn(console, 'error').mockImplementation(() => undefined);
    finishWith({ state: { current: 'complete' } });
    localSet.mockRejectedValue(new Error(`quota ${PATH}`));
    vi.mocked(sendNative).mockResolvedValue({ ok: true, job_id: 'j1' });
    expect(await saveRecording({ ...opts, remember })).toEqual({ kind: 'saved' });
    expect(error).toHaveBeenCalledWith('Could not store the last recording');
    expect(sendNative).toHaveBeenCalled();
    error.mockRestore();
  });
});

describe('saveRecording a continued part', () => {
  const DIR = '/Users/me/Downloads/Meetings/meeting-2026-10-19_0900';
  const RECORDING = `${DIR}/recording.webm`;
  const PART = `${DIR}/recording-part-2.webm`;
  const NEW = '/Users/me/Downloads/Meetings/meeting-2026-10-19_1430/recording.webm';
  const continuing = {
    folder: 'meeting-2026-10-19_0900',
    part: 2,
    partMs: 8_000,
    recording: { path: RECORDING, downloadId: 1 },
  };
  const part = {
    ...opts,
    continuing,
    remember: { recordedMs: 12_000, stoppedAt: STARTED + 8_000, stopReason: 'user' as const },
  };
  const ENTRY = { [STARTED]: { folder: continuing.folder, part: 2, language: 'en' } };

  /** The earlier recording exists unless `moved`; each download completes at the next path of `saved`. */
  function chromeHas({ moved = false, saved = [PART] }: { moved?: boolean; saved?: string[] } = {}) {
    const paths = [...saved];
    let current = '';
    search.mockImplementation(async (query: { id: number; exists?: boolean }) => {
      if (query.exists) return moved ? [] : [{ id: 1, filename: RECORDING }];
      return [{ filename: current }];
    });
    download.mockImplementation(async () => {
      current = paths.shift() ?? '';
      setTimeout(() => listener({ id: 5, state: { current: 'complete' } }), 0);
      return 5;
    });
  }

  beforeEach(() => {
    flag.continueOn = true;
    local[CONTINUING_PARTS_KEY] = ENTRY;
  });

  it('saves recording-part-N beside recording.webm and sends process_part', async () => {
    chromeHas();
    vi.mocked(sendNative).mockResolvedValue({ ok: true, job_id: 'j1' });
    expect(await saveRecording(part)).toEqual({ kind: 'saved' });
    expect(search).toHaveBeenCalledWith({ id: 1, exists: true });
    expect(download).toHaveBeenCalledTimes(1);
    expect(download).toHaveBeenCalledWith(
      expect.objectContaining({ filename: 'Meetings/meeting-2026-10-19_0900/recording-part-2.webm' }),
    );
    expect(sendNative).toHaveBeenCalledWith({ type: 'process_part', path: PART, language: 'en' });
    expect(removeEntry).toHaveBeenCalledWith(STARTED);
    expect(local[CONTINUING_PARTS_KEY]).toEqual({});
  });

  it('rewrites the last recording with the part count and total, keeping recording.webm', async () => {
    chromeHas();
    vi.mocked(sendNative).mockResolvedValue({ ok: true, job_id: 'j1' });
    await saveRecording(part);
    expect(local[LAST_RECORDING_KEY]).toEqual({
      download_id: 1,
      path: RECORDING,
      folder: 'meeting-2026-10-19_0900',
      ext: 'webm',
      recorded_ms: 12_000,
      language: 'en',
      stopped_at: STARTED + 8_000,
      stop_reason: 'user',
      parts: 2,
    });
  });

  it('saves but does not hand off a total under 10 s', async () => {
    chromeHas();
    expect(await saveRecording({ ...part, process: false })).toEqual({ kind: 'too_short' });
    expect(sendNative).not.toHaveBeenCalled();
    expect(local[CONTINUING_PARTS_KEY]).toEqual({});
  });

  it('keeps the file and the link when the download is interrupted', async () => {
    search.mockResolvedValue([{ id: 1, filename: RECORDING }]);
    finishWith({ state: { current: 'interrupted' }, error: { current: 'FILE_NO_SPACE' } });
    expect(await saveRecording(part)).toMatchObject({ kind: 'error', message: expect.stringContaining('Disk full') });
    expect(removeEntry).not.toHaveBeenCalled();
    expect(local[CONTINUING_PARTS_KEY]).toEqual(ENTRY);
    expect(sendNative).not.toHaveBeenCalled();
  });

  it.each([
    ['bad_request', { kind: 'error', message: 'msg' }],
    ['host_missing', { kind: 'setup', message: 'msg' }],
    ['not_configured', { kind: 'setup', message: 'msg' }],
  ] as const)('maps %s from an old or missing tool', async (code, notice) => {
    chromeHas();
    vi.mocked(sendNative).mockResolvedValue({ ok: false, code, message: 'msg' });
    expect(await saveRecording(part)).toEqual(notice);
  });

  it('saves a new meeting when the folder was moved, processing the part alone from 10 s', async () => {
    chromeHas({ moved: true, saved: [NEW] });
    vi.mocked(sendNative).mockResolvedValue({ ok: true, job_id: 'j1' });
    const notice = await saveRecording({ ...part, continuing: { ...continuing, partMs: 10_000 } });
    expect(notice).toEqual({
      kind: 'error',
      message: 'Could not add to meeting-2026-10-19_0900: it was moved; saved as a new meeting.',
    });
    expect(notice).toEqual(movedNotice(continuing.folder));
    expect(download).toHaveBeenCalledTimes(1);
    expect(download).toHaveBeenCalledWith(
      expect.objectContaining({ filename: 'Meetings/meeting-2026-10-19_1430/recording.webm' }),
    );
    expect(sendNative).toHaveBeenCalledWith({ type: 'process', path: NEW, language: 'en' });
    expect(local[LAST_RECORDING_KEY]).toMatchObject({ path: NEW, recorded_ms: 10_000, parts: 1 });
    expect(local[CONTINUING_PARTS_KEY]).toEqual({});
    expect(removeEntry).toHaveBeenCalledWith(STARTED);
  });

  it('does not process a moved part shorter than 10 s alone, even when the total is longer', async () => {
    chromeHas({ moved: true, saved: [NEW] });
    expect(await saveRecording(part)).toEqual(movedNotice(continuing.folder));
    expect(sendNative).not.toHaveBeenCalled();
  });

  it('saves a new meeting when the last recording was no longer known', async () => {
    chromeHas({ saved: [NEW] });
    expect(await saveRecording({ ...part, continuing: { ...continuing, recording: null } })).toEqual(
      movedNotice(continuing.folder),
    );
    expect(search).not.toHaveBeenCalledWith({ id: 1, exists: true });
    expect(download).toHaveBeenCalledWith(
      expect.objectContaining({ filename: 'Meetings/meeting-2026-10-19_1430/recording.webm' }),
    );
  });

  it.each([
    ['a uniquified name', `${DIR}/recording-part-2 (1).webm`],
    ['another directory', '/Users/me/Downloads/Meetings/meeting-2026-10-19_0900 (1)/recording-part-2.webm'],
  ])('treats %s as a moved folder and saves a new meeting', async (_name, stray) => {
    chromeHas({ saved: [stray, NEW] });
    vi.mocked(sendNative).mockResolvedValue({ ok: true, job_id: 'j1' });
    expect(await saveRecording({ ...part, continuing: { ...continuing, partMs: 10_000 } })).toEqual(
      movedNotice(continuing.folder),
    );
    expect(download).toHaveBeenCalledTimes(2);
    expect(sendNative).toHaveBeenCalledTimes(1);
    expect(sendNative).toHaveBeenCalledWith({ type: 'process', path: NEW, language: 'en' });
    expect(local[LAST_RECORDING_KEY]).toMatchObject({ path: NEW, parts: 1 });
  });

  it('keeps the file when the new-meeting fallback download fails too', async () => {
    search.mockResolvedValue([]);
    finishWith({ state: { current: 'interrupted' }, error: { current: 'NETWORK_FAILED' } });
    expect(await saveRecording(part)).toMatchObject({
      kind: 'error',
      message: expect.stringContaining('(NETWORK_FAILED)'),
    });
    expect(removeEntry).not.toHaveBeenCalled();
    expect(local[CONTINUING_PARTS_KEY]).toEqual(ENTRY);
  });
});
