import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { NATIVE_HOST, type NativeRequest } from './native';
import { HOST_MISSING_MESSAGE, sendNative } from './nativeClient';

const sendNativeMessage = vi.fn();

beforeEach(() => {
  sendNativeMessage.mockReset();
  vi.stubGlobal('chrome', { runtime: { sendNativeMessage } });
});

afterEach(() => {
  vi.unstubAllGlobals();
});

describe('sendNative', () => {
  const cases: [NativeRequest, object][] = [
    [
      { type: 'process', path: '/tmp/a.webm', language: 'en' },
      { ok: true, job_id: 'j1' },
    ],
    [
      { type: 'process_part', path: '/tmp/m/recording-part-2.webm', language: 'bn' },
      { ok: true, job_id: 'j1' },
    ],
    [
      { type: 'retry', job_id: 'j1' },
      { ok: true, job_id: 'j2' },
    ],
    [
      { type: 'list_jobs', limit: 10 },
      { ok: true, jobs: [] },
    ],
    [{ type: 'open', job_id: 'j1', what: 'summary' }, { ok: true }],
    [{ type: 'health' }, { ok: true, checks: [{ name: 'ffmpeg', ok: true, fix: '' }] }],
  ];

  it.each(cases)('sends a %j request unchanged and returns the reply', async (request, reply) => {
    sendNativeMessage.mockResolvedValue(reply);

    await expect(sendNative(request)).resolves.toEqual(reply);
    expect(sendNativeMessage).toHaveBeenCalledWith(NATIVE_HOST, request);
  });

  it('returns a wire error instead of throwing', async () => {
    const reply = { ok: false, code: 'already_processing', message: 'busy' };
    sendNativeMessage.mockResolvedValue(reply);

    await expect(sendNative({ type: 'retry', job_id: 'j1' })).resolves.toEqual(reply);
  });

  it("maps Chrome's missing-host error to host_missing", async () => {
    sendNativeMessage.mockRejectedValue(new Error('Specified native messaging host not found.'));

    await expect(sendNative({ type: 'health' })).resolves.toEqual({
      ok: false,
      code: 'host_missing',
      message: HOST_MISSING_MESSAGE,
    });
  });

  it('rethrows any other rejection unchanged', async () => {
    const error = new Error('Native host has exited.');
    sendNativeMessage.mockRejectedValue(error);

    await expect(sendNative({ type: 'health' })).rejects.toBe(error);
  });

  it.each([null, {}, { ok: 'yes' }, { ok: false }])('rejects the malformed reply %j', async (reply) => {
    sendNativeMessage.mockResolvedValue(reply);

    await expect(sendNative({ type: 'health' })).rejects.toThrow('Invalid reply from the meeting host');
  });
});
