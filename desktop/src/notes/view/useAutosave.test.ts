import { act, renderHook } from '@testing-library/react';
import { useState } from 'react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { Note } from '../../api';
import * as api from '../../api';
import { MY_NOTES_LIMIT, TOO_LARGE } from './format';
import { deferred, testNote } from './testNote';
import { SAVE_DELAY_MS, useAutosave } from './useAutosave';

vi.mock('../../api', async () => (await import('../../api.mock')).mockApi());

const noteUpdate = vi.mocked(api.noteUpdate);
type UpdateReply = Awaited<ReturnType<typeof api.noteUpdate>>;
const saved = (note: Note): UpdateReply => ({ ok: true, note, vault: 'ok', vault_error: null });

beforeEach(() => {
  vi.useFakeTimers();
  vi.resetAllMocks();
  noteUpdate.mockImplementation(async (req) => saved(testNote({ ...req, id: 7 } as Partial<Note>)));
});

afterEach(() => {
  vi.useRealTimers();
});

function setup(initial: Note = testNote()) {
  const reload = vi.fn();
  const hook = renderHook(() => {
    const [note, setNote] = useState<Note | null>(initial);
    return { note, ...useAutosave({ id: 7, note, apply: setNote, reload }) };
  });
  return { ...hook, reload, now: () => hook.result.current };
}

const tick = (ms: number) => act(() => vi.advanceTimersByTimeAsync(ms));

describe('useAutosave', () => {
  it('saves 1 s after typing stops, sending only the changed field', async () => {
    const { now } = setup();
    act(() => now().change('title', 'Planning'));

    await tick(SAVE_DELAY_MS - 1);
    expect(noteUpdate).not.toHaveBeenCalled();
    await tick(1);
    expect(noteUpdate).toHaveBeenCalledExactlyOnceWith({ id: 7, title: 'Planning' });
    expect(now().saveState).toBe('saved');
    expect(now().note?.title).toBe('Planning');
  });

  it('restarts the timer on each change and sends the latest text once', async () => {
    const { now } = setup();
    act(() => now().change('title', 'Pla'));
    await tick(800);
    act(() => now().change('title', 'Planning'));
    await tick(800);
    expect(noteUpdate).not.toHaveBeenCalled();

    await tick(200);
    expect(noteUpdate).toHaveBeenCalledExactlyOnceWith({ id: 7, title: 'Planning' });
  });

  it('saves at once on blur', async () => {
    const { now } = setup();
    act(() => now().change('my_notes', 'remember the deck'));
    await act(async () => now().blur('my_notes'));

    expect(noteUpdate).toHaveBeenCalledExactlyOnceWith({ id: 7, my_notes: 'remember the deck' });
    await tick(SAVE_DELAY_MS);
    expect(noteUpdate).toHaveBeenCalledOnce();
  });

  it('sends nothing when the text is back to what is stored', async () => {
    const { now } = setup();
    act(() => now().change('title', 'Weekly sync!'));
    act(() => now().change('title', 'Weekly sync'));
    await tick(SAVE_DELAY_MS);
    await act(async () => now().blur('title'));

    expect(noteUpdate).not.toHaveBeenCalled();
  });

  it('sends one save at a time; a queued field sends its latest text after the reply', async () => {
    const first = deferred<UpdateReply>();
    noteUpdate.mockReturnValueOnce(first.promise);
    const { now } = setup();

    act(() => now().change('title', 'A'));
    await act(async () => now().blur('title'));
    act(() => now().change('my_notes', 'one'));
    await act(async () => now().blur('my_notes'));
    act(() => now().change('my_notes', 'one two'));
    await act(async () => now().blur('my_notes'));
    expect(noteUpdate).toHaveBeenCalledOnce();

    await act(async () => first.resolve(saved(testNote({ title: 'A' }))));
    expect(noteUpdate).toHaveBeenCalledTimes(2);
    expect(noteUpdate).toHaveBeenLastCalledWith({ id: 7, my_notes: 'one two' });
  });

  it('keeps text typed after a request left when its reply arrives', async () => {
    const first = deferred<UpdateReply>();
    noteUpdate.mockReturnValueOnce(first.promise);
    const { now } = setup();

    act(() => now().change('title', 'Plan'));
    await act(async () => now().blur('title'));
    act(() => now().change('title', 'Planning'));
    await act(async () => first.resolve(saved(testNote({ title: 'Plan' }))));

    expect(now().note?.title).toBe('Plan');
    expect(now().value('title')).toBe('Planning');
    await tick(SAVE_DELAY_MS);
    expect(noteUpdate).toHaveBeenLastCalledWith({ id: 7, title: 'Planning' });
  });

  it('refuses "My notes" over 1 MB without sending', async () => {
    const { now } = setup();
    act(() => now().change('my_notes', 'x'.repeat(MY_NOTES_LIMIT + 1)));
    await act(async () => now().blur('my_notes'));

    expect(noteUpdate).not.toHaveBeenCalled();
    expect(now().problem).toBe(TOO_LARGE);
    expect(now().saveState).toBe('error');
  });

  it('counts bytes, not characters, against the 1 MB limit', async () => {
    const { now } = setup();
    // 'আ' is 3 bytes in UTF-8.
    act(() => now().change('my_notes', 'আ'.repeat(Math.floor(MY_NOTES_LIMIT / 3) + 1)));
    await act(async () => now().blur('my_notes'));

    expect(noteUpdate).not.toHaveBeenCalled();
  });

  it('shows the too-large message for a too_large reply', async () => {
    noteUpdate.mockResolvedValueOnce({ ok: false, code: 'too_large', message: 'my_notes is too large' });
    const { now } = setup();
    act(() => now().change('my_notes', 'notes'));
    await act(async () => now().blur('my_notes'));

    expect(now().problem).toBe(TOO_LARGE);
    expect(now().value('my_notes')).toBe('notes');
  });

  it('shows not_editable, drops the draft and reloads the note', async () => {
    noteUpdate.mockResolvedValueOnce({ ok: false, code: 'not_editable', message: 'This note is processing' });
    const { now, reload } = setup();
    act(() => now().change('title', 'New'));
    await act(async () => now().blur('title'));

    expect(now().problem).toBe('This note is processing');
    expect(now().value('title')).toBe('Weekly sync');
    expect(reload).toHaveBeenCalledOnce();
  });

  it('shows a rejected call as an error and keeps the draft', async () => {
    noteUpdate.mockRejectedValueOnce(new Error('ipc closed'));
    const { now } = setup();
    act(() => now().change('title', 'New'));
    await act(async () => now().blur('title'));

    expect(now().problem).toBe('Could not reach the Meeting app: ipc closed');
    expect(now().value('title')).toBe('New');
  });

  it('flushes pending fields before a speaker rename', async () => {
    const { now } = setup();
    act(() => now().change('title', 'Plan with Rahim'));
    act(() => now().change('speaker:Speaker 1', 'Rahim Uddin '));
    await act(async () => now().blur('speaker:Speaker 1'));

    expect(noteUpdate.mock.calls.map(([req]) => req)).toEqual([
      { id: 7, title: 'Plan with Rahim' },
      { id: 7, speaker: { label: 'Speaker 1', name: 'Rahim Uddin' } },
    ]);
  });

  it('never sends an empty speaker name and shows the stored name again on blur', async () => {
    const { now } = setup();
    act(() => now().change('speaker:Speaker 1', '  '));
    await tick(SAVE_DELAY_MS);
    await act(async () => now().blur('speaker:Speaker 1'));

    expect(noteUpdate).not.toHaveBeenCalled();
    expect(now().value('speaker:Speaker 1')).toBe('Rahim');
  });

  it('reports a vault failure and retries with only the id', async () => {
    noteUpdate.mockResolvedValueOnce({
      ok: true,
      note: testNote({ title: 'New' }),
      vault: 'failed',
      vault_error: 'vault folder is missing',
    });
    const { now } = setup();
    act(() => now().change('title', 'New'));
    await act(async () => now().blur('title'));
    expect(now().vaultError).toBe('vault folder is missing');
    expect(now().note?.title).toBe('New');

    noteUpdate.mockResolvedValueOnce(saved(testNote({ title: 'New' })));
    await act(async () => now().retryVault());
    expect(noteUpdate).toHaveBeenLastCalledWith({ id: 7 });
    expect(now().vaultError).toBeNull();
  });

  it('flushAll sends every pending field and resolves after the replies', async () => {
    const { now } = setup();
    act(() => now().change('title', 'T'));
    act(() => now().change('my_notes', 'M'));
    await act(() => now().flushAll());

    expect(noteUpdate).toHaveBeenCalledTimes(2);
    expect(now().saveState).toBe('saved');
  });

  it('sends pending edits on unmount', async () => {
    const { now, unmount } = setup();
    act(() => now().change('my_notes', 'last words'));
    unmount();
    await act(() => vi.runAllTimersAsync());

    expect(noteUpdate).toHaveBeenCalledExactlyOnceWith({ id: 7, my_notes: 'last words' });
  });

  it('reports a focused field with unsaved text as dirty', () => {
    const { now } = setup();
    act(() => now().focus('title'));
    expect(now().focusedDirty()).toBe(false);
    act(() => now().change('title', 'x'));
    expect(now().focusedDirty()).toBe(true);
  });
});
