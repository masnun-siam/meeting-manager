import { describe, expect, it } from 'vitest';
import {
  calendarName,
  formatClock,
  hasEdits,
  problemText,
  recordingOpenProblem,
  shownName,
  TOO_LARGE,
  TOOL_MISSING,
  tooLarge,
} from './format';
import { testNote } from './testNote';

describe('shownName (store::shown_name)', () => {
  it.each([
    [{ name: 'Rahim', name_edited: true }, 'Rahim'],
    [{ name: 'Rahim', name_edited: false }, 'Rahim (inferred)'],
    [{ name: 'Rahim (inferred)', name_edited: false }, 'Rahim (inferred)'],
    [{ name: null, name_edited: false }, 'Speaker 1'],
  ])('%o is %s', (speaker, want) => {
    expect(shownName({ label: 'Speaker 1', evidence: '', ...speaker })).toBe(want);
  });
});

describe('format', () => {
  it('never shows a calendar named by an email address', () => {
    expect(calendarName('Work')).toBe('Work');
    expect(calendarName('me@example.com')).toBeNull();
    expect(calendarName(null)).toBeNull();
  });

  it('counts title, body and speaker edits but not "My notes"', () => {
    expect(hasEdits(testNote({ my_notes: 'mine' }))).toBe(false);
    expect(hasEdits(testNote({ title_edited: true }))).toBe(true);
    expect(hasEdits(testNote({ body_edited: true }))).toBe(true);
    const speakers = testNote().speakers.map((s) => ({ ...s, name_edited: s.label === 'Speaker 1' }));
    expect(hasEdits(testNote({ speakers }))).toBe(true);
  });

  it('limits "My notes" to 1 MB of UTF-8', () => {
    expect(tooLarge('x'.repeat(1_048_576))).toBe(false);
    expect(tooLarge('x'.repeat(1_048_577))).toBe(true);
  });

  it.each([
    ['store_unavailable', 'disk I/O error', "Notes database can't be opened: disk I/O error"],
    ['tool_missing', 'Processing tool not found (x); run `meeting install`', TOOL_MISSING],
    ['too_large', 'my_notes is too large', TOO_LARGE],
    ['already_processing', 'already processing', 'already processing'],
    [null, 'Could not reach the Meeting app: x', 'Could not reach the Meeting app: x'],
  ])('%s shows %s', (code, text, want) => {
    expect(problemText(code, text)).toBe(want);
  });
});

describe('formatClock (FR-5)', () => {
  it('writes h:mm:ss, whole seconds, hours unpadded', () => {
    expect(formatClock(0)).toBe('0:00:00');
    expect(formatClock(12.9)).toBe('0:00:12');
    expect(formatClock(3 * 3600)).toBe('3:00:00');
    expect(formatClock(36_000 + 61)).toBe('10:01:01');
  });

  it('reads a length not known yet, or a negative time, as 0', () => {
    expect(formatClock(Number.NaN)).toBe('0:00:00');
    expect(formatClock(Number.POSITIVE_INFINITY)).toBe('0:00:00');
    expect(formatClock(-4)).toBe('0:00:00');
  });
});

describe('recordingOpenProblem (FR-7, FR-8)', () => {
  it('prefixes the reason macOS gave for open_failed', () => {
    expect(recordingOpenProblem('open_failed', 'no app')).toBe("Couldn't open the recording: no app");
  });

  it('words other codes as problemText does', () => {
    expect(recordingOpenProblem('tool_missing', 'x')).toBe(TOOL_MISSING);
    expect(recordingOpenProblem('missing_file', 'Recording missing')).toBe('Recording missing');
  });
});
