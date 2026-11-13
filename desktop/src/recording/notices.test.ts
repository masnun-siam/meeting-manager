import { describe, expect, it } from 'vitest';
import { canRetry, errorText, failureText, noticeText, permissionMessage } from './notices';

describe('notices', () => {
  it('names the System Settings pane for each permission code', () => {
    expect(permissionMessage('permission_mic')).toBe(
      'Allow Meeting in System Settings › Privacy & Security › Microphone',
    );
    expect(permissionMessage('permission_audio')).toBe(
      'Allow Meeting in System Settings › Privacy & Security › Screen & System Audio Recording',
    );
    expect(permissionMessage('already_recording')).toBeNull();
  });

  it('uses the pane for permission errors and the reply message otherwise', () => {
    expect(errorText({ ok: false, code: 'permission_mic', message: 'mic denied' })).toBe(
      'Allow Meeting in System Settings › Privacy & Security › Microphone',
    );
    expect(errorText({ ok: false, code: 'capture_failed', message: 'Capture failed: device gone' })).toBe(
      'Capture failed: device gone',
    );
  });

  it('describes a rejected call', () => {
    expect(failureText(new Error('boom'))).toBe('Could not reach the Meeting app: boom');
    expect(failureText('down')).toBe('Could not reach the Meeting app: down');
  });

  it('shows the notice message as written, whatever its kind', () => {
    expect(noticeText({ kind: 'saved', message: 'Saved, sent for processing', folder: null })).toBe(
      'Saved, sent for processing',
    );
    expect(noticeText({ kind: 'recovered', message: 'Recovered 2 recording(s)', folder: null })).toBe(
      'Recovered 2 recording(s)',
    );
  });

  it('offers Retry only for an error notice with a folder', () => {
    expect(canRetry({ kind: 'error', message: 'ffmpeg failed', folder: '/r/m' })).toBe(true);
    expect(canRetry({ kind: 'error', message: 'Capture failed', folder: null })).toBe(false);
    expect(canRetry({ kind: 'saved', message: 'Saved', folder: '/r/m' })).toBe(false);
    expect(canRetry(null)).toBe(false);
  });
});
