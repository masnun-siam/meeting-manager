import type { Err, Notice } from '../api';

// PRD desktop-app FR-2: the window names the System Settings pane that grants the missing permission.
export const MIC_PERMISSION = 'Allow Meeting in System Settings › Privacy & Security › Microphone';
export const AUDIO_PERMISSION =
  'Allow Meeting in System Settings › Privacy & Security › Screen & System Audio Recording';

export function permissionMessage(code: string): string | null {
  switch (code) {
    case 'permission_mic':
      return MIC_PERMISSION;
    case 'permission_audio':
      return AUDIO_PERMISSION;
    default:
      return null;
  }
}

/** The text for an error reply: the pane to open for a permission code, otherwise the reply's own message. */
export function errorText(reply: Err): string {
  return permissionMessage(reply.code) ?? reply.message;
}

/** The text for a call that threw or rejected instead of replying. */
export function failureText(error: unknown): string {
  return `Could not reach the Meeting app: ${error instanceof Error ? error.message : String(error)}`;
}

/** The Rust side writes the notice's words (D33); the UI never rebuilds them from `kind`. */
export function noticeText(notice: Notice): string {
  return notice.message;
}

/** A failed conversion keeps its WAV and can be retried (FR-7). */
export function canRetry(notice: Notice | null): notice is Notice & { folder: string } {
  return notice !== null && notice.kind === 'error' && notice.folder !== null;
}
