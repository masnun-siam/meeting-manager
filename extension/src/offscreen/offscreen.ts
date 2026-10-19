// Offscreen document: tab+mic capture and chunked MediaRecorder writes.
import type { OffscreenCommand } from '../shared/messages';
import { getRecordingFile } from '../shared/recordingFile';
import { pauseRecording, resumeRecording, startRecording, stopRecording } from './recorder';

chrome.runtime.onMessage.addListener((message: OffscreenCommand, _sender, sendResponse) => {
  switch (message.type) {
    case 'offscreen_start':
      void startRecording(message);
      break;
    case 'offscreen_pause':
      pauseRecording();
      break;
    case 'offscreen_resume':
      resumeRecording();
      break;
    case 'offscreen_stop':
      void stopRecording('user');
      break;
    case 'offscreen_file_url':
      // The blob URL lives as long as this document; the service worker closes it after the download.
      getRecordingFile(message.startedAt).then(
        (file) => sendResponse({ url: URL.createObjectURL(file) }),
        (error) => sendResponse({ error: error instanceof Error ? error.message : String(error) }),
      );
      return true;
  }
});
