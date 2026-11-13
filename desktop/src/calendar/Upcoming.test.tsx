import { act, render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { CalendarDay, CalendarListEvent } from '../api';
import * as api from '../api';
import { RecordingBar } from '../recording/RecordingBar';
import { deferred, statusReply } from '../recording/test-status';
import { EMPTY, Upcoming } from './Upcoming';
import { CHANGE_DEBOUNCE_MS } from './useUpcoming';

vi.mock('../api', async () => (await import('../api.mock')).mockApi());

const events = vi.hoisted(() => ({
  handlers: [] as (() => void)[],
  unlisten: [] as (() => void)[],
}));
vi.mock('@tauri-apps/api/event', async () => {
  const { vi } = await import('vitest');
  return {
    listen: vi.fn(async (name: string, handler: () => void) => {
      if (name !== 'calendar-changed') throw new Error(`unexpected event ${name}`);
      events.handlers.push(handler);
      const stop = vi.fn(() => {
        events.handlers = events.handlers.filter((h) => h !== handler);
      });
      events.unlisten.push(stop);
      return stop;
    }),
  };
});

const calendarEvents = vi.mocked(api.calendarEvents);

function row(id: string, title: string, extra: Partial<CalendarListEvent> = {}): CalendarListEvent {
  return {
    occurrence_id: id,
    series_id: `series-${id}`,
    title,
    start: '2026-11-14T10:00:00+06:00',
    end: '2026-11-14T10:30:00+06:00',
    calendar: 'Work',
    has_notes: false,
    ...extra,
  };
}

const DAYS: CalendarDay[] = [
  { date: '2026-11-14', events: [row('a', 'Standup', { has_notes: true })] },
  { date: '2026-11-15', events: [row('b', 'Planning', { calendar: null })] },
];

beforeEach(() => {
  vi.useFakeTimers({ shouldAdvanceTime: true });
  vi.setSystemTime(new Date(2026, 10, 14, 9, 0));
  vi.resetAllMocks();
  events.handlers = [];
  events.unlisten = [];
  vi.mocked(api.recorderStatus).mockResolvedValue(statusReply());
  calendarEvents.mockResolvedValue({ ok: true, days: DAYS });
});

afterEach(() => {
  vi.useRealTimers();
});

function setup(onOpenNote = vi.fn()) {
  const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime });
  const view = render(<Upcoming onOpenNote={onOpenNote} />);
  return { user, ...view };
}

describe('Upcoming', () => {
  it('shows "Loading calendar…" while the call is pending (also while access is being asked)', async () => {
    const pending = deferred<Awaited<ReturnType<typeof api.calendarEvents>>>();
    calendarEvents.mockReturnValue(pending.promise);
    setup();

    expect(screen.getByRole('status')).toHaveTextContent('Loading calendar…');
    await act(async () => pending.resolve({ ok: true, days: DAYS }));
    expect(screen.queryByText('Loading calendar…')).not.toBeInTheDocument();
  });

  it('groups events by day in the order returned, today first, with a notes marker', async () => {
    setup();

    const today = await screen.findByRole('region', { name: 'Today' });
    expect(within(today).getByRole('heading', { level: 3 })).toHaveTextContent('Today');
    const standup = within(today).getByRole('button', { name: /Standup/ });
    expect(standup).toHaveTextContent('Work');
    expect(standup).toHaveTextContent('notes');

    const headings = screen.getAllByRole('heading', { level: 3 }).map((h) => h.textContent);
    expect(headings[0]).toBe('Today');
    expect(headings[1]).toContain('15');
    const planning = screen.getByRole('button', { name: /Planning/ });
    expect(planning).not.toHaveTextContent('notes');
  });

  it.each([
    ['no days', []],
    ['only empty days', [{ date: '2026-11-14', events: [] }]],
  ])('shows the empty state for %s', async (_, days) => {
    calendarEvents.mockResolvedValue({ ok: true, days });
    setup();

    expect(await screen.findByText(EMPTY)).toBeInTheDocument();
    expect(EMPTY).toBe('Nothing on your calendar for the next 7 days.');
  });

  it('calendar_denied shows the Calendars pane and the recording bar still starts', async () => {
    calendarEvents.mockResolvedValue({ ok: false, code: 'calendar_denied', message: 'denied' });
    const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime });
    render(
      <>
        <RecordingBar />
        <Upcoming onOpenNote={vi.fn()} />
      </>,
    );

    expect(
      await screen.findByText('Allow Meeting in System Settings › Privacy & Security › Calendars'),
    ).toBeInTheDocument();
    const start = screen.getByRole('button', { name: 'Start recording' });
    await waitFor(() => expect(start).toBeEnabled());
    await user.click(start);
    expect(api.recorderStart).toHaveBeenCalledWith({ language: 'en', event_occurrence_id: null });
  });

  it('calendar_failed shows its message', async () => {
    calendarEvents.mockResolvedValue({
      ok: false,
      code: 'calendar_failed',
      message: 'Calendar could not be read; quit and reopen Meeting',
    });
    setup();

    expect(await screen.findByText('Calendar could not be read; quit and reopen Meeting')).toBeInTheDocument();
  });

  it('refetches when the window gains focus', async () => {
    setup();
    await screen.findByText('Standup');
    expect(calendarEvents).toHaveBeenCalledTimes(1);

    calendarEvents.mockResolvedValue({ ok: true, days: [{ date: '2026-11-14', events: [row('c', 'Retro')] }] });
    act(() => {
      window.dispatchEvent(new Event('focus'));
    });
    expect(await screen.findByText('Retro')).toBeInTheDocument();
    expect(calendarEvents).toHaveBeenCalledTimes(2);
  });

  it('refetches once per burst of calendar-changed events and unlistens on unmount', async () => {
    const { unmount } = setup();
    await screen.findByText('Standup');
    await waitFor(() => expect(events.handlers).toHaveLength(1));

    calendarEvents.mockResolvedValue({ ok: true, days: [{ date: '2026-11-14', events: [row('c', 'Retro')] }] });
    act(() => {
      for (let i = 0; i < 3; i++) events.handlers[0]();
    });
    await act(() => vi.advanceTimersByTimeAsync(CHANGE_DEBOUNCE_MS));
    expect(await screen.findByText('Retro')).toBeInTheDocument();
    expect(calendarEvents).toHaveBeenCalledTimes(2);

    unmount();
    expect(events.unlisten[0]).toHaveBeenCalledOnce();
    expect(events.handlers).toHaveLength(0);
  });

  it('unlistens when unmounted before listen resolves', async () => {
    const { unmount } = setup();
    unmount();
    await waitFor(() => expect(events.unlisten[0]).toHaveBeenCalledOnce());
  });

  it('a failed refetch keeps the last good list and shows the error', async () => {
    setup();
    await screen.findByText('Standup');

    calendarEvents.mockResolvedValue({
      ok: false,
      code: 'calendar_failed',
      message: 'Calendar could not be read; quit and reopen Meeting',
    });
    act(() => {
      window.dispatchEvent(new Event('focus'));
    });
    expect(await screen.findByRole('status')).toHaveTextContent('Calendar could not be read; quit and reopen Meeting');
    expect(screen.getByText('Standup')).toBeInTheDocument();

    calendarEvents.mockRejectedValue(new Error('ipc closed'));
    act(() => {
      window.dispatchEvent(new Event('focus'));
    });
    await waitFor(() => expect(screen.getByRole('status')).toHaveTextContent('ipc closed'));
    expect(screen.getByText('Standup')).toBeInTheDocument();
  });

  it('opens the event view from a row and returns focus to it on Back without refetching', async () => {
    const { user } = setup();
    await user.click(await screen.findByRole('button', { name: /Planning/ }));

    expect(api.calendarEvent).toHaveBeenCalledWith({ occurrence_id: 'b' });
    const heading = await screen.findByRole('heading', { level: 2, name: 'Weekly sync' });
    await waitFor(() => expect(heading).toHaveFocus());

    await user.click(screen.getByRole('button', { name: 'Back' }));
    expect(screen.getByRole('button', { name: /Planning/ })).toHaveFocus();
    expect(calendarEvents).toHaveBeenCalledTimes(1);
  });

  it('opens a row with the keyboard', async () => {
    const { user } = setup();
    await screen.findByText('Standup');

    await user.tab();
    expect(screen.getByRole('button', { name: /Standup/ })).toHaveFocus();
    await user.keyboard('{Enter}');
    expect(api.calendarEvent).toHaveBeenCalledWith({ occurrence_id: 'a' });
  });
});
