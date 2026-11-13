import { afterEach, describe, expect, it, vi } from 'vitest';
import { dayLabel, localDate, START_WINDOW_MS, tooFar } from './format';

afterEach(() => {
  vi.useRealTimers();
});

describe('format', () => {
  it('reads YYYY-MM-DD as a local date in any time zone', () => {
    const day = localDate('2026-11-14');
    expect([day.getFullYear(), day.getMonth(), day.getDate(), day.getHours()]).toEqual([2026, 10, 14, 0]);
    const recorded = localDate('2026-01-31T23:30');
    expect([recorded.getMonth(), recorded.getDate()]).toEqual([0, 31]);
  });

  it('labels the group for the local today "Today"', () => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date(2026, 10, 14, 23, 59));

    expect(dayLabel('2026-11-14')).toBe('Today');
    expect(dayLabel('2026-11-15')).not.toBe('Today');
    expect(dayLabel('2026-11-15')).toContain('15');
  });

  it('is too far only when the start is more than 7 days away', () => {
    const now = Date.parse('2026-11-14T10:00:00Z');
    expect(tooFar(new Date(now + START_WINDOW_MS).toISOString(), now)).toBe(false);
    expect(tooFar(new Date(now + START_WINDOW_MS + 60_000).toISOString(), now)).toBe(true);
    expect(tooFar('2026-11-14T09:00:00+00:00', now)).toBe(false);
  });
});
