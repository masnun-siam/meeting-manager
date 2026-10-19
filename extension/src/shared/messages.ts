// Internal message contracts between the popup, the service worker and the offscreen document.
// This file is the one place for message types; later work extends these unions here.

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
