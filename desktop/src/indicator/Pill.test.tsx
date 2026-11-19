import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { act, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { Notice } from '../api';
import * as api from '../api';
import { statusReply } from '../recording/test-status';
import { POLL_MS } from '../recording/useRecorderStatus';
import { Pill, SHOW_MS } from './Pill';

vi.mock('../api', async () => (await import('../api.mock')).mockApi());

const recorderStatus = vi.mocked(api.recorderStatus);

beforeEach(() => {
  vi.useFakeTimers({ shouldAdvanceTime: true });
  vi.resetAllMocks();
  recorderStatus.mockResolvedValue(statusReply());
  vi.mocked(api.recorderPause).mockResolvedValue({ ok: true, state: 'paused' });
  vi.mocked(api.recorderResume).mockResolvedValue({ ok: true, state: 'recording' });
  vi.mocked(api.recorderStop).mockResolvedValue({ ok: true, state: 'saving' });
  vi.mocked(api.indicatorDismiss).mockResolvedValue({ ok: true });
  vi.mocked(api.windowOpenMain).mockResolvedValue({ ok: true });
});

afterEach(() => {
  vi.useRealTimers();
});

function setup() {
  const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime });
  const view = render(<Pill />);
  return { user, ...view };
}

const poll = () => act(() => vi.advanceTimersByTimeAsync(POLL_MS));
// Later polls never answer, so only a command's reply can change the state.
const holdPolls = () => recorderStatus.mockReturnValue(new Promise(() => {}));
const button = (name: string) => screen.getByRole('button', { name });

const saved: Notice = { kind: 'saved', message: 'Saved, sent for processing', folder: null };
const tooShort: Notice = { kind: 'too_short', message: 'Under 10 seconds: saved, not processed', folder: null };
const setupNotice: Notice = { kind: 'setup', message: 'Run meeting install first', folder: null };
const errorNotice: Notice = { kind: 'error', message: 'ffmpeg: invalid data', folder: '/r/meeting-1' };

/** A session the page saw, then saving, then idle with `notice`. */
async function endWith(notice: Notice) {
  recorderStatus.mockResolvedValueOnce(statusReply({ state: 'saving', elapsed_s: 30 }));
  recorderStatus.mockResolvedValue(statusReply({ notice }));
  const view = setup();
  expect(await screen.findByText('Saving…')).toBeInTheDocument();
  await poll();
  return view;
}

describe('Pill', () => {
  it('recording: the time as H:MM:SS, Pause and Stop, and the time opens the main window', async () => {
    recorderStatus.mockResolvedValue(statusReply({ state: 'recording', elapsed_s: 65 }));
    const { user } = setup();

    expect(await screen.findByRole('timer', { name: 'Recorded time' })).toHaveTextContent('0:01:05');
    expect(button('Pause recording')).toBeInTheDocument();
    expect(button('Stop recording')).toBeInTheDocument();
    expect(screen.queryByText('Paused')).not.toBeInTheDocument();

    await user.click(button('Open Meeting'));
    expect(api.windowOpenMain).toHaveBeenCalledOnce();
    expect(api.indicatorDismiss).not.toHaveBeenCalled();
  });

  it.each([
    [3725, '1:02:05'],
    [36_000, '10:00:00'],
  ])('elapsed %i s reads %s', async (elapsed_s, text) => {
    recorderStatus.mockResolvedValue(statusReply({ state: 'recording', elapsed_s }));
    setup();
    expect(await screen.findByRole('timer', { name: 'Recorded time' })).toHaveTextContent(text);
  });

  it('Pause shows Paused and Resume; Resume goes back to recording', async () => {
    recorderStatus.mockResolvedValue(statusReply({ state: 'recording', elapsed_s: 10 }));
    const { user } = setup();

    const pause = await screen.findByRole('button', { name: 'Pause recording' });
    holdPolls();
    await user.click(pause);
    expect(api.recorderPause).toHaveBeenCalledOnce();
    expect(await screen.findByRole('button', { name: 'Resume recording' })).toBeInTheDocument();
    expect(screen.getByText('Paused')).toBeInTheDocument();

    await user.click(button('Resume recording'));
    expect(api.recorderResume).toHaveBeenCalledOnce();
    expect(await screen.findByRole('button', { name: 'Pause recording' })).toBeInTheDocument();
    expect(screen.queryByText('Paused')).not.toBeInTheDocument();
  });

  it('Stop takes one click, then saving shows no buttons and "Saving…"', async () => {
    recorderStatus.mockResolvedValue(statusReply({ state: 'recording', elapsed_s: 10 }));
    const { user } = setup();

    const stop = await screen.findByRole('button', { name: 'Stop recording' });
    holdPolls();
    await user.click(stop);
    expect(api.recorderStop).toHaveBeenCalledOnce();
    expect(await screen.findByText('Saving…')).toBeInTheDocument();
    expect(screen.queryAllByRole('button')).toHaveLength(0);
  });

  it.each([
    ['saved', saved],
    ['too_short', tooShort],
  ])('a %s outcome shows its message and dismisses after 4 s, not before', async (_, notice) => {
    await endWith(notice);
    expect(screen.getByRole('status')).toHaveTextContent(notice.message);
    expect(screen.queryAllByRole('button')).toHaveLength(0);

    // `shouldAdvanceTime` lets real time move the clock too: leave a margin.
    await act(() => vi.advanceTimersByTimeAsync(SHOW_MS - 1000));
    expect(api.indicatorDismiss).not.toHaveBeenCalled();
    await act(() => vi.advanceTimersByTimeAsync(1000));
    expect(api.indicatorDismiss).toHaveBeenCalledOnce();
  });

  it.each([
    ['setup', setupNotice],
    ['error', errorNotice],
  ])('a %s outcome stays with Open Meeting and Dismiss; Dismiss dismisses', async (_, notice) => {
    const { user } = await endWith(notice);
    expect(screen.getByRole('status')).toHaveTextContent(notice.message);

    await act(() => vi.advanceTimersByTimeAsync(SHOW_MS * 3));
    expect(api.indicatorDismiss).not.toHaveBeenCalled();
    expect(screen.getByRole('status')).toHaveTextContent(notice.message);

    await user.click(button('Dismiss'));
    expect(api.indicatorDismiss).toHaveBeenCalledOnce();
    expect(api.windowOpenMain).not.toHaveBeenCalled();
  });

  it('Open Meeting on an error outcome opens the main window and dismisses', async () => {
    const { user } = await endWith(errorNotice);

    await user.click(button('Open Meeting'));
    expect(api.windowOpenMain).toHaveBeenCalledOnce();
    expect(api.indicatorDismiss).toHaveBeenCalledOnce();
    expect(vi.mocked(api.windowOpenMain).mock.invocationCallOrder[0]).toBeLessThan(
      vi.mocked(api.indicatorDismiss).mock.invocationCallOrder[0],
    );
  });

  it('Open Meeting that fails keeps the outcome pill and shows why', async () => {
    vi.mocked(api.windowOpenMain).mockResolvedValue({ ok: false, code: 'window_failed', message: 'No main window' });
    const { user } = await endWith(errorNotice);

    await user.click(button('Open Meeting'));
    expect(screen.getByRole('alert')).toHaveTextContent('No main window');
    expect(api.indicatorDismiss).not.toHaveBeenCalled();
    expect(button('Dismiss')).toBeInTheDocument();
  });

  it('a new recording replaces a waiting outcome', async () => {
    await endWith(setupNotice);
    expect(button('Dismiss')).toBeInTheDocument();

    recorderStatus.mockResolvedValue(statusReply({ state: 'recording', elapsed_s: 1 }));
    await poll();
    expect(screen.getByRole('timer', { name: 'Recorded time' })).toHaveTextContent('0:00:01');
    expect(screen.queryByText(setupNotice.message)).not.toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'Dismiss' })).not.toBeInTheDocument();
  });

  it('a not_recording control reply shows nothing', async () => {
    recorderStatus.mockResolvedValue(statusReply({ state: 'recording' }));
    vi.mocked(api.recorderStop).mockResolvedValue({
      ok: false,
      code: 'not_recording',
      message: 'Nothing is recording',
    });
    const { user } = setup();

    await user.click(await screen.findByRole('button', { name: 'Stop recording' }));
    expect(api.recorderStop).toHaveBeenCalledOnce();
    expect(screen.queryByRole('alert')).not.toBeInTheDocument();
  });

  it('another error reply shows its message for 4 s, then clears', async () => {
    recorderStatus.mockResolvedValue(statusReply({ state: 'recording' }));
    vi.mocked(api.windowOpenMain).mockResolvedValue({ ok: false, code: 'window_failed', message: 'No main window' });
    const { user } = setup();

    await user.click(await screen.findByRole('button', { name: 'Open Meeting' }));
    expect(screen.getByRole('alert')).toHaveTextContent('No main window');
    expect(button('Pause recording')).toBeEnabled();
    expect(screen.queryByRole('timer')).not.toBeInTheDocument();

    await act(() => vi.advanceTimersByTimeAsync(SHOW_MS));
    expect(screen.queryByRole('alert')).not.toBeInTheDocument();
    expect(screen.getByRole('timer', { name: 'Recorded time' })).toBeInTheDocument();
  });

  it('a rejected call shows a message instead of crashing', async () => {
    recorderStatus.mockResolvedValue(statusReply({ state: 'recording' }));
    vi.mocked(api.recorderPause).mockRejectedValue(new Error('ipc closed'));
    const { user } = setup();

    await user.click(await screen.findByRole('button', { name: 'Pause recording' }));
    expect(screen.getByRole('alert')).toHaveTextContent('Could not reach the Meeting app: ipc closed');
  });

  it('an old notice, idle before any session was seen, shows no outcome', async () => {
    recorderStatus.mockResolvedValue(statusReply({ notice: saved }));
    const { container } = setup();

    await poll();
    await poll();
    expect(container).toBeEmptyDOMElement();
    await act(() => vi.advanceTimersByTimeAsync(SHOW_MS));
    expect(api.indicatorDismiss).not.toHaveBeenCalled();
  });

  it.each([
    ['saved', saved],
    ['error', errorNotice],
  ])('a %s outcome goes when the notice is cleared in the main window, with no indicatorDismiss', async (_, notice) => {
    await endWith(notice);
    expect(screen.getByRole('status')).toHaveTextContent(notice.message);

    recorderStatus.mockResolvedValue(statusReply());
    await poll();

    expect(screen.queryByText(notice.message)).not.toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'Open Meeting' })).not.toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'Dismiss' })).not.toBeInTheDocument();
    expect(api.indicatorDismiss).not.toHaveBeenCalled();
  });

  it('marks the pill as the drag region and uses no inline style', async () => {
    recorderStatus.mockResolvedValue(statusReply({ state: 'recording' }));
    const { container } = setup();

    await screen.findByRole('timer');
    expect(container.firstElementChild).toHaveAttribute('data-tauri-drag-region', 'deep');
    expect(container.querySelector('[style]')).toBeNull();
  });
});

describe('Pill.module.css', () => {
  it('turns the dot pulse off under Reduce motion', () => {
    // Vitest stubs CSS imports, so the rule is read from the file.
    const css = readFileSync(join(import.meta.dirname, 'Pill.module.css'), 'utf8');
    expect(css).toMatch(/@media \(prefers-reduced-motion: reduce\) \{\s*\.dot \{\s*animation: none;/);
  });
});
