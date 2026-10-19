// Internal message contracts between the popup, the service worker and the offscreen document.
// This file is the one place for message types; later work extends these unions here.

import type { RecordingState } from './recordingState';

export type Language = 'bn' | 'en';

/** Popup → service worker. */
export type PopupCommand =
  | { type: 'start'; language: Language }
  | { type: 'pause' }
  | { type: 'resume' }
  | { type: 'stop' };

/** Service worker → offscreen document. */
export type OffscreenCommand =
  | { type: 'offscreen_start'; streamId: string; language: Language }
  | { type: 'offscreen_pause' }
  | { type: 'offscreen_resume' }
  | { type: 'offscreen_stop' };

/** The Record tab's last message after a save or hand-off (#26 writes these). */
export type Notice =
  | { kind: 'saved' }
  | { kind: 'too_short' }
  | { kind: 'setup'; message: string }
  | { kind: 'error'; message: string };

/** The service worker's recorder status in `chrome.storage.session`, read by the popup. */
export interface RecorderStatus {
  state: RecordingState;
  language: Language;
  /** null until the offscreen document has started; false means tab audio only. */
  micIncluded: boolean | null;
  notice: Notice | null;
}

export const RECORDER_STATUS_KEY = 'recorder';
