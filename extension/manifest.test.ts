import { createHash } from 'node:crypto';
import { describe, expect, it } from 'vitest';
import { buildManifest, flagFromEnv } from './manifest.config.ts';

// Chrome's ID rule: the first 32 hex chars of SHA-256 over the DER key, with 0-9a-f mapped to a-p.
function extensionIdFromKey(key: string): string {
  const hex = createHash('sha256').update(Buffer.from(key, 'base64')).digest('hex');
  return [...hex.slice(0, 32)].map((c) => String.fromCharCode(97 + Number.parseInt(c, 16))).join('');
}

const FIVE = ['tabCapture', 'offscreen', 'downloads', 'nativeMessaging', 'storage'];

describe('manifest', () => {
  it.each([false, true])('has the key for the fixed extension ID (flag %s)', (flag) => {
    const { key } = buildManifest(flag);

    expect(extensionIdFromKey(key)).toBe('obbaikcmcdooakogedoiiflflbbhpgdh');
  });

  it("flag off is today's manifest", () => {
    const { key, version, ...rest } = buildManifest(false);

    expect(rest).toEqual({
      manifest_version: 3,
      name: 'Meeting Manager',
      minimum_chrome_version: '116',
      permissions: FIVE,
      action: { default_popup: 'src/popup/index.html' },
      background: { service_worker: 'src/background/serviceWorker.ts', type: 'module' },
    });
  });

  it('flag on adds activeTab, Meet host access and the content script', () => {
    const m = buildManifest(true);

    expect(m.permissions).toEqual([...FIVE, 'activeTab']);
    expect(m).toMatchObject({
      host_permissions: ['https://meet.google.com/*'],
      content_scripts: [
        {
          matches: ['https://meet.google.com/*'],
          js: ['src/content/meet.ts'],
          all_frames: false,
          run_at: 'document_idle',
        },
      ],
    });
  });

  it('reads the flag as exactly 1', () => {
    expect(flagFromEnv({ FEATURE_MEETING_CONTEXT: '1' })).toBe(true);
    for (const v of [undefined, '', '0', 'true']) expect(flagFromEnv({ FEATURE_MEETING_CONTEXT: v })).toBe(false);
  });

  it('reads a named flag as exactly 1', () => {
    const name = 'FEATURE_CONTINUE_RECORDING';
    expect(flagFromEnv({ FEATURE_CONTINUE_RECORDING: '1' }, name)).toBe(true);
    for (const v of [undefined, '', '0', 'true'])
      expect(flagFromEnv({ FEATURE_CONTINUE_RECORDING: v }, name)).toBe(false);
    expect(flagFromEnv({ FEATURE_MEETING_CONTEXT: '1' }, name)).toBe(false);
  });
});
