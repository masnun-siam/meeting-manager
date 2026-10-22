import { afterEach, describe, expect, it, vi } from 'vitest';
import { meetingContextEnabled } from './flags';

describe('meetingContextEnabled', () => {
  afterEach(() => vi.unstubAllGlobals());

  it('is off when the build define is absent', () => {
    expect(meetingContextEnabled()).toBe(false);
  });

  it('is on when the build defines it true', () => {
    vi.stubGlobal('__FEATURE_MEETING_CONTEXT__', true);
    expect(meetingContextEnabled()).toBe(true);
  });
});
