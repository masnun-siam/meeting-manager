import { describe, expect, it } from 'vitest';
import { formatElapsed } from './formatElapsed';

describe('formatElapsed', () => {
  it.each([
    [0, '0:00:00'],
    [999, '0:00:00'],
    [61_000, '0:01:01'],
    [3_599_000, '0:59:59'],
    [3 * 3_600_000, '3:00:00'],
  ])('formats %i ms as %s', (ms, text) => {
    expect(formatElapsed(ms)).toBe(text);
  });
});
