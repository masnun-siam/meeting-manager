// Meeting context helpers: pure, no Chrome APIs, every time passed in (like recordingState.ts).
import type { MeetingContextState } from './messages';
import type { ContextIdentity, ContextPayload } from './native';

const MAX_URL = 2_000;
const MAX_TITLE = 300;
const MEET_CODE = /^[a-z]{3}-[a-z]{4}-[a-z]{3}$/;
const ZOOM_ID = /^\d{9,11}$/;
const TEAMS_HOSTS = ['teams.microsoft.com', 'teams.live.com', 'teams.cloud.microsoft'];

/** Trims, removes control characters and cuts to `max` characters. */
export function cleanText(value: string, max: number): string {
  // biome-ignore lint/suspicious/noControlCharactersInRegex: removing them is the point
  const clean = value.replace(/[\u0000-\u001f\u007f-\u009f]/g, '').trim();
  return [...clean].slice(0, max).join('');
}

function platformOf(host: string): ContextIdentity['platform'] {
  if (host === 'meet.google.com') return 'meet';
  if (host === 'zoom.us' || host.endsWith('.zoom.us')) return 'zoom';
  if (TEAMS_HOSTS.includes(host)) return 'teams';
  return 'other';
}

/** The identity of the captured tab. An unreadable URL or title gives `other` with nulls. */
export function identityFromTab(
  tab: { url?: string; title?: string },
  startedAt: number,
  offsetMin: number,
  timeZone: string | null,
): ContextIdentity {
  const identity: ContextIdentity = {
    platform: 'other',
    url: null,
    meeting_id: null,
    tab_title: tab.title ? cleanText(tab.title, MAX_TITLE) : null,
    started_at_ms: startedAt,
    utc_offset_min: offsetMin,
    time_zone: timeZone,
  };
  let url: URL;
  try {
    url = new URL(tab.url ?? '');
  } catch {
    return identity;
  }
  if (url.protocol !== 'http:' && url.protocol !== 'https:') return identity;
  identity.platform = platformOf(url.hostname);
  const [segment = ''] = url.pathname.split('/').filter(Boolean);
  if (identity.platform === 'meet') {
    identity.meeting_id = MEET_CODE.test(segment) ? segment : null;
    identity.url = url.origin + url.pathname;
  } else if (identity.platform === 'zoom') {
    const id = url.pathname.split('/').find((s) => ZOOM_ID.test(s));
    identity.meeting_id = id ?? null;
    identity.url = url.origin + url.pathname;
  } else {
    identity.url = url.origin;
  }
  if (identity.url.length > MAX_URL) identity.url = url.origin;
  return identity;
}

/** `hh:mm:ss`, floored, hours not capped at 24. */
export function formatClock(ms: number): string {
  const s = Math.floor(Math.max(0, ms) / 1000);
  const p = (n: number) => String(n).padStart(2, '0');
  return `${p(Math.floor(s / 3600))}:${p(Math.floor(s / 60) % 60)}:${p(s % 60)}`;
}

/** The state in its snake_case wire shape. */
export function contextForHandoff(state: MeetingContextState): ContextPayload {
  return {
    version: 1,
    ...state.identity,
    meeting_title: state.meetingTitle,
    participants: state.participants,
    speaking: state.speaking.map((s) => ({ from: formatClock(s.fromMs), to: formatClock(s.toMs), names: s.names })),
    truncated: [...state.truncated],
  };
}

const bytes = (payload: ContextPayload) => new TextEncoder().encode(JSON.stringify(payload)).length;

/** Drops `speaking` entries from the end (earliest kept) until the payload fits, and lists `speaking` in `truncated`. */
export function trimToLimit(payload: ContextPayload, maxBytes = 256 * 1024): ContextPayload {
  if (bytes(payload) <= maxBytes) return payload;
  const withSpeaking = (n: number): ContextPayload => ({
    ...payload,
    speaking: payload.speaking.slice(0, n),
    truncated: payload.truncated.includes('speaking') ? payload.truncated : [...payload.truncated, 'speaking'],
  });
  let lo = 0;
  let hi = payload.speaking.length;
  while (lo < hi) {
    const mid = Math.ceil((lo + hi) / 2);
    if (bytes(withSpeaking(mid)) <= maxBytes) lo = mid;
    else hi = mid - 1;
  }
  return withSpeaking(lo);
}
