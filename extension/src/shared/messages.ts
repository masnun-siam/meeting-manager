// Internal message contracts between the popup, the service worker and the offscreen document.
// This file is the one place for message types; later work extends these unions here.

import type { ContextIdentity } from './native';
import type { RecordingState, StopReason } from './recordingState';

export type Language = 'bn' | 'en';

/** Popup → service worker. */
export type PopupCommand =
  | { type: 'start'; language: Language }
  /** Records the next part of the last recording; the language comes from `last_recording`. */
  | { type: 'continue' }
  | { type: 'pause' }
  | { type: 'resume' }
  | { type: 'stop' }
  /** Clears a Record tab message: the notice, the stop reason (idle only) or the mic warning (this recording). */
  | { type: 'dismiss'; what: DismissTarget };

/** The Record tab messages the service worker can clear. A send error is popup-local and never sent. */
export type DismissTarget = 'notice' | 'stop' | 'mic';

/** Service worker → offscreen document. */
export type OffscreenCommand =
  /** `baseMs`: the earlier parts' recorded time, for a continued part. */
  | { type: 'offscreen_start'; streamId: string; language: Language; startedAt: number; baseMs?: number }
  | { type: 'offscreen_pause' }
  | { type: 'offscreen_resume' }
  | { type: 'offscreen_stop' }
  | { type: 'offscreen_file_url'; startedAt: number };

/** Offscreen document → service worker. `tab_closed` is never sent from here: the service worker sees `tabs.onRemoved`. */
export type OffscreenEvent =
  | { type: 'offscreen_started'; micIncluded: boolean }
  | { type: 'offscreen_stopped'; startedAt: number; reason: StopReason }
  | { type: 'offscreen_error'; message: string };

/** Service worker → Meet content script (`chrome.tabs.sendMessage`). `context_start` is answered `{ ok: true }`, `context_stop` with the final `ContextUpdate` payload (no `type`). */
export type ContentCommand = { type: 'context_start'; startedAt: number } | { type: 'context_stop' };

/** Everyone Meet shows speaking from `at` (wall-clock epoch ms) on; empty means nobody. */
export interface SpeakingChange {
  at: number;
  names: string[];
}

/** Content script → service worker: what it has read so far. `participants` is the full union, `speaking` only the changes since the last batch. */
export interface ContextUpdate {
  type: 'context_update';
  meetingTitle?: string | null;
  participants?: { name: string; self: boolean }[];
  speaking?: SpeakingChange[];
  gaveUp?: boolean;
}

/** Content script → service worker on load; the reply is a `ContextHelloReply`. */
export interface ContextHello {
  type: 'context_hello';
}

export interface ContextHelloReply {
  recording: boolean;
  startedAt?: number;
}

/** Content script → service worker. */
export type ContentEvent = ContextUpdate | ContextHello;

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
  /** The mic warning was dismissed; `start` builds a status without it, so it resets per recording. */
  micWarningDismissed?: boolean;
  notice: Notice | null;
  /** The captured tab, so `tabs.onRemoved` still matches after the service worker slept. */
  tabId: number | null;
  /** Set while recording the next part of the last recording; `base_ms` is the earlier parts' total. */
  continuing: Continuing | null;
}

export interface Continuing {
  folder: string;
  /** This part's number: 2 for the first continued part. */
  part: number;
  base_ms: number;
}

export const RECORDER_STATUS_KEY = 'recorder';

/** `chrome.storage.session` key for the meeting context of the current recording; cleared after hand-off. */
export const MEETING_CONTEXT_KEY = 'meeting_context';

/** One recorded stretch: wall-clock bounds and where it starts in the recording file. */
export interface Segment {
  wallStart: number;
  wallEnd: number | null;
  offsetMs: number;
}

export interface MeetingContextState {
  identity: ContextIdentity;
  segments: Segment[];
  meetingTitle: string | null;
  participants: { name: string; self: boolean }[];
  speaking: { fromMs: number; toMs: number; names: string[] }[];
  openSpeaking: { at: number; names: string[] } | null;
  truncated: string[];
  capture: 'reading' | 'captured' | 'unavailable';
  reloadHint: boolean;
}
