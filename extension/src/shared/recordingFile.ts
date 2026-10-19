// The one origin-private file per recording. Shared because the service worker reads it back to save and recover.

export const RECORDINGS_DIR = 'recordings';

export function recordingFileName(startedAt: number): string {
  return `${startedAt}.webm`;
}

/** Appends one chunk and closes the writer, so a crash keeps every chunk already appended. */
export async function appendChunk(startedAt: number, chunk: Blob): Promise<void> {
  const root = await navigator.storage.getDirectory();
  const dir = await root.getDirectoryHandle(RECORDINGS_DIR, { create: true });
  const handle = await dir.getFileHandle(recordingFileName(startedAt), { create: true });
  const writable = await handle.createWritable({ keepExistingData: true });
  try {
    await writable.seek((await handle.getFile()).size);
    await writable.write(chunk);
  } catch (error) {
    await writable.abort();
    throw error;
  }
  await writable.close();
}
