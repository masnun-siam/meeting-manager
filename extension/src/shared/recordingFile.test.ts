import { afterEach, describe, expect, it, vi } from 'vitest';
import { appendChunk, RECORDINGS_DIR, recordingFileName } from './recordingFile';

function fakeStorage(existingSize: number) {
  const writable = { seek: vi.fn(), write: vi.fn(), close: vi.fn(), abort: vi.fn() };
  const fileHandle = {
    getFile: vi.fn(async () => ({ size: existingSize })),
    createWritable: vi.fn(async () => writable),
  };
  const dir = { getFileHandle: vi.fn(async () => fileHandle) };
  const root = { getDirectoryHandle: vi.fn(async () => dir) };
  vi.stubGlobal('navigator', { storage: { getDirectory: async () => root } });
  return { writable, fileHandle, dir, root };
}

afterEach(() => vi.unstubAllGlobals());

describe('recordingFile', () => {
  it('names the file after the start time', () => {
    expect(recordingFileName(1760866200000)).toBe('1760866200000.webm');
  });

  it('appends at the current size and closes', async () => {
    const { writable, fileHandle, dir, root } = fakeStorage(42);
    const chunk = new Blob(['x']);
    await appendChunk(1760866200000, chunk);
    expect(root.getDirectoryHandle).toHaveBeenCalledWith(RECORDINGS_DIR, { create: true });
    expect(dir.getFileHandle).toHaveBeenCalledWith('1760866200000.webm', { create: true });
    expect(fileHandle.createWritable).toHaveBeenCalledWith({ keepExistingData: true });
    expect(writable.seek).toHaveBeenCalledWith(42);
    expect(writable.write).toHaveBeenCalledWith(chunk);
    expect(writable.close).toHaveBeenCalled();
  });

  it('aborts the writer and rethrows when the write fails', async () => {
    const { writable } = fakeStorage(0);
    writable.write.mockRejectedValue(new Error('disk full'));
    await expect(appendChunk(1, new Blob(['x']))).rejects.toThrow('disk full');
    expect(writable.abort).toHaveBeenCalled();
    expect(writable.close).not.toHaveBeenCalled();
  });
});
