import { vi } from 'vitest';

/**
 * jsdom has no media playback: one fake element state, read and written through the prototype.
 * Call `stubMedia()` in `beforeEach`, and unmount (`cleanup()`) before `vi.restoreAllMocks()`.
 */
export const media = { paused: true, duration: Number.NaN, time: 0 };

export function stubMedia() {
  media.paused = true;
  media.duration = Number.NaN;
  media.time = 0;
  const proto = HTMLMediaElement.prototype;
  Object.defineProperty(proto, 'paused', { configurable: true, get: () => media.paused });
  Object.defineProperty(proto, 'duration', { configurable: true, get: () => media.duration });
  Object.defineProperty(proto, 'currentTime', {
    configurable: true,
    get: () => media.time,
    set: (value: number) => {
      media.time = value;
    },
  });
  vi.spyOn(proto, 'play').mockImplementation(function (this: HTMLMediaElement) {
    media.paused = false;
    this.dispatchEvent(new Event('play'));
    return Promise.resolve();
  });
  vi.spyOn(proto, 'pause').mockImplementation(function (this: HTMLMediaElement) {
    if (media.paused) return;
    media.paused = true;
    this.dispatchEvent(new Event('pause'));
  });
  vi.spyOn(proto, 'load').mockImplementation(() => {});
}
