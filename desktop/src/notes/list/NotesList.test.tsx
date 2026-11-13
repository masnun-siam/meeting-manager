import { act, render, screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { NotesListRow } from '../../api';
import type { Api } from '../../api.mock';

const api = vi.hoisted(() => ({
  notesList: vi.fn(),
  rescan: vi.fn(),
  rescanStatus: vi.fn(),
}));

vi.mock('../../api', async () =>
  (await import('../../api.mock')).mockApi({
    notesList: (req) => api.notesList(req),
    rescan: () => api.rescan(),
    rescanStatus: () => api.rescanStatus(),
  } satisfies Partial<Api>),
);

import { NotesList } from './NotesList';

function row(extra: Partial<NotesListRow> = {}): NotesListRow {
  return {
    id: 1,
    title: 'Weekly sync',
    folder_name: 'meeting-2026-11-12_1000',
    recorded_at: '2026-11-12T10:00',
    duration_s: 1805,
    language: 'en',
    source: 'desktop',
    status: 'done',
    stage: null,
    error: null,
    recording_missing: false,
    ...extra,
  };
}

const notes = (...list: NotesListRow[]) => ({ ok: true, notes: list });
const idle = { ok: true, running: false, done: 0, total: 0 };
const deferred = <T,>() => {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((r) => {
    resolve = r;
  });
  return { promise, resolve };
};
const queries = () => api.notesList.mock.calls.map(([req]) => req.query);

function setVisibility(value: 'visible' | 'hidden') {
  Object.defineProperty(document, 'visibilityState', { configurable: true, value });
  document.dispatchEvent(new Event('visibilitychange'));
}

beforeEach(() => {
  vi.useFakeTimers({ shouldAdvanceTime: true });
  api.notesList.mockReset().mockResolvedValue(notes());
  api.rescan.mockReset().mockResolvedValue({ ok: true, started: true });
  api.rescanStatus.mockReset().mockResolvedValue(idle);
});

afterEach(() => {
  // jsdom's own getter is on the prototype; dropping the override restores it.
  delete (document as { visibilityState?: unknown }).visibilityState;
  vi.useRealTimers();
});

function setup() {
  const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime });
  const onOpen = vi.fn();
  const view = render(<NotesList onOpen={onOpen} />);
  return { user, onOpen, ...view };
}

const list = () => screen.getByRole('list', { name: 'Notes' });
const tick = (ms: number) => act(() => vi.advanceTimersByTimeAsync(ms));

describe('NotesList', () => {
  it('shows Loading meetings… until the first reply', () => {
    api.notesList.mockReturnValue(new Promise(() => {}));
    setup();

    expect(screen.getByText('Loading meetings…')).toBeInTheDocument();
    expect(api.notesList).toHaveBeenCalledWith({ query: null });
  });

  it('shows the PRD empty text with meeting import as code', async () => {
    setup();

    const empty = await screen.findByText(/No meetings yet/);
    expect(empty).toHaveTextContent('No meetings yet — start a recording or run meeting import.');
    expect(within(empty).getByText('meeting import').tagName).toBe('CODE');
  });

  it('shows each row’s title, date, duration, language, source and status, in the order given', async () => {
    api.notesList.mockResolvedValue(
      notes(
        row({ id: 3 }),
        row({ id: 2, title: null, duration_s: null, language: null, source: 'browser', recorded_at: 'bad' }),
        row({ id: 1, title: 'Older', language: 'bn', source: 'import', duration_s: 59 }),
      ),
    );
    setup();

    const rows = await within(await screen.findByRole('list', { name: 'Notes' })).findAllByRole('button');
    expect(rows.map((r) => r.querySelector('span')?.textContent)).toEqual([
      'Weekly sync',
      'meeting-2026-11-12_1000',
      'Older',
    ]);
    const date = new Date(2026, 10, 12, 10, 0).toLocaleString(undefined, { dateStyle: 'medium', timeStyle: 'short' });
    expect(rows[0]).toHaveTextContent(`${date} · 30:05 · English · Desktop`);
    expect(rows[0]).toHaveTextContent('Done');
    expect(rows[1]).toHaveTextContent('bad · Browser');
    expect(rows[1]).not.toHaveTextContent('English');
    expect(rows[2]).toHaveTextContent('0:59 · Bengali · Import');
  });

  it('shows the stage, a failed reason as text, no speech and a missing recording', async () => {
    api.notesList.mockResolvedValue(
      notes(
        row({ id: 1, status: 'processing', stage: 'transcribing' }),
        row({ id: 2, status: 'processing', stage: null }),
        row({ id: 3, status: 'failed', stage: 'failed', error: '<b>no api key</b>' }),
        row({ id: 4, status: 'no_speech', title: 'No speech detected' }),
        row({ id: 5, recording_missing: true }),
        row({ id: 6, status: 'failed', error: 'gone', recording_missing: true }),
      ),
    );
    setup();

    const rows = await within(await screen.findByRole('list', { name: 'Notes' })).findAllByRole('button');
    expect(rows[0]).toHaveTextContent('transcribing');
    expect(rows[1]).toHaveTextContent('processing');
    expect(rows[2]).toHaveTextContent('Failed — <b>no api key</b>');
    expect(rows[2].querySelector('b')).toBeNull();
    expect(rows[3]).toHaveTextContent('No speech detected');
    expect(rows[3]).toHaveTextContent('No speech');
    expect(rows[4]).toHaveTextContent('Done · Recording missing');
    expect(rows[5]).toHaveTextContent('Failed — gone · Recording missing');
  });

  it('opens a note on click and on Enter', async () => {
    api.notesList.mockResolvedValue(notes(row({ id: 7 }), row({ id: 8, title: 'Other' })));
    const { user, onOpen } = setup();

    await user.click(await screen.findByRole('button', { name: /Other/ }));
    expect(onOpen).toHaveBeenLastCalledWith(8);

    screen.getByRole('button', { name: /Weekly sync/ }).focus();
    await user.keyboard('{Enter}');
    expect(onOpen).toHaveBeenLastCalledWith(7);
  });

  it('reaches the search box and the rows with the keyboard', async () => {
    api.notesList.mockResolvedValue(notes(row()));
    const { user } = setup();
    await screen.findByRole('list', { name: 'Notes' });

    await user.tab();
    expect(screen.getByRole('searchbox', { name: 'Search meetings' })).toHaveFocus();
    await user.tab();
    expect(screen.getByRole('button', { name: /Weekly sync/ })).toHaveFocus();
  });

  it('renders 1,000 rows', async () => {
    api.notesList.mockResolvedValue(notes(...Array.from({ length: 1000 }, (_, i) => row({ id: i + 1 }))));
    const started = performance.now();
    setup();

    expect(await within(await screen.findByRole('list', { name: 'Notes' })).findAllByRole('listitem')).toHaveLength(
      1000,
    );
    expect(performance.now() - started).toBeLessThan(5000);
  });

  describe('polling', () => {
    it('refreshes every 3 s and updates a row in place through its stages', async () => {
      api.notesList
        .mockResolvedValueOnce(notes(row({ status: 'processing', stage: 'transcribing', title: null })))
        .mockResolvedValueOnce(notes(row({ status: 'processing', stage: 'summarising', title: null })))
        .mockResolvedValue(notes(row({ title: 'Budget review' })));
      setup();
      expect(await screen.findByText('transcribing')).toBeInTheDocument();
      expect(list()).toHaveTextContent('meeting-2026-11-12_1000');

      expect(api.notesList).toHaveBeenCalledTimes(1);
      await tick(3000);
      expect(api.notesList).toHaveBeenCalledTimes(2);
      expect(await screen.findByText('summarising')).toBeInTheDocument();

      await tick(3000);
      expect(await screen.findByText('Budget review')).toBeInTheDocument();
      expect(list()).toHaveTextContent('Done');
    });

    it('does not poll while hidden and refreshes at once when visible again', async () => {
      setup();
      await screen.findByText(/No meetings yet/);

      setVisibility('hidden');
      await tick(9000);
      expect(api.notesList).toHaveBeenCalledTimes(1);

      api.notesList.mockResolvedValue(notes(row()));
      setVisibility('visible');
      expect(api.notesList).toHaveBeenCalledTimes(2);
      expect(await screen.findByText('Weekly sync')).toBeInTheDocument();

      await tick(3000);
      expect(api.notesList).toHaveBeenCalledTimes(3);
    });

    it('stops on unmount', async () => {
      const remove = vi.spyOn(document, 'removeEventListener');
      const { unmount } = setup();
      await screen.findByText(/No meetings yet/);

      unmount();
      await tick(9000);

      expect(api.notesList).toHaveBeenCalledTimes(1);
      expect(remove).toHaveBeenCalledWith('visibilitychange', expect.any(Function));
      remove.mockRestore();
    });

    it('never lets a late reply replace a newer one', async () => {
      const slow = deferred<unknown>();
      api.notesList.mockReturnValueOnce(slow.promise).mockResolvedValue(notes(row({ title: 'Newer' })));
      setup();

      await tick(3000);
      expect(await screen.findByText('Newer')).toBeInTheDocument();

      await act(async () => slow.resolve(notes(row({ title: 'Older' }))));
      expect(screen.getByText('Newer')).toBeInTheDocument();
      expect(screen.queryByText('Older')).not.toBeInTheDocument();
    });
  });

  describe('search', () => {
    it('asks once, 200 ms after the last key, with the whole text', async () => {
      const { user } = setup();
      await screen.findByText(/No meetings yet/);

      await user.type(screen.getByRole('searchbox', { name: 'Search meetings' }), 'bud');
      await tick(150);
      expect(queries()).toEqual([null]);
      await tick(50);

      expect(queries()).toEqual([null, 'bud']);
    });

    it('polls with the current query', async () => {
      const { user } = setup();
      await user.type(screen.getByRole('searchbox', { name: 'Search meetings' }), 'budget');
      await tick(200);
      await tick(3000);

      expect(queries().slice(-2)).toEqual(['budget', 'budget']);
    });

    it('shows No meetings match. and keeps the input value and focus', async () => {
      api.notesList.mockImplementation(async ({ query }) => (query === null ? notes(row()) : notes()));
      const { user } = setup();
      await screen.findByText('Weekly sync');
      const search = screen.getByRole('searchbox', { name: 'Search meetings' });

      await user.type(search, 'zzz');
      await tick(200);

      expect(await screen.findByText('No meetings match.')).toBeInTheDocument();
      expect(screen.queryByText(/No meetings yet/)).not.toBeInTheDocument();
      expect(search).toHaveValue('zzz');
      expect(search).toHaveFocus();
    });

    it('sends null for whitespace and restores the full list when cleared, without waiting for the poll', async () => {
      api.notesList.mockImplementation(async ({ query }) =>
        query === null ? notes(row(), row({ id: 2, title: 'Second' })) : notes(row({ title: 'Budget' })),
      );
      const { user } = setup();
      const search = screen.getByRole('searchbox', { name: 'Search meetings' });

      await user.type(search, 'budg');
      await tick(200);
      expect(await screen.findByText('Budget')).toBeInTheDocument();

      await user.clear(search);
      await user.type(search, '   ');
      await tick(200);

      expect(await screen.findByText('Second')).toBeInTheDocument();
      expect(queries()).toEqual([null, 'budg', null]);
    });

    it('never shows an older query’s slow reply over a newer one', async () => {
      const slow = deferred<unknown>();
      api.notesList.mockImplementation(async ({ query }) => {
        if (query === 'a') return slow.promise;
        return query === null ? notes() : notes(row({ title: 'Answer to ab' }));
      });
      const { user } = setup();
      const search = screen.getByRole('searchbox', { name: 'Search meetings' });

      await user.type(search, 'a');
      await tick(200);
      await user.type(search, 'b');
      await tick(200);
      expect(await screen.findByText('Answer to ab')).toBeInTheDocument();

      await act(async () => slow.resolve(notes(row({ title: 'Answer to a' }))));
      expect(screen.getByText('Answer to ab')).toBeInTheDocument();
      expect(screen.queryByText('Answer to a')).not.toBeInTheDocument();
    });
  });

  describe('backfill', () => {
    it('shows progress while running, keeps the list usable and refreshes it once when done', async () => {
      api.rescanStatus
        .mockResolvedValueOnce({ ok: true, running: true, done: 3, total: 10 })
        .mockResolvedValueOnce({ ok: true, running: true, done: 10, total: 10 })
        .mockResolvedValue(idle);
      api.notesList.mockResolvedValueOnce(notes(row())).mockResolvedValue(notes(row(), row({ id: 2, title: 'New' })));
      const { onOpen, user } = setup();

      expect(await screen.findByText('Loading meetings… 3 of 10')).toHaveAttribute('role', 'status');
      await user.click(await screen.findByRole('button', { name: /Weekly sync/ }));
      expect(onOpen).toHaveBeenCalledWith(1);

      await tick(1000);
      expect(await screen.findByText('Loading meetings… 10 of 10')).toBeInTheDocument();
      const before = api.notesList.mock.calls.length;
      await tick(1000);

      expect(await screen.findByText('New')).toBeInTheDocument();
      expect(screen.queryByText(/of 10/)).not.toBeInTheDocument();
      expect(api.notesList.mock.calls.length).toBe(before + 1);
      await tick(2000);
      expect(api.rescanStatus).toHaveBeenCalledTimes(3);
    });

    it('ignores a late backfill status that a newer one already answered', async () => {
      const late = deferred<unknown>();
      api.rescanStatus
        .mockResolvedValueOnce({ ok: true, running: true, done: 1, total: 4 })
        .mockReturnValueOnce(late.promise)
        .mockResolvedValue(idle);
      setup();
      expect(await screen.findByText('Loading meetings… 1 of 4')).toBeInTheDocument();
      await tick(1000);
      await tick(1000);
      await screen.findByText(/No meetings yet/);
      expect(screen.queryByText(/of 4/)).not.toBeInTheDocument();
      const lists = api.notesList.mock.calls.length;

      await act(async () => late.resolve({ ok: true, running: true, done: 2, total: 4 }));
      await tick(3000);

      expect(screen.queryByText(/of 4/)).not.toBeInTheDocument();
      expect(api.rescanStatus).toHaveBeenCalledTimes(3);
      expect(api.notesList.mock.calls.length).toBe(lists + 1);
    });

    it('does not poll the backfill when it is not running', async () => {
      setup();
      await screen.findByText(/No meetings yet/);
      await tick(5000);

      expect(api.rescanStatus).toHaveBeenCalledTimes(1);
    });
  });

  describe('failures', () => {
    it('replaces the list with an alert when the store is unavailable; Rescan folders reloads it', async () => {
      api.notesList.mockResolvedValue({ ok: false, code: 'store_unavailable', message: 'file is not a database' });
      const { user } = setup();

      const alert = await screen.findByRole('alert');
      expect(alert).toHaveTextContent("Notes database can't be opened: file is not a database");
      expect(screen.getByRole('heading', { name: 'All notes' })).toBeInTheDocument();

      api.notesList.mockResolvedValue(notes(row()));
      api.rescanStatus.mockResolvedValue(idle);
      await user.click(within(alert).getByRole('button', { name: 'Rescan folders' }));

      expect(api.rescan).toHaveBeenCalledTimes(1);
      expect(await screen.findByText('Weekly sync')).toBeInTheDocument();
      expect(screen.queryByRole('alert')).not.toBeInTheDocument();
    });

    it('shows Rescan folders’ own error', async () => {
      api.notesList.mockResolvedValue({ ok: false, code: 'store_unavailable', message: 'broken' });
      api.rescan.mockResolvedValue({ ok: false, code: 'not_configured', message: 'run meeting install' });
      const { user } = setup();

      await user.click(await screen.findByRole('button', { name: 'Rescan folders' }));

      expect(await screen.findByText('run meeting install')).toHaveAttribute('role', 'status');
    });

    it('keeps the last good list and shows the message of a later failure', async () => {
      api.notesList
        .mockResolvedValueOnce(notes(row()))
        .mockResolvedValueOnce({ ok: false, code: 'other', message: 'busy' })
        .mockRejectedValueOnce(new Error('Invalid reply from the Meeting app'))
        .mockResolvedValue(notes(row({ title: 'Back' })));
      setup();
      expect(await screen.findByText('Weekly sync')).toBeInTheDocument();

      await tick(3000);
      expect(await screen.findByText('busy')).toHaveAttribute('role', 'status');
      expect(screen.getByText('Weekly sync')).toBeInTheDocument();

      await tick(3000);
      expect(await screen.findByText('Invalid reply from the Meeting app')).toBeInTheDocument();
      expect(screen.getByText('Weekly sync')).toBeInTheDocument();

      await tick(3000);
      expect(await screen.findByText('Back')).toBeInTheDocument();
      expect(screen.queryByText('Invalid reply from the Meeting app')).not.toBeInTheDocument();
    });

    it('shows a failure before any list in the status region', async () => {
      api.notesList.mockRejectedValue('IPC down');
      setup();

      expect(await screen.findByText('IPC down')).toHaveAttribute('role', 'status');
      expect(screen.queryByText('Loading meetings…')).not.toBeInTheDocument();
    });

    it('writes nothing to the console', async () => {
      const spies = (['log', 'info', 'warn', 'error', 'debug'] as const).map((m) => vi.spyOn(console, m));
      api.notesList
        .mockResolvedValueOnce(notes(row({ status: 'failed', error: 'secret reason' })))
        .mockRejectedValue(new Error('boom'));
      const { user } = setup();
      await screen.findByText(/secret reason/);
      await user.type(screen.getByRole('searchbox', { name: 'Search meetings' }), 'private words');
      await tick(3200);

      for (const spy of spies) {
        expect(spy).not.toHaveBeenCalled();
        spy.mockRestore();
      }
    });
  });
});
