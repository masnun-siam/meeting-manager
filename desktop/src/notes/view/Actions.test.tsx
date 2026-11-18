import { act, cleanup, render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { Note } from '../../api';
import * as api from '../../api';
import { Actions, ARCHIVED, CONFIRM_MS, COPIED_MS } from './Actions';
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
  vi.mocked(api.noteCopy).mockResolvedValue({ ok: true });
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
  it('shows the copy buttons, Open in Obsidian and a closed More menu button, in that order', () => {
    setup();
    const order = ['Copy markdown', 'Copy formatted', 'Open in Obsidian', 'More'];
    expect(screen.getAllByRole('button').slice(0, 4)).toEqual(
      order.map((name) => screen.getByRole('button', { name })),
    );
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
    expect(more()).toHaveFocus();
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

    act(() => more().focus());
    await user.keyboard('{Enter}');
    await user.tab({ shift: true });
    expect(menu()).not.toBeInTheDocument();
    expect(more()).toHaveFocus();

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

const copyButton = (name: string) => screen.getByRole('button', { name });

describe('Actions copy', () => {
  it.each([
    ['Copy markdown', 'markdown'],
    ['Copy formatted', 'rich'],
  ] as const)('%s copies once with format %s, after flushing pending saves', async (name, format) => {
    const flushed = deferred<void>();
    const { user, flushAll } = setup();
    flushAll.mockReturnValue(flushed.promise);
    await user.click(copyButton(name));
    expect(flushAll).toHaveBeenCalledOnce();
    expect(api.noteCopy).not.toHaveBeenCalled();
    await act(async () => flushed.resolve());
    expect(api.noteCopy).toHaveBeenCalledExactlyOnceWith({ id: 7, format });
  });

  it.each([
    ['done', {}],
    ['processing', { status: 'processing' }],
    ['failed', { status: 'failed', failed_stage: 'transcribe', error: 'x' }],
    ['no_speech', { status: 'no_speech' }],
    ['recording missing', { recording_missing: true }],
    ['recording deleted', { recording_deleted: true, recording_missing: true }],
  ] as const)('offers both copy buttons, enabled, for a %s note', (_, extra) => {
    setup(testNote(extra as Partial<Note>));
    for (const name of ['Copy markdown', 'Copy formatted']) {
      expect(copyButton(name)).toBeEnabled();
      expect(copyButton(name)).not.toHaveAttribute('aria-disabled');
    }
  });

  it('names the clicked button Copied for 2 s, announced, with focus kept', async () => {
    const { user } = setup();
    await user.click(copyButton('Copy markdown'));
    const copied = await screen.findByRole('button', { name: 'Copied' });
    expect(copied).toHaveFocus();
    expect(copyButton('Copy formatted')).toBeInTheDocument();
    expect(screen.getByRole('status')).toHaveTextContent('Copied');
    await tick(COPIED_MS - 500);
    expect(copyButton('Copied')).toBeInTheDocument();
    await tick(500);
    expect(copyButton('Copy markdown')).toHaveFocus();
    expect(screen.queryByRole('button', { name: 'Copied' })).not.toBeInTheDocument();
    expect(screen.getByRole('status')).toBeEmptyDOMElement();
  });

  it('restarts the 2 s on a second copy, and only the last clicked reads Copied', async () => {
    const { user } = setup();
    await user.click(copyButton('Copy markdown'));
    await screen.findByRole('button', { name: 'Copied' });
    await tick(1500);
    await user.click(copyButton('Copied'));
    await waitFor(() => expect(api.noteCopy).toHaveBeenCalledTimes(2));
    await tick(1500);
    expect(copyButton('Copied')).toBeInTheDocument();
    await tick(500);
    expect(screen.queryByRole('button', { name: 'Copied' })).not.toBeInTheDocument();

    await user.click(copyButton('Copy markdown'));
    await screen.findByRole('button', { name: 'Copied' });
    await user.click(copyButton('Copy formatted'));
    await waitFor(() => expect(copyButton('Copy markdown')).toBeInTheDocument());
    expect(screen.getByRole('button', { name: 'Copied' })).toHaveTextContent('Copied');
    expect(api.noteCopy).toHaveBeenLastCalledWith({ id: 7, format: 'rich' });
  });

  it('sends no second call while a copy is in flight', async () => {
    const reply = deferred<Awaited<ReturnType<typeof api.noteCopy>>>();
    vi.mocked(api.noteCopy).mockReturnValue(reply.promise);
    const { user } = setup();
    await user.click(copyButton('Copy markdown'));
    await waitFor(() => expect(copyButton('Copy markdown')).toHaveAttribute('aria-disabled', 'true'));
    expect(copyButton('Copy formatted')).toHaveAttribute('aria-disabled', 'true');
    expect(copyButton('Copy markdown')).toHaveFocus();
    await user.click(copyButton('Copy markdown'));
    await user.click(copyButton('Copy formatted'));
    expect(api.noteCopy).toHaveBeenCalledOnce();
    await act(async () => reply.resolve({ ok: true }));
    expect(await screen.findByRole('button', { name: 'Copied' })).not.toHaveAttribute('aria-disabled');
  });

  it.each([
    ['clipboard_failed', 'pasteboard busy', "Couldn't copy: pasteboard busy"],
    ['store_unavailable', 'disk I/O error', "Notes database can't be opened: disk I/O error"],
    ['unknown_note', 'No note 7', 'No note 7'],
  ] as const)('shows a %s reply in the status line and never reads Copied', async (code, message, text) => {
    vi.mocked(api.noteCopy).mockResolvedValue({ ok: false, code, message });
    const { user, say } = setup();
    await user.click(copyButton('Copy formatted'));
    await waitFor(() => expect(say).toHaveBeenLastCalledWith(text));
    expect(say).toHaveBeenNthCalledWith(1, null);
    expect(copyButton('Copy formatted')).not.toHaveAttribute('aria-disabled');
    expect(screen.queryByRole('button', { name: 'Copied' })).not.toBeInTheDocument();
  });

  it('shows a rejected call as Could not reach the Meeting app', async () => {
    vi.mocked(api.noteCopy).mockRejectedValue(new Error('ipc down'));
    const { user, say } = setup();
    await user.click(copyButton('Copy markdown'));
    await waitFor(() =>
      expect(say).toHaveBeenLastCalledWith(expect.stringMatching(/^Could not reach the Meeting app/)),
    );
    expect(screen.queryByRole('button', { name: 'Copied' })).not.toBeInTheDocument();
  });

  it('operates with Tab, Enter and Space', async () => {
    const { user } = setup();
    await user.tab();
    expect(copyButton('Copy markdown')).toHaveFocus();
    await user.keyboard('{Enter}');
    await waitFor(() => expect(api.noteCopy).toHaveBeenLastCalledWith({ id: 7, format: 'markdown' }));
    await user.tab();
    expect(copyButton('Copy formatted')).toHaveFocus();
    await user.keyboard(' ');
    await waitFor(() => expect(api.noteCopy).toHaveBeenLastCalledWith({ id: 7, format: 'rich' }));
    expect(await screen.findByRole('button', { name: 'Copied' })).toHaveFocus();
  });

  it('clears the timer on unmount', async () => {
    const { user } = setup();
    await user.click(copyButton('Copy markdown'));
    await screen.findByRole('button', { name: 'Copied' });
    expect(vi.getTimerCount()).toBeGreaterThan(0);
    cleanup();
    expect(vi.getTimerCount()).toBe(0);
  });
});
