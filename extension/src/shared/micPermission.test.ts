import { afterEach, describe, expect, it, vi } from 'vitest';
import { micPermissionState, openMicPermissionPage } from './micPermission';

afterEach(() => {
  vi.unstubAllGlobals();
});

describe('micPermissionState', () => {
  it.each(['granted', 'denied', 'prompt'] as const)('returns %s', async (state) => {
    const query = vi.fn().mockResolvedValue({ state });
    vi.stubGlobal('navigator', { permissions: { query } });
    expect(await micPermissionState()).toBe(state);
    expect(query).toHaveBeenCalledWith({ name: 'microphone' });
  });
});

describe('openMicPermissionPage', () => {
  it('opens the permission page in a tab', async () => {
    const create = vi.fn().mockResolvedValue(undefined);
    const getURL = vi.fn((path: string) => `chrome-extension://id/${path}`);
    vi.stubGlobal('chrome', { tabs: { create }, runtime: { getURL } });
    await openMicPermissionPage();
    expect(getURL).toHaveBeenCalledWith('src/permission/permission.html');
    expect(create).toHaveBeenCalledWith({ url: 'chrome-extension://id/src/permission/permission.html' });
  });
});
