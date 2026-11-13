import { describe, expect, it } from 'vitest';
import {
  calendarName,
  duration,
  hasEdits,
  localDateTime,
  problemText,
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
  it('reads a store time as local time', () => {
    const date = localDateTime('2026-11-12T10:05');
    expect([date.getFullYear(), date.getMonth(), date.getDate(), date.getHours(), date.getMinutes()]).toEqual([
      2026, 10, 12, 10, 5,
    ]);
  });

  it.each([
    [45, '45 s'],
    [60, '1 min'],
    [1830, '31 min'],
    [3900, '1 h 5 min'],
  ])('duration %d is %s', (seconds, want) => {
    expect(duration(seconds)).toBe(want);
  });

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
