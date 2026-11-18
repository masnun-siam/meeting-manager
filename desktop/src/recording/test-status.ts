import type { Ok, RecorderStatus } from '../api';

/** A `recorder_status` reply for tests: idle on English unless told otherwise. */
export function statusReply(extra: Partial<RecorderStatus> = {}): Ok<RecorderStatus> {
  return {
    ok: true,
    state: 'idle',
    elapsed_s: 0,
    language: 'en',
    folder: null,
    notice: null,
    start_error: null,
    ...extra,
  };
}

/** A promise with its resolver, for replies a test answers out of order. */
export function deferred<T>() {
  let resolve: (value: T) => void = () => {};
  const promise = new Promise<T>((done) => {
    resolve = done;
  });
  return { promise, resolve };
}
