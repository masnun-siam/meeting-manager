import { afterEach, describe, expect, it, vi } from 'vitest';
import { continueRecordingEnabled, meetingContextEnabled } from './flags';

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

describe('continueRecordingEnabled', () => {
  afterEach(() => vi.unstubAllGlobals());

  it('is off when the build define is absent', () => {
    expect(continueRecordingEnabled()).toBe(false);
  });

  it('is on when the build defines it true', () => {
    vi.stubGlobal('__FEATURE_CONTINUE_RECORDING__', true);
    expect(continueRecordingEnabled()).toBe(true);
  });

  it('is not turned on by the meeting-context define', () => {
    vi.stubGlobal('__FEATURE_MEETING_CONTEXT__', true);
    expect(continueRecordingEnabled()).toBe(false);
  });
});
