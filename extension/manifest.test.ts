import { createHash } from 'node:crypto';
import { describe, expect, it } from 'vitest';
import manifestExport from './manifest.config.ts';

// Chrome's ID rule: the first 32 hex chars of SHA-256 over the DER key, with 0-9a-f mapped to a-p.
function extensionIdFromKey(key: string): string {
  const hex = createHash('sha256').update(Buffer.from(key, 'base64')).digest('hex');
  return [...hex.slice(0, 32)].map((c) => String.fromCharCode(97 + Number.parseInt(c, 16))).join('');
}

async function loadManifest() {
  if (typeof manifestExport === 'function') throw new Error('manifest.config.ts must export a static manifest');
  return await manifestExport;
}

describe('manifest', () => {
  it('has the key for the fixed extension ID', async () => {
    const { key } = await loadManifest();

    expect(key).toBeDefined();
    expect(extensionIdFromKey(key ?? '')).toBe('obbaikcmcdooakogedoiiflflbbhpgdh');
  });

  it('asks for exactly the five permissions', async () => {
    const { permissions } = await loadManifest();

    expect(permissions).toEqual(['tabCapture', 'offscreen', 'downloads', 'nativeMessaging', 'storage']);
  });
});
