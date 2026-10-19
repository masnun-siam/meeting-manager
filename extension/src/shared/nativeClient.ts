import { type HostMissing, NATIVE_HOST, type NativeRequest, type NativeResponse } from './native';

export const HOST_MISSING_MESSAGE = 'The meeting tool is not installed. Run: meeting install';

const CHROME_HOST_MISSING = 'Specified native messaging host not found';

function isReply(value: unknown): value is { ok: boolean } {
  if (typeof value !== 'object' || value === null || !('ok' in value) || typeof value.ok !== 'boolean') {
    return false;
  }
  if (value.ok) {
    return true;
  }
  return 'code' in value && typeof value.code === 'string' && 'message' in value && typeof value.message === 'string';
}

export async function sendNative<R extends NativeRequest>(request: R): Promise<NativeResponse<R> | HostMissing> {
  let reply: unknown;
  try {
    reply = await chrome.runtime.sendNativeMessage(NATIVE_HOST, request);
  } catch (error) {
    if (error instanceof Error && error.message.includes(CHROME_HOST_MISSING)) {
      return { ok: false, code: 'host_missing', message: HOST_MISSING_MESSAGE };
    }
    throw error;
  }
  if (!isReply(reply)) {
    throw new Error('Invalid reply from the meeting host');
  }
  // The host is trusted to answer with the shape for the type it was asked; #21 and #25 test that side.
  return reply as NativeResponse<R>;
}
