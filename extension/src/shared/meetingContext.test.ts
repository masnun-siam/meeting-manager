import { describe, expect, it, vi } from 'vitest';
import {
  cleanText,
  contextForHandoff,
  formatClock,
  identityFromTab,
  trimToLimit,
  utcOffsetMin,
} from './meetingContext';
import type { MeetingContextState } from './messages';

const id = (url: string | undefined, title: string | undefined = 'T') =>
  identityFromTab({ url, title }, 1000, 360, 'Asia/Dhaka');

describe('identityFromTab', () => {
  it('keeps a Zoom web client URL without its query', () => {
    expect(id('https://app.zoom.us/wc/123456789/join?pwd=secret')).toMatchObject({
      platform: 'zoom',
      url: 'https://app.zoom.us/wc/123456789/join',
      meeting_id: '123456789',
    });
  });

  it('keeps only the origin for Teams', () => {
    expect(id('https://teams.microsoft.com/l/meetup-join/19:meeting_x@thread.v2/0?context=x')).toMatchObject({
      platform: 'teams',
      url: 'https://teams.microsoft.com',
      meeting_id: null,
    });
  });

  it('keeps a Meet code', () => {
    expect(id('https://meet.google.com/abc-defg-hij?authuser=1')).toMatchObject({
      platform: 'meet',
      url: 'https://meet.google.com/abc-defg-hij',
      meeting_id: 'abc-defg-hij',
    });
    expect(id('https://meet.google.com/landing').meeting_id).toBeNull();
  });

  it('gives other hosts scheme and host only', () => {
    expect(id('https://example.com/a/b?c=d')).toMatchObject({
      platform: 'other',
      url: 'https://example.com',
      meeting_id: null,
    });
  });

  it.each([undefined, 'not a url', 'chrome://extensions', 'file:///x'])('is other with no URL for %s', (url) => {
    expect(id(url)).toMatchObject({ platform: 'other', url: null, meeting_id: null });
  });

  it('has a null title when unreadable and cleans a readable one', () => {
    expect(identityFromTab({ url: 'https://example.com' }, 1, 0, null).tab_title).toBeNull();
    expect(id('https://example.com', ' a\u0000b\n ').tab_title).toBe('ab');
  });

  it('falls back to the origin when the URL is too long', () => {
    expect(id(`https://app.zoom.us/wc/123456789/${'x'.repeat(2_000)}`).url).toBe('https://app.zoom.us');
  });

  it('carries the start time, offset and zone', () => {
    expect(id('https://example.com')).toMatchObject({
      started_at_ms: 1000,
      utc_offset_min: 360,
      time_zone: 'Asia/Dhaka',
    });
    expect(identityFromTab({}, 1, -300, null)).toMatchObject({ utc_offset_min: -300, time_zone: null });
  });
});

describe('cleanText and formatClock', () => {
  it('trims, strips control characters and cuts', () => {
    expect(cleanText('  héllo\tworld  ', 5)).toBe('héllo');
  });

  it('formats hh:mm:ss, floored, past 24 hours', () => {
    expect(formatClock(3_661_999)).toBe('01:01:01');
    expect(formatClock(100 * 3_600_000)).toBe('100:00:00');
  });
});

const state = (n: number): MeetingContextState => ({
  identity: id('https://meet.google.com/abc-defg-hij'),
  segments: [{ wallStart: 1000, wallEnd: null, offsetMs: 0 }],
  meetingTitle: 'M',
  participants: [{ name: 'A', self: true }],
  speaking: Array.from({ length: n }, (_, i) => ({
    fromMs: i * 1000,
    toMs: i * 1000 + 500,
    names: ['Ann'.repeat(30)],
  })),
  openSpeaking: null,
  truncated: [],
  capture: 'captured',
  reloadHint: false,
});

describe('contextForHandoff and trimToLimit', () => {
  it('maps to the wire shape with exactly the host keys', () => {
    const payload = contextForHandoff(state(1));
    expect(Object.keys(payload).sort()).toEqual(
      [
        'meeting_id',
        'meeting_title',
        'participants',
        'platform',
        'speaking',
        'started_at_ms',
        'tab_title',
        'time_zone',
        'truncated',
        'url',
        'utc_offset_min',
        'version',
      ].sort(),
    );
    expect(payload.speaking).toEqual([{ from: '00:00:00', to: '00:00:00', names: ['Ann'.repeat(30)] }]);
  });

  it('leaves a small payload unchanged', () => {
    const payload = contextForHandoff(state(3));
    expect(trimToLimit(payload)).toBe(payload);
  });

  it('keeps the earliest speaking entries and lists speaking', () => {
    const payload = contextForHandoff(state(5_000));
    const out = trimToLimit(payload, 20_000);
    expect(new TextEncoder().encode(JSON.stringify(out)).length).toBeLessThanOrEqual(20_000);
    expect(out.speaking.length).toBeGreaterThan(0);
    expect(out.speaking.length).toBeLessThan(5_000);
    expect(out.speaking).toEqual(payload.speaking.slice(0, out.speaking.length));
    expect(out.truncated).toEqual(['speaking']);
  });
});

describe('utcOffsetMin', () => {
  it.each([
    [-360, 360],
    [300, -300],
  ])('turns getTimezoneOffset %i into %i', (raw, want) => {
    vi.spyOn(Date.prototype, 'getTimezoneOffset').mockReturnValue(raw);
    expect(utcOffsetMin(0)).toBe(want);
    vi.restoreAllMocks();
  });

  it('is 0, not -0, in UTC', () => {
    vi.spyOn(Date.prototype, 'getTimezoneOffset').mockReturnValue(0);
    expect(Object.is(utcOffsetMin(0), 0)).toBe(true);
    vi.restoreAllMocks();
  });
});
