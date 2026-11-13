// PRD desktop-app FR-8.
export const CALENDAR_DENIED = 'Allow Meeting in System Settings › Privacy & Security › Calendars';
// FR-11: an event starting more than 7 days from now cannot be started.
export const START_WINDOW_MS = 7 * 24 * 60 * 60 * 1000;

/**
 * A local `YYYY-MM-DD` (or the date part of `YYYY-MM-DDTHH:MM`) as a local Date.
 * `new Date('YYYY-MM-DD')` would read it as UTC midnight, a day early west of UTC.
 */
export function localDate(value: string): Date {
  const [y, m, d] = value.slice(0, 10).split('-').map(Number);
  return new Date(y, m - 1, d);
}

export function isToday(date: Date, now: Date = new Date()): boolean {
  return (
    date.getFullYear() === now.getFullYear() && date.getMonth() === now.getMonth() && date.getDate() === now.getDate()
  );
}

export function dayLabel(date: string, now: Date = new Date()): string {
  const day = localDate(date);
  return isToday(day, now)
    ? 'Today'
    : day.toLocaleDateString(undefined, { weekday: 'long', month: 'long', day: 'numeric' });
}

export function timeOf(iso: string): string {
  return new Date(iso).toLocaleTimeString(undefined, { hour: '2-digit', minute: '2-digit' });
}

export function timeRange(start: string, end: string): string {
  return `${timeOf(start)}–${timeOf(end)}`;
}

export function tooFar(start: string, now: number = Date.now()): boolean {
  return new Date(start).getTime() - now > START_WINDOW_MS;
}
