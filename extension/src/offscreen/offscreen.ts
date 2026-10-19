// Offscreen document: tab+mic capture and chunked MediaRecorder writes.
import type { OffscreenCommand } from '../shared/messages';
import { pauseRecording, resumeRecording, startRecording, stopRecording } from './recorder';

chrome.runtime.onMessage.addListener((message: OffscreenCommand) => {
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
  }
});
