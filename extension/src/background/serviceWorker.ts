// Service worker: the state owner. Listeners are registered at top level, as MV3 requires; the logic is in recordingControl.

import type { OffscreenEvent, PopupCommand } from '../shared/messages';
import { handleOffscreenEvent, handlePopupCommand, handleTabRemoved, serial } from './recordingControl';
import { recoverLeftovers } from './recover';

const POPUP_COMMANDS: ReadonlySet<string> = new Set(['start', 'pause', 'resume', 'stop']);
const OFFSCREEN_EVENTS: ReadonlySet<string> = new Set(['offscreen_started', 'offscreen_stopped', 'offscreen_error']);

function logFailure(error: unknown): void {
  console.error('Recording control failed', error);
}

chrome.runtime.onMessage.addListener((message: { type?: string }) => {
  if (typeof message?.type !== 'string') return false;
  if (POPUP_COMMANDS.has(message.type)) handlePopupCommand(message as PopupCommand).catch(logFailure);
  else if (OFFSCREEN_EVENTS.has(message.type)) handleOffscreenEvent(message as OffscreenEvent).catch(logFailure);
  return false;
});

chrome.tabs.onRemoved.addListener((tabId) => {
  handleTabRemoved(tabId).catch(logFailure);
});

// Only on browser start and extension install/reload, never on a service worker wake, so a recording in progress is left alone.
const recover = () => {
  serial(recoverLeftovers).catch(logFailure);
};
chrome.runtime.onStartup.addListener(recover);
chrome.runtime.onInstalled.addListener(recover);
