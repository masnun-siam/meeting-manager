import { act, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import type { CalendarInfo } from '../api';
import type { Api } from '../api.mock';

const api = vi.hoisted(() => ({
  calendarStatus: vi.fn(),
  calendars: vi.fn(),
  setHiddenCalendars: vi.fn(),
}));

vi.mock('../api', async () =>
  (await import('../api.mock')).mockApi({
    calendarStatus: () => api.calendarStatus(),
    calendars: () => api.calendars(),
    setHiddenCalendars: (req) => api.setHiddenCalendars(req),
  } satisfies Partial<Api>),
);

import { CALENDAR_DENIED, CalendarsSection } from './CalendarsSection';

const work: CalendarInfo = { id: 'work', title: 'Work', account: 'me@work.example', hidden: false };
const home: CalendarInfo = { id: 'home', title: 'Home', account: 'iCloud', hidden: false };

/** A stand-in for the app's stored hidden list, answering `set_hidden_calendars` as the Rust side does. */
function storeOf(...list: CalendarInfo[]) {
  let hidden = new Set(list.filter((c) => c.hidden).map((c) => c.id));
  const reply = () => ({ ok: true, calendars: list.map((c) => ({ ...c, hidden: hidden.has(c.id) })) });
  api.calendars.mockImplementation(async () => reply());
  api.setHiddenCalendars.mockImplementation(async ({ ids }: { ids: string[] }) => {
    hidden = new Set(ids);
    return reply();
  });
}

const deferred = <T,>() => {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((r) => {
    resolve = r;
  });
  return { promise, resolve };
};

beforeEach(() => {
  api.calendarStatus.mockReset().mockResolvedValue({ ok: true, access: 'granted' });
  api.calendars.mockReset();
  api.setHiddenCalendars.mockReset();
  storeOf(work, { ...home, hidden: true });
});

function setup() {
  const user = userEvent.setup();
  return { user, ...render(<CalendarsSection />) };
}

const box = (name: RegExp) => screen.getByRole('checkbox', { name });
const sentIds = () => api.setHiddenCalendars.mock.calls.map(([req]) => [...req.ids].sort());

describe('CalendarsSection', () => {
  it('lists each calendar labelled by title and account, checked when shown', async () => {
    setup();

    expect(await screen.findByRole('checkbox', { name: /Work me@work\.example/ })).toBeChecked();
    expect(box(/Home iCloud/)).not.toBeChecked();
  });

  it('sends the full hidden list on a toggle and shows the reply', async () => {
    const { user } = setup();

    await user.click(await screen.findByRole('checkbox', { name: /Work/ }));

    expect(sentIds()).toEqual([['home', 'work']]);
    expect(box(/Work/)).not.toBeChecked();
    await user.click(box(/Home/));
    expect(sentIds()[1]).toEqual(['work']);
    expect(box(/Home/)).toBeChecked();
  });

  it('serialises two quick toggles so the last list holds both', async () => {
    storeOf(work, home);
    const first = deferred<unknown>();
    const { user } = setup();
    await screen.findByRole('checkbox', { name: /Work/ });
    const store = api.setHiddenCalendars.getMockImplementation();
    api.setHiddenCalendars.mockImplementationOnce(async (req) => {
      await first.promise;
      return store?.(req);
    });

    await user.click(box(/Work/));
    await user.click(box(/Home/));
    expect(api.setHiddenCalendars).toHaveBeenCalledTimes(1);
    expect(box(/Work/)).not.toBeChecked();
    expect(box(/Home/)).not.toBeChecked();

    await act(async () => first.resolve(undefined));

    expect(sentIds()).toEqual([['work'], ['home', 'work']]);
    expect(box(/Work/)).not.toBeChecked();
    expect(box(/Home/)).not.toBeChecked();
  });

  it('reverts the box and shows the message when the store is unavailable', async () => {
    const { user } = setup();
    await screen.findByRole('checkbox', { name: /Work/ });
    api.setHiddenCalendars.mockResolvedValueOnce({ ok: false, code: 'store_unavailable', message: 'disk full' });

    await user.click(box(/Work/));

    expect(await screen.findByText('disk full')).toHaveAttribute('role', 'status');
    expect(box(/Work/)).toBeChecked();
    expect(box(/Home/)).not.toBeChecked();
  });

  it('reverts to what the app last saved when a queued change fails after an earlier one succeeded', async () => {
    storeOf(work, home);
    const first = deferred<unknown>();
    const { user } = setup();
    await screen.findByRole('checkbox', { name: /Work/ });
    const store = api.setHiddenCalendars.getMockImplementation();
    api.setHiddenCalendars
      .mockImplementationOnce(async (req) => {
        await first.promise;
        return store?.(req);
      })
      .mockResolvedValueOnce({ ok: false, code: 'store_unavailable', message: 'disk full' });

    await user.click(box(/Work/));
    await user.click(box(/Home/));
    await act(async () => first.resolve(undefined));

    expect(await screen.findByText('disk full')).toBeInTheDocument();
    expect(box(/Work/)).not.toBeChecked();
    expect(box(/Home/)).toBeChecked();
  });

  it('reverts and explains a rejected call', async () => {
    const { user } = setup();
    await screen.findByRole('checkbox', { name: /Work/ });
    api.setHiddenCalendars.mockRejectedValueOnce(new Error('ipc closed'));

    await user.click(box(/Work/));

    expect(await screen.findByText('Could not reach the Meeting app: ipc closed')).toBeInTheDocument();
    expect(box(/Work/)).toBeChecked();
  });

  it('shows the FR-8 text when access is denied, without asking for calendars', async () => {
    api.calendarStatus.mockResolvedValue({ ok: true, access: 'denied' });
    setup();

    expect(await screen.findByText(CALENDAR_DENIED)).toBeInTheDocument();
    expect(CALENDAR_DENIED).toBe('Allow Meeting in System Settings › Privacy & Security › Calendars');
    expect(api.calendars).not.toHaveBeenCalled();
    expect(screen.queryByRole('checkbox')).not.toBeInTheDocument();
  });

  it('explains where access is asked when it never was', async () => {
    api.calendarStatus.mockResolvedValue({ ok: true, access: 'not_asked' });
    setup();

    expect(await screen.findByText(/once you allow access in Upcoming/)).toBeInTheDocument();
    expect(api.calendars).not.toHaveBeenCalled();
  });

  it('says No calendars found for an empty list', async () => {
    api.calendars.mockResolvedValue({ ok: true, calendars: [] });
    setup();

    expect(await screen.findByText('No calendars found')).toBeInTheDocument();
  });

  it('shows a failed load instead of the list', async () => {
    api.calendarStatus.mockRejectedValue(new Error('down'));
    setup();

    expect(await screen.findByText('Could not reach the Meeting app: down')).toBeInTheDocument();
  });

  it('keeps an unchecked calendar unchecked after a relaunch', async () => {
    const { user, unmount } = setup();
    await user.click(await screen.findByRole('checkbox', { name: /Work/ }));
    expect(box(/Work/)).not.toBeChecked();
    unmount();

    setup();

    expect(await screen.findByRole('checkbox', { name: /Work/ })).not.toBeChecked();
  });
});
