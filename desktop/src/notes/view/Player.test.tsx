import { act, cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { StrictMode } from 'react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { Note } from '../../api';
import * as api from '../../api';
import { LOADING_RECORDING, RECORDING_DELETED, RECORDING_MISSING, UNPLAYABLE } from './format';
import { media, stubMedia } from './mediaStub';
import { Player, type PlayerHandle } from './Player';
import { deferred, testNote } from './testNote';

vi.mock('../../api', async () => (await import('../../api.mock')).mockApi());

const noteOpen = vi.mocked(api.noteOpen);

beforeEach(() => {
  vi.resetAllMocks();
  noteOpen.mockResolvedValue({ ok: true });
  stubMedia();
});

afterEach(() => {
  // Unmount while the stubs are in place: the player pauses and reloads its element on the way out.
  cleanup();
  vi.restoreAllMocks();
});

type Setup = { note?: Partial<Note>; reload?: () => Promise<unknown> };

function setup({ note = {}, reload = vi.fn(async () => ({ ok: true })) }: Setup = {}) {
  const say = vi.fn();
  const handles: (PlayerHandle | null)[] = [];
  const onHandle = vi.fn((handle: PlayerHandle | null) => {
    handles.push(handle);
  });
  const props = { say, reload, onHandle };
  const view = render(<Player note={testNote(note)} {...props} />);
  const rerender = (next: Partial<Note>) => view.rerender(<Player note={testNote(next)} {...props} />);
  return { ...view, say, reload, onHandle, handle: () => handles[handles.length - 1], rerender };
}

const element = () => document.querySelector('audio') as HTMLAudioElement;
const player = () => screen.getByRole('group', { name: 'Recording player' });
const seekBar = () => screen.getByRole('slider', { name: 'Seek' });

/** The file's length becomes known. */
function load(duration = 3600) {
  media.duration = duration;
  fireEvent.loadedMetadata(element());
}

function playTo(seconds: number) {
  media.time = seconds;
  fireEvent.timeUpdate(element());
}

describe('Player', () => {
  it('points a metadata-only audio element at the recording, with no native controls or CORS mode', () => {
    setup();

    const audio = element();
    expect(audio).toHaveAttribute('src', 'recording://localhost/7');
    expect(audio).toHaveAttribute('preload', 'metadata');
    expect(audio).not.toHaveAttribute('controls');
    expect(audio).not.toHaveAttribute('crossorigin');
    expect(audio).not.toHaveAttribute('autoplay');
  });

  it("keeps its source through StrictMode's second mount", () => {
    render(
      <StrictMode>
        <Player note={testNote()} say={vi.fn()} reload={vi.fn()} onHandle={vi.fn()} />
      </StrictMode>,
    );

    expect(element()).toHaveAttribute('src', 'recording://localhost/7');
  });

  it('shows Loading recording… with Play not ready until the length is known', () => {
    const { handle } = setup();

    expect(within(player()).getByRole('status')).toHaveTextContent(LOADING_RECORDING);
    expect(screen.getByRole('button', { name: 'Play' })).toBeDisabled();
    expect(seekBar()).toBeDisabled();
    expect(handle()?.ready).toBe(false);

    load();

    expect(within(player()).getByRole('status')).toBeEmptyDOMElement();
    expect(screen.getByRole('button', { name: 'Play' })).toBeEnabled();
    expect(handle()?.ready).toBe(true);
  });

  it('shows the current time and the length as h:mm:ss, also as the seek bar value text', () => {
    setup();
    load(3 * 3600);

    playTo(12);

    expect(within(player()).getByText('0:00:12')).toBeInTheDocument();
    expect(within(player()).getByText('3:00:00')).toBeInTheDocument();
    expect(seekBar()).toHaveAttribute('aria-valuetext', '0:00:12 of 3:00:00');
  });

  it('plays and pauses with one button named after what it does', async () => {
    const user = userEvent.setup();
    setup();
    load();

    await user.click(screen.getByRole('button', { name: 'Play' }));
    expect(HTMLMediaElement.prototype.play).toHaveBeenCalledTimes(1);
    expect(screen.getByRole('button', { name: 'Pause' })).toBeInTheDocument();

    await user.click(screen.getByRole('button', { name: 'Pause' }));
    expect(media.paused).toBe(true);
    expect(screen.getByRole('button', { name: 'Play' })).toBeInTheDocument();
  });

  it('shows Play again when the recording ends', async () => {
    const user = userEvent.setup();
    setup();
    load();
    await user.click(screen.getByRole('button', { name: 'Play' }));

    media.paused = true;
    fireEvent.ended(element());

    expect(screen.getByRole('button', { name: 'Play' })).toBeInTheDocument();
  });

  it('seeks with the seek bar', () => {
    setup();
    load();

    fireEvent.change(seekBar(), { target: { value: '1800' } });

    expect(media.time).toBe(1800);
    expect(seekBar()).toHaveAttribute('aria-valuetext', '0:30:00 of 1:00:00');
  });

  it('moves 5 s per arrow key, to the start with Home and to the end with End', async () => {
    const user = userEvent.setup();
    setup();
    load();
    playTo(60);
    seekBar().focus();

    await user.keyboard('{ArrowRight}');
    expect(media.time).toBe(65);
    await user.keyboard('{ArrowLeft}{ArrowLeft}');
    expect(media.time).toBe(55);
    await user.keyboard('{End}');
    expect(media.time).toBe(3600);
    await user.keyboard('{ArrowUp}');
    expect(media.time).toBe(3600);
    await user.keyboard('{Home}{ArrowDown}');
    expect(media.time).toBe(0);
  });

  it('offers 1×, 1.25×, 1.5× and 2×, starts at 1× and sets the playback rate', async () => {
    const user = userEvent.setup();
    setup();
    load();
    const speed = screen.getByRole('combobox', { name: 'Speed' });

    expect(
      within(speed)
        .getAllByRole('option')
        .map((option) => option.textContent),
    ).toEqual(['1×', '1.25×', '1.5×', '2×']);
    expect(speed).toHaveValue('1');

    await user.selectOptions(speed, '1.5');

    expect(element().playbackRate).toBe(1.5);
    expect(element().defaultPlaybackRate).toBe(1.5);
  });

  it('reaches every control by Tab and plays with the keyboard', async () => {
    const user = userEvent.setup();
    setup();
    load();

    await user.tab();
    expect(screen.getByRole('button', { name: 'Play' })).toHaveFocus();
    await user.keyboard(' ');
    expect(screen.getByRole('button', { name: 'Pause' })).toHaveFocus();
    await user.tab();
    expect(seekBar()).toHaveFocus();
    await user.tab();
    expect(screen.getByRole('combobox', { name: 'Speed' })).toHaveFocus();
  });

  it('uses the stored length when the file has none in its header (WebM)', () => {
    setup({ note: { duration_s: 1830 } });

    load(Number.POSITIVE_INFINITY);

    expect(within(player()).getByText('0:30:30')).toBeInTheDocument();
    expect(seekBar()).toHaveAttribute('max', '1830');
  });

  it('pauses and empties the element when it unmounts, and hands up null', async () => {
    const user = userEvent.setup();
    const { unmount, onHandle } = setup();
    load();
    await user.click(screen.getByRole('button', { name: 'Play' }));
    const audio = element();

    unmount();

    expect(media.paused).toBe(true);
    expect(audio).not.toHaveAttribute('src');
    expect(HTMLMediaElement.prototype.load).toHaveBeenCalled();
    expect(onHandle).toHaveBeenLastCalledWith(null);
  });

  describe('without a recording', () => {
    it('says Recording deleted after Delete recording', () => {
      const { onHandle } = setup({ note: { media: null, recording_deleted: true } });

      expect(screen.getByText(RECORDING_DELETED)).toBeInTheDocument();
      expect(element()).toBeNull();
      expect(onHandle).not.toHaveBeenCalled();
    });

    it('says Recording missing when the file is gone', () => {
      setup({ note: { media: null, recording_missing: true } });

      expect(screen.getByText(RECORDING_MISSING)).toBeInTheDocument();
      expect(screen.queryByRole('group', { name: 'Recording player' })).not.toBeInTheDocument();
    });

    it('stops and is replaced when a later reply has no recording', async () => {
      const user = userEvent.setup();
      const { rerender, onHandle } = setup();
      load();
      await user.click(screen.getByRole('button', { name: 'Play' }));

      rerender({ media: null, recording_deleted: true });

      expect(media.paused).toBe(true);
      expect(screen.getByText(RECORDING_DELETED)).toBeInTheDocument();
      expect(onHandle).toHaveBeenLastCalledWith(null);
    });
  });

  describe('a media error', () => {
    it('with playable_hint false still tries the file and says nothing before an error', () => {
      setup({ note: { media: { kind: 'video', playable_hint: false } } });

      expect(element()).toHaveAttribute('src', 'recording://localhost/7');
      expect(screen.queryByText(UNPLAYABLE)).not.toBeInTheDocument();
    });

    it('reloads the note, then says the file cannot be played and offers Open recording', async () => {
      const user = userEvent.setup();
      const { reload, handle, say } = setup();
      load();

      fireEvent.error(element());

      expect(await within(player()).findByRole('status')).toHaveTextContent(UNPLAYABLE);
      expect(reload).toHaveBeenCalledTimes(1);
      expect(handle()?.ready).toBe(false);
      expect(screen.queryByRole('button', { name: 'Play' })).not.toBeInTheDocument();

      await user.click(screen.getByRole('button', { name: 'Open recording' }));

      expect(noteOpen).toHaveBeenCalledWith({ id: 7, what: 'recording' });
      expect(say).toHaveBeenLastCalledWith(null);
    });

    it('shows why a failed Open recording failed in the note status line', async () => {
      const user = userEvent.setup();
      noteOpen.mockResolvedValue({ ok: false, code: 'open_failed', message: 'no app for .mkv' });
      const { say } = setup();
      fireEvent.error(element());

      await user.click(await screen.findByRole('button', { name: 'Open recording' }));

      await waitFor(() => expect(say).toHaveBeenLastCalledWith("Couldn't open the recording: no app for .mkv"));
    });

    it('ends in Recording missing, not the unplayable message, when the file vanished', async () => {
      const reply = deferred<unknown>();
      const { rerender } = setup({ reload: () => reply.promise });
      load();

      fireEvent.error(element());
      expect(screen.queryByText(UNPLAYABLE)).not.toBeInTheDocument();
      rerender({ media: null, recording_missing: true });
      await act(async () => reply.resolve({ ok: true }));

      expect(screen.getByText(RECORDING_MISSING)).toBeInTheDocument();
      expect(screen.queryByText(UNPLAYABLE)).not.toBeInTheDocument();
    });
  });

  describe('the handle', () => {
    it('seeks without starting playback, clamped to the recording', () => {
      const { handle } = setup();
      load(600);

      act(() => handle()?.seek(42));
      expect(media.time).toBe(42);
      expect(media.paused).toBe(true);

      act(() => handle()?.seek(9999));
      expect(media.time).toBe(600);
      act(() => handle()?.seek(-3));
      expect(media.time).toBe(0);
    });

    it('seeks while playing and keeps playing', async () => {
      const user = userEvent.setup();
      const { handle } = setup();
      load();
      await user.click(screen.getByRole('button', { name: 'Play' }));

      act(() => handle()?.seek(90));

      expect(media.time).toBe(90);
      expect(media.paused).toBe(false);
    });

    it('does not seek while loading', () => {
      const { handle } = setup();

      act(() => handle()?.seek(42));

      expect(media.time).toBe(0);
    });

    it('pauses a playing recording', async () => {
      const user = userEvent.setup();
      const { handle } = setup();
      load();
      await user.click(screen.getByRole('button', { name: 'Play' }));

      act(() => handle()?.pause());

      expect(media.paused).toBe(true);
      expect(screen.getByRole('button', { name: 'Play' })).toBeInTheDocument();
    });
  });
});
