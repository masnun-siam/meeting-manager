export const MIC_PERMISSION_PAGE = 'src/permission/permission.html';

export async function micPermissionState(): Promise<PermissionState> {
  return (await navigator.permissions.query({ name: 'microphone' })).state;
}

export async function openMicPermissionPage(): Promise<void> {
  await chrome.tabs.create({ url: chrome.runtime.getURL(MIC_PERMISSION_PAGE) });
}
