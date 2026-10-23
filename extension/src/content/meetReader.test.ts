import { describe, expect, it } from 'vitest';
import adHoc from './fixtures/meet-ad-hoc.html?raw';
import full from './fixtures/meet-full.html?raw';
import lobby from './fixtures/meet-lobby.html?raw';
import noRoster from './fixtures/meet-no-roster.html?raw';
import noTitle from './fixtures/meet-no-title.html?raw';
import { readRoster, readTitle } from './meetReader';

const parse = (html: string) => new DOMParser().parseFromString(html, 'text/html');

function rosterPage(items: { label: string; you?: boolean }[]): Document {
  const html = items
    .map(
      ({ label, you }) =>
        `<div role="listitem" aria-label="${label}"><span>${label}</span>${you ? '<span>(You)</span>' : ''}</div>`,
    )
    .join('');
  return parse(`<title>Meet</title><div role="list" aria-label="Participants">${html}</div>`);
}

describe('readRoster', () => {
  it('reads the panel with exactly one self', () => {
    expect(readRoster(parse(full))).toEqual([
      { name: 'Sam Host', self: true },
      { name: 'Alex Example', self: false },
      { name: 'Riley Sample', self: false },
    ]);
  });

  it.each([
    ['no roster', noRoster],
    ['lobby', lobby],
    ['ad hoc, panel closed', adHoc],
  ])('finds nothing on %s', (_name, html) => {
    expect(readRoster(parse(html))).toEqual([]);
  });

  it('cleans, cuts and de-duplicates names and drops empty ones', () => {
    const long = 'x'.repeat(150);
    const doc = rosterPage([
      { label: '  Alex\u0007 Example ' },
      { label: 'Alex Example' },
      { label: long },
      { label: ' ' },
    ]);
    expect(readRoster(doc)).toEqual([
      { name: 'Alex Example', self: false },
      { name: 'x'.repeat(100), self: false },
    ]);
  });

  it('marks only the first (You) entry as self', () => {
    const doc = rosterPage([
      { label: 'Sam', you: true },
      { label: 'Sam Two', you: true },
    ]);
    expect(readRoster(doc)).toEqual([
      { name: 'Sam', self: true },
      { name: 'Sam Two', self: false },
    ]);
  });

  it('leaves out a self entry named only "You"', () => {
    expect(readRoster(rosterPage([{ label: 'You', you: true }, { label: 'Alex' }]))).toEqual([
      { name: 'Alex', self: false },
    ]);
  });
});

describe('readTitle', () => {
  it('reads the event name', () => {
    expect(readTitle(parse(full))).toBe('Team Sync Example');
    expect(readTitle(parse(noRoster))).toBe('Team Sync Example');
  });

  it.each([
    ['no title', noTitle],
    ['ad hoc (meeting code)', adHoc],
    ['lobby', lobby],
  ])('is null on %s', (_name, html) => {
    expect(readTitle(parse(html))).toBeNull();
  });

  it('cuts to 300 characters and treats a blank name as none', () => {
    expect(readTitle(parse(`<title>Meet - ${'t'.repeat(400)}</title>`))).toBe('t'.repeat(300));
    expect(readTitle(parse('<title>Meet -  </title>'))).toBeNull();
  });
});
