import { render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { CalendarEventDetail, EarlierNote } from '../api';
import * as api from '../api';
import { statusReply } from '../recording/test-status';
import { EventView, TOO_FAR } from './EventView';

vi.mock('../api', async () => (await import('../api.mock')).mockApi());

const NOW = new Date('2026-11-14T09:00:00+06:00');

const EVENT: CalendarEventDetail = {
  occurrence_id: 'occ-1',
  series_id: 'series-1',
  title: 'Design review',
  start: '2026-11-14T10:00:00+06:00',
  end: '2026-11-14T11:00:00+06:00',
  organizer: 'Rahim Uddin',
  calendar: 'Work',
  attendees: [
    { name: 'Ayesha', response: 'accepted' },
    { name: 'Karim', response: 'declined' },
    { name: 'Nadia', response: 'tentative' },
    { name: 'Tanvir', response: 'needs_action' },
  ],
  description: 'Agenda:\n1. <b>Mockups</b>\n2. Copy',
};

const calendarEvent = vi.mocked(api.calendarEvent);

function earlier(n: number): EarlierNote[] {
  return Array.from({ length: n }, (_, i) => ({
    id: 100 - i,
    title: `Review ${n - i}`,
    recorded_at: '2026-11-07T10:00',
  }));
}

beforeEach(() => {
  vi.useFakeTimers({ shouldAdvanceTime: true });
  vi.setSystemTime(NOW);
  vi.resetAllMocks();
  vi.mocked(api.recorderStatus).mockResolvedValue(statusReply({ language: 'bn' }));
  calendarEvent.mockResolvedValue({ ok: true, event: EVENT, earlier: [] });
});

afterEach(() => {
  vi.useRealTimers();
});

function setup(props: { onBack?: () => void; onOpenNote?: (id: number) => void } = {}) {
  const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime });
  const onBack = props.onBack ?? vi.fn();
  const onOpenNote = props.onOpenNote ?? vi.fn();
  const view = render(<EventView occurrenceId="occ-1" onBack={onBack} onOpenNote={onOpenNote} />);
  return { user, onBack, onOpenNote, ...view };
}

const start = () => screen.getByRole('button', { name: 'Start recording' });

describe('EventView', () => {
  it('shows the event and focuses its heading', async () => {
    setup();

    const heading = await screen.findByRole('heading', { level: 2, name: 'Design review' });
    await waitFor(() => expect(heading).toHaveFocus());
    expect(calendarEvent).toHaveBeenCalledWith({ occurrence_id: 'occ-1' });
    expect(screen.getByText('Work')).toBeInTheDocument();
    expect(screen.getByText('Rahim Uddin')).toBeInTheDocument();
    expect(screen.getByText(/–/)).toBeInTheDocument();
  });

  it('lists attendees by name with their response as text', async () => {
    setup();

    const list = within(await screen.findByRole('list'));
    const items = list.getAllByRole('listitem').map((item) => item.textContent);
    expect(items).toEqual(['Ayesha (Accepted)', 'Karim (Declined)', 'Nadia (Tentative)', 'Tanvir (No response)']);
  });

  it('renders only attendee names, never anything else of the attendee', async () => {
    calendarEvent.mockResolvedValue({
      ok: true,
      event: {
        ...EVENT,
        organizer: null,
        description: null,
        attendees: [{ name: 'Ayesha', response: 'accepted', email: 'ayesha@example.com' } as never],
      },
      earlier: [],
    });
    const { container } = setup();

    await screen.findByText(/Ayesha/);
    expect(container.textContent).not.toContain('@');
  });

  it('shows the notes as plain text, keeping line breaks', async () => {
    setup();

    const notes = await screen.findByText(/Agenda:/);
    expect(notes.textContent).toBe('Agenda:\n1. <b>Mockups</b>\n2. Copy');
    expect(notes.querySelector('b')).toBeNull();
  });

  it('starts a recording linked to the event in the chosen language', async () => {
    const { user } = setup();
    await screen.findByRole('heading', { name: 'Design review' });

    const language = screen.getByRole('combobox', { name: 'Language' });
    await waitFor(() => expect(language).toHaveValue('bn'));
    await waitFor(() => expect(start()).toBeEnabled());
    await user.selectOptions(language, 'English');
    await user.click(start());
    expect(api.recorderStart).toHaveBeenCalledWith({ language: 'en', event_occurrence_id: 'occ-1' });
  });

  it('disables Start while a recording runs', async () => {
    vi.mocked(api.recorderStatus).mockResolvedValue(statusReply({ state: 'recording', elapsed_s: 5 }));
    setup();
    await screen.findByRole('heading', { name: 'Design review' });

    await waitFor(() => expect(api.recorderStatus).toHaveBeenCalled());
    expect(start()).toBeDisabled();
  });

  it('disables Start with a visible reason for an event more than 7 days away', async () => {
    calendarEvent.mockResolvedValue({
      ok: true,
      event: { ...EVENT, start: '2026-11-21T10:00:00+06:00', end: '2026-11-21T11:00:00+06:00' },
      earlier: [],
    });
    setup();
    await screen.findByRole('heading', { name: 'Design review' });

    expect(screen.getByText(TOO_FAR)).toBeInTheDocument();
    expect(start()).toBeDisabled();
  });

  it('shows the event_too_far message from the server check', async () => {
    vi.mocked(api.recorderStart).mockResolvedValue({
      ok: false,
      code: 'event_too_far',
      message: 'This event starts more than 7 days from now',
    });
    const { user } = setup();
    await screen.findByRole('heading', { name: 'Design review' });
    await waitFor(() => expect(start()).toBeEnabled());

    await user.click(start());
    expect(await screen.findByText('This event starts more than 7 days from now')).toBeInTheDocument();
  });

  it('lists at most 20 earlier notes in the order returned and opens one', async () => {
    calendarEvent.mockResolvedValue({ ok: true, event: EVENT, earlier: earlier(25) });
    const { user, onOpenNote } = setup();

    expect(await screen.findByRole('heading', { name: 'Earlier notes' })).toBeInTheDocument();
    const notes = screen.getAllByRole('button', { name: /Review \d+/ });
    expect(notes).toHaveLength(20);
    expect(notes[0]).toHaveTextContent('Review 25');
    expect(notes[0]).toHaveTextContent('2026');

    await user.click(notes[1]);
    expect(onOpenNote).toHaveBeenCalledWith(99);
  });

  it('shows no earlier notes heading when there are none', async () => {
    setup();
    await screen.findByRole('heading', { name: 'Design review' });

    expect(screen.queryByRole('heading', { name: 'Earlier notes' })).not.toBeInTheDocument();
  });

  it.each([
    ['unknown_event', 'That event is gone', 'That event is gone'],
    ['calendar_denied', 'denied', 'Allow Meeting in System Settings › Privacy & Security › Calendars'],
    [
      'calendar_failed',
      'Calendar could not be read; quit and reopen Meeting',
      'Calendar could not be read; quit and reopen Meeting',
    ],
  ] as const)('%s shows its message and a Back button', async (code, message, text) => {
    calendarEvent.mockResolvedValue({ ok: false, code, message });
    const { user, onBack } = setup();

    expect(await screen.findByText(text)).toBeInTheDocument();
    const back = screen.getByRole('button', { name: 'Back' });
    await waitFor(() => expect(back).toHaveFocus());
    await user.click(back);
    expect(onBack).toHaveBeenCalledOnce();
  });

  it('reaches every control by keyboard', async () => {
    calendarEvent.mockResolvedValue({ ok: true, event: EVENT, earlier: earlier(1) });
    const { user } = setup();
    await screen.findByRole('heading', { name: 'Design review' });
    await waitFor(() => expect(start()).toBeEnabled());

    await user.tab();
    expect(screen.getByRole('combobox', { name: 'Language' })).toHaveFocus();
    await user.tab();
    expect(start()).toHaveFocus();
    await user.tab();
    expect(screen.getByRole('button', { name: /Review 1/ })).toHaveFocus();
  });
});
