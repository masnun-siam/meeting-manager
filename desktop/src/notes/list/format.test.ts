import { describe, expect, it } from 'vitest';
import { formatDuration, formatRecorded, languageLabel, sourceLabel, statusText } from './format';

describe('formatRecorded', () => {
  it('reads the time as local wall-clock time', () => {
    expect(formatRecorded('2026-11-12T10:05')).toBe(
      new Date(2026, 10, 12, 10, 5).toLocaleString(undefined, { dateStyle: 'medium', timeStyle: 'short' }),
    );
  });

  it('shows anything else as it came', () => {
    expect(formatRecorded('yesterday')).toBe('yesterday');
  });
});

describe('formatDuration', () => {
  it.each([
    [null, null],
    [0, '0:00'],
    [59, '0:59'],
    [61, '1:01'],
    [3599, '59:59'],
    [3600, '1:00:00'],
    [3725, '1:02:05'],
  ])('%s s is %s', (seconds, text) => {
    expect(formatDuration(seconds)).toBe(text);
  });
});

describe('labels', () => {
  it('names languages and sources', () => {
    expect(languageLabel('bn')).toBe('Bengali');
    expect(languageLabel('en')).toBe('English');
    expect(languageLabel(null)).toBeNull();
    expect(sourceLabel('browser')).toBe('Browser');
    expect(sourceLabel('import')).toBe('Import');
    expect(sourceLabel('desktop')).toBe('Desktop');
  });

  it('gives the status word, the stage while processing', () => {
    expect(statusText({ status: 'done', stage: null })).toBe('Done');
    expect(statusText({ status: 'processing', stage: 'transcribing' })).toBe('transcribing');
    expect(statusText({ status: 'processing', stage: null })).toBe('processing');
    expect(statusText({ status: 'failed', stage: 'failed' })).toBe('Failed');
    expect(statusText({ status: 'no_speech', stage: null })).toBe('No speech');
  });
});
