import { act, render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { Note } from '../../api';
import * as api from '../../api';
import { Actions, ARCHIVED, CONFIRM_MS } from './Actions';
import { NOT_WHILE_PROCESSING, RECORDING_MISSING } from './format';
import { deferred, testNote } from './testNote';

vi.mock('../../api', async () => (await import('../../api.mock')).mockApi());

beforeEach(() => {
  vi.useFakeTimers({ shouldAdvanceTime: true });
  vi.resetAllMocks();
  vi.mocked(api.noteOpen).mockResolvedValue({ ok: true });
  vi.mocked(api.noteRerun).mockResolvedValue({ ok: true });
  vi.mocked(api.noteArchive).mockResolvedValue({ ok: true });
  vi.mocked(api.noteTrash).mockResolvedValue({ ok: true });
});

afterEach(() => {
  vi.useRealTimers();
});

function setup(note: Note = testNote()) {
  const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime });
  const props = {
    note,
    latest: vi.fn(() => note),
    flushAll: vi.fn(async () => {}),
    reload: vi.fn(async () => ({ ok: true })),
    markRerun: vi.fn(),
    onBack: vi.fn(),
    say: vi.fn(),
  };
  render(
    <>
      <Actions {...props} />
      <button type="button">Elsewhere</button>
    </>,
  );
  return { user, ...props };
}

const more = () => screen.getByRole('button', { name: 'More' });
const item = (name: string) => screen.getByRole('menuitem', { name });
const menu = () => screen.queryByRole('menu');
const names = () => screen.getAllByRole('menuitem').map((el) => el.textContent);
const tick = (ms: number) => act(() => vi.advanceTimersByTimeAsync(ms));

describe('Actions toolbar', () => {
  it('shows Open in Obsidian and a closed More menu button', () => {
    setup();
    expect(screen.getByRole('button', { name: 'Open in Obsidian' })).toBeInTheDocument();
    expect(more()).toHaveAttribute('aria-haspopup', 'menu');
    expect(more()).toHaveAttribute('aria-expanded', 'false');
    expect(more()).toHaveAttribute('aria-controls');
    expect(menu()).not.toBeInTheDocument();
    for (const name of ['Open folder', 'Rerun summary', 'Archive', 'Trash']) {
      expect(screen.queryByRole('button', { name })).not.toBeInTheDocument();
    }
  });

  it('opens on click with the items in order and the first focused', async () => {
    const { user } = setup();
    await user.click(more());
    expect(more()).toHaveAttribute('aria-expanded', 'true');
    expect(menu()).toHaveAttribute('id', more().getAttribute('aria-controls'));
    expect(names()).toEqual(['Open folder', 'Rerun summary', 'Archive', 'Trash']);
    expect(item('Open folder')).toHaveFocus();
    await user.click(more());
    expect(menu()).not.toBeInTheDocument();
  });

  it.each([
    ['{Enter}', 'Open folder'],
    [' ', 'Open folder'],
    ['{ArrowDown}', 'Open folder'],
    ['{ArrowUp}', 'Trash'],
  ])('opens with %s focusing %s', async (key, first) => {
    const { user } = setup();
    more().focus();
    await user.keyboard(key);
    expect(menu()).toBeInTheDocument();
    expect(item(first)).toHaveFocus();
  });

  it('moves with the arrows (wrapping), Home and End, one item in the tab order', async () => {
    const { user } = setup();
    await user.click(more());
    await user.keyboard('{ArrowUp}');
    expect(item('Trash')).toHaveFocus();
    await user.keyboard('{ArrowDown}');
    expect(item('Open folder')).toHaveFocus();
    await user.keyboard('{ArrowDown}');
    expect(item('Rerun summary')).toHaveFocus();
    await user.keyboard('{End}');
    expect(item('Trash')).toHaveFocus();
    await user.keyboard('{Home}');
    expect(item('Open folder')).toHaveFocus();
    expect(screen.getAllByRole('menuitem').filter((el) => el.tabIndex === 0)).toEqual([item('Open folder')]);
  });

  it('closes on Escape with focus back on More, on Tab and on a click outside', async () => {
    const { user } = setup();
    await user.click(more());
    await user.keyboard('{Escape}');
    expect(menu()).not.toBeInTheDocument();
    expect(more()).toHaveFocus();

    await user.keyboard('{Enter}');
    await user.tab();
    expect(menu()).not.toBeInTheDocument();

    await user.click(more());
    await user.click(document.body);
    expect(menu()).not.toBeInTheDocument();
  });

  it('opens the folder, closes the menu and shows a missing_file reply', async () => {
    vi.mocked(api.noteOpen).mockResolvedValue({ ok: false, code: 'missing_file', message: 'No vault note found' });
    const { user, say } = setup();
    await user.click(more());
    await user.keyboard('{Enter}');
    expect(api.noteOpen).toHaveBeenCalledExactlyOnceWith({ id: 7, what: 'folder' });
    expect(menu()).not.toBeInTheDocument();
    expect(more()).toHaveFocus();
    await waitFor(() => expect(say).toHaveBeenLastCalledWith('No vault note found'));
  });

  it('archives after flushing pending saves', async () => {
    const { user, flushAll, say } = setup();
    await user.click(more());
    await user.click(item('Archive'));
    expect(flushAll).toHaveBeenCalled();
    expect(api.noteArchive).toHaveBeenCalledExactlyOnceWith({ id: 7 });
    await waitFor(() => expect(say).toHaveBeenLastCalledWith(ARCHIVED));
    expect(menu()).not.toBeInTheDocument();
  });

  it('cannot archive or trash again while a call is in flight', async () => {
    const reply = deferred<Awaited<ReturnType<typeof api.noteArchive>>>();
    vi.mocked(api.noteArchive).mockReturnValue(reply.promise);
    const { user } = setup();
    await user.click(more());
    await user.click(item('Archive'));
    await user.click(more());
    expect(item('Archive')).toHaveAttribute('aria-disabled', 'true');
    expect(item('Trash')).toHaveAttribute('aria-disabled', 'true');
    await user.click(item('Archive'));
    await user.click(item('Trash'));
    expect(api.noteArchive).toHaveBeenCalledOnce();
    expect(item('Trash')).toBeInTheDocument();
    await act(async () => reply.resolve({ ok: true }));
  });

  it('returns focus to More after the Rerun dialog closes', async () => {
    const note = testNote({ title_edited: true });
    const { user } = setup(note);
    await user.click(more());
    await user.click(item('Rerun summary'));
    await user.click(await screen.findByRole('button', { name: 'Cancel' }));
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
    expect(more()).toHaveFocus();
    expect(api.noteRerun).not.toHaveBeenCalled();
  });

  it('shows Recording missing in the menu and does not run a disabled item', async () => {
    const { user } = setup(testNote({ recording_missing: true }));
    await user.click(more());
    for (const name of ['Open folder', 'Rerun summary']) {
      expect(item(name)).toHaveAttribute('aria-disabled', 'true');
      expect(item(name)).toHaveAccessibleDescription(RECORDING_MISSING);
    }
    expect(item('Archive')).not.toHaveAttribute('aria-disabled');
    expect(item('Trash')).not.toHaveAttribute('aria-disabled');
    expect(item('Open folder')).toHaveFocus();
    await user.keyboard('{Enter}');
    await user.click(item('Rerun summary'));
    expect(api.noteOpen).not.toHaveBeenCalled();
    expect(api.noteRerun).not.toHaveBeenCalled();
    expect(menu()).toBeInTheDocument();
  });

  it('disables Archive and Trash with the processing reason, reachable by the arrows', async () => {
    const { user } = setup(testNote({ status: 'processing' }));
    await user.click(more());
    expect(item('Open folder')).not.toHaveAttribute('aria-disabled');
    expect(item('Rerun summary')).not.toHaveAttribute('aria-disabled');
    for (const name of ['Archive', 'Trash']) {
      expect(item(name)).toHaveAttribute('aria-disabled', 'true');
      expect(item(name)).toHaveAccessibleDescription(NOT_WHILE_PROCESSING);
    }
    await user.keyboard('{End}');
    expect(item('Trash')).toHaveFocus();
    await user.keyboard('{Enter}');
    expect(screen.queryByRole('menuitem', { name: 'Confirm trash' })).not.toBeInTheDocument();
    expect(api.noteTrash).not.toHaveBeenCalled();
  });
});

describe('Actions Trash confirm', () => {
  it('keeps the menu open on Confirm trash, then reverts after 3 s without sending', async () => {
    const { user } = setup();
    await user.click(more());
    await user.click(item('Trash'));
    expect(item('Confirm trash')).toHaveFocus();
    await tick(CONFIRM_MS);
    expect(item('Trash')).toBeInTheDocument();
    expect(menu()).toBeInTheDocument();
    expect(api.noteTrash).not.toHaveBeenCalled();
  });

  it('trashes with the keyboard only', async () => {
    const { user, onBack, flushAll } = setup();
    more().focus();
    await user.keyboard('{ArrowDown}{ArrowDown}{ArrowDown}{ArrowDown}{Enter}');
    expect(item('Confirm trash')).toHaveFocus();
    await user.keyboard('{Enter}');
    expect(menu()).not.toBeInTheDocument();
    expect(flushAll).toHaveBeenCalled();
    expect(api.noteTrash).toHaveBeenCalledExactlyOnceWith({ id: 7 });
    await waitFor(() => expect(onBack).toHaveBeenCalledOnce());
  });

  it('shows a trash_failed reply and stays', async () => {
    vi.mocked(api.noteTrash).mockResolvedValue({ ok: false, code: 'trash_failed', message: 'Trash is full' });
    const { user, onBack, say } = setup();
    await user.click(more());
    await user.click(item('Trash'));
    await user.click(item('Confirm trash'));
    await waitFor(() => expect(say).toHaveBeenLastCalledWith('Trash is full'));
    expect(onBack).not.toHaveBeenCalled();
  });

  it('keeps the armed state across Escape until the 3 s run out', async () => {
    const { user } = setup();
    await user.click(more());
    await user.click(item('Trash'));
    await user.keyboard('{Escape}');
    expect(api.noteTrash).not.toHaveBeenCalled();
    await user.click(more());
    expect(item('Confirm trash')).toBeInTheDocument();
    await user.keyboard('{Escape}');
    await tick(CONFIRM_MS);
    await user.click(more());
    expect(item('Trash')).toBeInTheDocument();
    expect(api.noteTrash).not.toHaveBeenCalled();
  });
});
