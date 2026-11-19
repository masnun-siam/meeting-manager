import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { act, render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import * as api from '../api';
import { formatElapsed, RecordingBar } from './RecordingBar';
import { deferred, statusReply } from './test-status';
import { POLL_MS } from './useRecorderStatus';

vi.mock('../api', async () => (await import('../api.mock')).mockApi());

const recorderStatus = vi.mocked(api.recorderStatus);

beforeEach(() => {
  vi.useFakeTimers({ shouldAdvanceTime: true });
  vi.resetAllMocks();
  recorderStatus.mockResolvedValue(statusReply());
});

afterEach(() => {
  vi.useRealTimers();
});

function setup() {
  const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime });
  render(<RecordingBar />);
  return user;
}

// Later polls never answer, so only a command's reply can change the state.
const holdPolls = () => recorderStatus.mockReturnValue(new Promise(() => {}));

const start = () => screen.getByRole('button', { name: 'Start recording' });
const language = () => screen.getByRole('combobox', { name: 'Language' });
const status = () => screen.getByRole('status');

describe('formatElapsed', () => {
  it('formats seconds as HH:MM:SS', () => {
    expect(formatElapsed(0)).toBe('00:00:00');
    expect(formatElapsed(3725.9)).toBe('01:02:05');
  });
});

describe('RecordingBar', () => {
  it('idle: shows the language from the status and starts an ad-hoc recording', async () => {
    recorderStatus.mockResolvedValue(statusReply({ language: 'bn' }));
    const user = setup();

    await waitFor(() => expect(start()).toBeEnabled());
    expect(language()).toHaveValue('bn');
    expect(screen.queryByRole('timer')).not.toBeInTheDocument();

    await user.selectOptions(language(), 'English');
    await user.click(start());
    expect(api.recorderStart).toHaveBeenCalledWith({ language: 'en', event_occurrence_id: null });
  });

  it('Start shows recording at once, so it cannot be pressed twice', async () => {
    const user = setup();
    await waitFor(() => expect(start()).toBeEnabled());

    holdPolls();
    await user.click(start());
    await waitFor(() => expect(start()).toBeDisabled());
    expect(api.recorderStart).toHaveBeenCalledOnce();
  });

  it('after a recording, the idle language is the one it used, not an older pick', async () => {
    const user = setup();
    await waitFor(() => expect(start()).toBeEnabled());
    await user.selectOptions(language(), 'Bengali');

    recorderStatus.mockResolvedValue(statusReply({ state: 'recording', language: 'en' }));
    await act(() => vi.advanceTimersByTimeAsync(POLL_MS));
    expect(language()).toHaveValue('en');

    recorderStatus.mockResolvedValue(statusReply({ state: 'idle', language: 'en' }));
    await act(() => vi.advanceTimersByTimeAsync(POLL_MS));
    expect(language()).toHaveValue('en');
    expect(start()).toBeEnabled();
  });

  it('recording: disables Start, locks the language and advances the timer', async () => {
    let elapsed = 240;
    recorderStatus.mockImplementation(async () =>
      statusReply({ state: 'recording', elapsed_s: elapsed++, language: 'bn' }),
    );
    setup();

    const timer = await screen.findByRole('timer', { name: 'Recorded time' });
    expect(timer).toHaveTextContent('00:04:00');
    expect(screen.getByText('● REC')).toBeInTheDocument();
    expect(start()).toBeDisabled();
    expect(language()).toBeDisabled();
    expect(language()).toHaveValue('bn');

    await act(() => vi.advanceTimersByTimeAsync(POLL_MS));
    expect(timer).toHaveTextContent('00:04:01');
  });

  it('paused: freezes the timer and offers Resume', async () => {
    recorderStatus.mockResolvedValue(statusReply({ state: 'paused', elapsed_s: 61 }));
    const user = setup();

    expect(await screen.findByRole('timer', { name: 'Recorded time' })).toHaveTextContent('00:01:01');
    expect(screen.getByText('II Paused')).toBeInTheDocument();
    await act(() => vi.advanceTimersByTimeAsync(POLL_MS * 3));
    expect(screen.getByRole('timer')).toHaveTextContent('00:01:01');

    holdPolls();
    await user.click(screen.getByRole('button', { name: 'Resume' }));
    expect(api.recorderResume).toHaveBeenCalledOnce();
    expect(await screen.findByText('● REC')).toBeInTheDocument();
  });

  it('Pause and Stop follow the returned state without waiting for a poll', async () => {
    recorderStatus.mockResolvedValue(statusReply({ state: 'recording', elapsed_s: 10 }));
    vi.mocked(api.recorderPause).mockResolvedValue({ ok: true, state: 'paused' });
    vi.mocked(api.recorderStop).mockResolvedValue({ ok: true, state: 'saving' });
    const user = setup();

    const pause = await screen.findByRole('button', { name: 'Pause' });
    holdPolls();
    await user.click(pause);
    expect(api.recorderPause).toHaveBeenCalledOnce();
    expect(await screen.findByRole('button', { name: 'Resume' })).toBeInTheDocument();

    await user.click(screen.getByRole('button', { name: 'Stop' }));
    expect(api.recorderStop).toHaveBeenCalledOnce();
    await waitFor(() => expect(status()).toHaveTextContent('Saving the recording…'));
  });

  it('saving: every control is disabled and a status line says so', async () => {
    recorderStatus.mockResolvedValue(statusReply({ state: 'saving', elapsed_s: 30 }));
    setup();

    expect(await screen.findByText('Saving the recording…')).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Pause' })).toBeDisabled();
    expect(screen.getByRole('button', { name: 'Stop' })).toBeDisabled();
    expect(start()).toBeDisabled();
  });

  it.each([
    ['permission_mic', 'Allow Meeting in System Settings › Privacy & Security › Microphone'],
    ['permission_audio', 'Allow Meeting in System Settings › Privacy & Security › Screen & System Audio Recording'],
  ] as const)('%s names the System Settings pane and Start stays enabled', async (code, text) => {
    vi.mocked(api.recorderStart).mockResolvedValue({ ok: false, code, message: 'denied' });
    const user = setup();

    await waitFor(() => expect(start()).toBeEnabled());
    await user.click(start());
    expect(status()).toHaveTextContent(text);
    expect(start()).toBeEnabled();

    vi.mocked(api.recorderStart).mockResolvedValue({ ok: true, folder: '/r/m' });
    await user.click(start());
    expect(api.recorderStart).toHaveBeenCalledTimes(2);
    expect(status()).not.toHaveTextContent(text);
  });

  it.each([
    'already_recording',
    'not_configured',
    'event_too_far',
    'unknown_event',
    'calendar_denied',
    'capture_failed',
  ] as const)('%s shows the reply message', async (code) => {
    vi.mocked(api.recorderStart).mockResolvedValue({ ok: false, code, message: `message for ${code}` });
    const user = setup();

    await waitFor(() => expect(start()).toBeEnabled());
    await user.click(start());
    expect(status()).toHaveTextContent(`message for ${code}`);
  });

  it('a rejected call shows a message instead of crashing', async () => {
    vi.mocked(api.recorderStart).mockRejectedValue(new Error('ipc closed'));
    const user = setup();

    await waitFor(() => expect(start()).toBeEnabled());
    await user.click(start());
    expect(status()).toHaveTextContent('Could not reach the Meeting app: ipc closed');
  });

  it('a control error shows its message', async () => {
    recorderStatus.mockResolvedValue(statusReply({ state: 'recording' }));
    vi.mocked(api.recorderStop).mockResolvedValue({
      ok: false,
      code: 'not_recording',
      message: 'Nothing is recording',
    });
    const user = setup();

    await user.click(await screen.findByRole('button', { name: 'Stop' }));
    expect(status()).toHaveTextContent('Nothing is recording');
  });

  it('shows the notice message until a status arrives without it', async () => {
    recorderStatus.mockResolvedValueOnce(
      statusReply({ notice: { kind: 'too_short', message: 'Under 10 seconds: saved, not processed', folder: null } }),
    );
    setup();

    expect(await screen.findByText('Under 10 seconds: saved, not processed')).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'Retry' })).not.toBeInTheDocument();

    await act(() => vi.advanceTimersByTimeAsync(POLL_MS));
    expect(screen.queryByText('Under 10 seconds: saved, not processed')).not.toBeInTheDocument();
  });

  it('Retry on a conversion failure calls recorderRetry and follows the returned state', async () => {
    const notice = { kind: 'error', message: 'ffmpeg: invalid data', folder: '/r/meeting-1' };
    recorderStatus.mockResolvedValue(statusReply({ notice }));
    const retry = deferred<Awaited<ReturnType<typeof api.recorderRetry>>>();
    vi.mocked(api.recorderRetry).mockReturnValue(retry.promise);
    const user = setup();

    expect(await screen.findByText('ffmpeg: invalid data')).toBeInTheDocument();
    holdPolls();
    await user.click(screen.getByRole('button', { name: 'Retry' }));
    expect(api.recorderRetry).toHaveBeenCalledWith({ folder: '/r/meeting-1' });
    expect(screen.getByRole('button', { name: 'Retry' })).toBeDisabled();

    await act(async () => retry.resolve({ ok: true, state: 'saving' }));
    await waitFor(() => expect(status()).toHaveTextContent('Saving the recording…'));
  });

  it.each(['not_found', 'already_recording'] as const)('Retry shows the %s message', async (code) => {
    const notice = { kind: 'error', message: 'ffmpeg: invalid data', folder: '/r/meeting-1' };
    recorderStatus.mockResolvedValue(statusReply({ notice }));
    vi.mocked(api.recorderRetry).mockResolvedValue({ ok: false, code, message: `retry ${code}` });
    const user = setup();

    await user.click(await screen.findByRole('button', { name: 'Retry' }));
    expect(status()).toHaveTextContent(`retry ${code}`);
    expect(screen.getByRole('button', { name: 'Retry' })).toBeEnabled();
  });

  it('stops polling when unmounted', async () => {
    const { unmount } = render(<RecordingBar />);
    await act(() => vi.advanceTimersByTimeAsync(POLL_MS));
    unmount();
    const calls = recorderStatus.mock.calls.length;

    await act(() => vi.advanceTimersByTimeAsync(POLL_MS * 3));
    expect(recorderStatus).toHaveBeenCalledTimes(calls);
  });

  describe('start error (quick-record FR-5)', () => {
    const MIC = 'Allow Meeting in System Settings › Privacy & Security › Microphone';
    const withError = () => statusReply({ start_error: { code: 'permission_mic', message: MIC } });
    const dismiss = () => within(screen.getByRole('alert')).getByRole('button', { name: 'Dismiss message' });

    it('shows the message verbatim in an alert with its ×, and no Dismiss button', async () => {
      recorderStatus.mockResolvedValue(withError());
      setup();

      expect(await screen.findByRole('alert')).toHaveTextContent(MIC);
      expect(dismiss()).toBeEnabled();
      expect(screen.queryByRole('button', { name: 'Dismiss' })).not.toBeInTheDocument();
    });

    it('× clears it and reads the status again at once', async () => {
      recorderStatus.mockResolvedValue(withError());
      vi.mocked(api.recorderDismissStartError).mockResolvedValue({ ok: true });
      const user = setup();
      await screen.findByRole('alert');
      const polls = recorderStatus.mock.calls.length;

      recorderStatus.mockResolvedValue(statusReply());
      await user.click(dismiss());
      // No timer advance: only the read Dismiss starts can clear it, not the next poll.
      await act(async () => {});

      expect(screen.queryByRole('alert')).not.toBeInTheDocument();
      expect(api.recorderDismissStartError).toHaveBeenCalledOnce();
      expect(recorderStatus).toHaveBeenCalledTimes(polls + 1);
    });

    it('a failed × keeps the alert and shows why in the message line', async () => {
      recorderStatus.mockResolvedValue(withError());
      vi.mocked(api.recorderDismissStartError).mockRejectedValue(new Error('ipc closed'));
      const user = setup();
      await screen.findByRole('alert');

      await user.click(dismiss());

      expect(status()).toHaveTextContent('Could not reach the Meeting app: ipc closed');
      expect(screen.getByRole('alert')).toHaveTextContent(MIC);
    });

    it('goes when a later status no longer has it, as after a successful Start', async () => {
      recorderStatus.mockResolvedValueOnce(withError()).mockResolvedValue(statusReply({ state: 'recording' }));
      setup();
      await screen.findByRole('alert');

      await act(() => vi.advanceTimersByTimeAsync(POLL_MS));

      expect(screen.queryByRole('alert')).not.toBeInTheDocument();
    });

    it('shows no alert when there is no start error', async () => {
      setup();
      await waitFor(() => expect(start()).toBeEnabled());

      expect(screen.queryByRole('alert')).not.toBeInTheDocument();
      expect(screen.queryByRole('button', { name: 'Dismiss message' })).not.toBeInTheDocument();
    });
  });

  describe('dismiss messages (continue-recording FR-1, FR-3)', () => {
    const saved = { kind: 'saved', message: 'Saved, sent for processing', folder: null } as const;
    const failed = { kind: 'error', message: 'ffmpeg: invalid data', folder: '/r/meeting-1' } as const;
    const MIC = 'Allow Meeting in System Settings › Privacy & Security › Microphone';
    const closeButtons = () => screen.queryAllByRole('button', { name: 'Dismiss message' });
    const closeOf = (text: string) => {
      const line = screen.getByText(text).parentElement as HTMLElement;
      return within(line).getByRole('button', { name: 'Dismiss message' });
    };
    const stopFails = (message: string) =>
      vi.mocked(api.recorderStop).mockResolvedValue({ ok: false, code: 'not_recording', message });

    it('every message line has a × named "Dismiss message"; the saving line does not', async () => {
      recorderStatus.mockResolvedValue(
        statusReply({ state: 'saving', notice: saved, start_error: { code: 'permission_mic', message: MIC } }),
      );
      setup();

      await screen.findByText('Saved, sent for processing');
      expect(closeOf(MIC)).toBeInTheDocument();
      expect(closeOf('Saved, sent for processing')).toBeInTheDocument();
      expect(closeButtons()).toHaveLength(2);
      expect(within(screen.getByText('Saving the recording…')).queryByRole('button')).not.toBeInTheDocument();
    });

    it('notice × calls recorderDismissNotice, reads again, and a later poll does not bring it back', async () => {
      recorderStatus.mockResolvedValue(statusReply({ notice: saved }));
      const user = setup();
      await screen.findByText('Saved, sent for processing');
      const polls = recorderStatus.mock.calls.length;

      recorderStatus.mockResolvedValue(statusReply());
      await user.click(closeOf('Saved, sent for processing'));
      await act(async () => {});

      expect(api.recorderDismissNotice).toHaveBeenCalledOnce();
      expect(recorderStatus).toHaveBeenCalledTimes(polls + 1);
      expect(screen.queryByText('Saved, sent for processing')).not.toBeInTheDocument();

      await act(() => vi.advanceTimersByTimeAsync(POLL_MS * 2));
      expect(screen.queryByText('Saved, sent for processing')).not.toBeInTheDocument();
      expect(api.recorderDismissStartError).not.toHaveBeenCalled();
    });

    it('an error notice has Retry and ×; × removes both', async () => {
      recorderStatus.mockResolvedValue(statusReply({ notice: failed }));
      const user = setup();
      await screen.findByText('ffmpeg: invalid data');
      expect(screen.getByRole('button', { name: 'Retry' })).toBeInTheDocument();

      recorderStatus.mockResolvedValue(statusReply());
      await user.click(closeOf('ffmpeg: invalid data'));
      await act(async () => {});

      expect(screen.queryByText('ffmpeg: invalid data')).not.toBeInTheDocument();
      expect(screen.queryByRole('button', { name: 'Retry' })).not.toBeInTheDocument();
      expect(api.recorderRetry).not.toHaveBeenCalled();
    });

    it.each([
      [
        'an error reply',
        () =>
          vi.mocked(api.recorderDismissNotice).mockResolvedValue({ ok: false, code: 'x', message: 'no lock' } as never),
        'no lock',
      ],
      [
        'a rejected call',
        () => vi.mocked(api.recorderDismissNotice).mockRejectedValue(new Error('ipc closed')),
        'Could not reach the Meeting app: ipc closed',
      ],
    ])('a failed notice × (%s) keeps the notice and shows why', async (_, fail, why) => {
      fail();
      recorderStatus.mockResolvedValue(statusReply({ notice: failed }));
      const user = setup();
      await screen.findByText('ffmpeg: invalid data');

      await user.click(closeOf('ffmpeg: invalid data'));

      expect(status()).toHaveTextContent(why);
      expect(screen.getByText('ffmpeg: invalid data')).toBeInTheDocument();
      expect(screen.getByRole('button', { name: 'Retry' })).toBeInTheDocument();
    });

    it('closing the notice leaves the start error and its ×', async () => {
      recorderStatus.mockResolvedValue(
        statusReply({ notice: saved, start_error: { code: 'permission_mic', message: MIC } }),
      );
      const user = setup();
      await screen.findByText('Saved, sent for processing');

      recorderStatus.mockResolvedValue(statusReply({ start_error: { code: 'permission_mic', message: MIC } }));
      await user.click(closeOf('Saved, sent for processing'));
      await act(async () => {});

      expect(screen.queryByText('Saved, sent for processing')).not.toBeInTheDocument();
      expect(screen.getByRole('alert')).toHaveTextContent(MIC);
      expect(closeOf(MIC)).toBeInTheDocument();
      expect(api.recorderDismissStartError).not.toHaveBeenCalled();
    });

    it('command error × hides it; a new failure with the same text shows it again', async () => {
      recorderStatus.mockResolvedValue(statusReply({ state: 'recording' }));
      stopFails('Nothing is recording');
      const user = setup();

      await user.click(await screen.findByRole('button', { name: 'Stop' }));
      await user.click(closeOf('Nothing is recording'));
      expect(status()).not.toHaveTextContent('Nothing is recording');

      await user.click(screen.getByRole('button', { name: 'Stop' }));
      expect(status()).toHaveTextContent('Nothing is recording');
      expect(api.recorderStop).toHaveBeenCalledTimes(2);
      expect(api.recorderDismissNotice).not.toHaveBeenCalled();
      expect(api.recorderDismissStartError).not.toHaveBeenCalled();
    });

    it('connection error × hides it through the outage; it shows again after a good poll then a failure', async () => {
      recorderStatus.mockResolvedValueOnce(statusReply()).mockRejectedValue(new Error('ipc closed'));
      const user = setup();
      const text = 'Could not reach the Meeting app: ipc closed';
      await act(() => vi.advanceTimersByTimeAsync(POLL_MS));
      expect(status()).toHaveTextContent(text);

      await user.click(closeOf(text));
      expect(status()).not.toHaveTextContent(text);
      await act(() => vi.advanceTimersByTimeAsync(POLL_MS * 2));
      expect(status()).not.toHaveTextContent(text);

      recorderStatus.mockResolvedValueOnce(statusReply()).mockRejectedValue(new Error('ipc closed'));
      await act(() => vi.advanceTimersByTimeAsync(POLL_MS));
      await act(() => vi.advanceTimersByTimeAsync(POLL_MS));
      expect(status()).toHaveTextContent(text);
      expect(api.recorderDismissNotice).not.toHaveBeenCalled();
      expect(api.recorderDismissStartError).not.toHaveBeenCalled();
    });

    it('keyboard: Tab reaches each ×, Enter and Space close; focus goes to the next × and then to Start', async () => {
      recorderStatus.mockResolvedValue(
        statusReply({ notice: saved, start_error: { code: 'permission_mic', message: MIC } }),
      );
      vi.mocked(api.recorderDismissStartError).mockResolvedValue({ ok: true });
      const user = setup();
      await screen.findByText('Saved, sent for processing');
      await waitFor(() => expect(start()).toBeEnabled());

      const first = closeOf(MIC);
      while (document.activeElement !== first) await user.tab();
      expect(first).toHaveAttribute('type', 'button');

      recorderStatus.mockResolvedValue(statusReply({ notice: saved }));
      await user.keyboard('{Enter}');
      await act(async () => {});
      expect(screen.queryByRole('alert')).not.toBeInTheDocument();
      expect(closeOf('Saved, sent for processing')).toHaveFocus();

      recorderStatus.mockResolvedValue(statusReply());
      await user.keyboard(' ');
      await act(async () => {});
      expect(screen.queryByText('Saved, sent for processing')).not.toBeInTheDocument();
      expect(start()).toHaveFocus();
      expect(document.body).not.toHaveFocus();
    });

    it('while recording, the last × hands focus to Pause', async () => {
      recorderStatus.mockResolvedValue(statusReply({ state: 'recording' }));
      stopFails('Nothing is recording');
      const user = setup();

      await user.click(await screen.findByRole('button', { name: 'Stop' }));
      await user.click(closeOf('Nothing is recording'));
      expect(screen.getByRole('button', { name: 'Pause' })).toHaveFocus();
    });

    it('a failed notice × keeps focus on it', async () => {
      vi.mocked(api.recorderDismissNotice).mockRejectedValue(new Error('ipc closed'));
      recorderStatus.mockResolvedValue(statusReply({ notice: saved }));
      const user = setup();
      await screen.findByText('Saved, sent for processing');

      const close = closeOf('Saved, sent for processing');
      await user.click(close);
      expect(close).toHaveFocus();
    });

    it('the × is at least 24 x 24 px', () => {
      const css = readFileSync(join(import.meta.dirname, 'RecordingBar.module.css'), 'utf8');
      const rule = css.slice(css.indexOf('.dismiss {'), css.indexOf('}', css.indexOf('.dismiss {')));
      expect(rule).toMatch(/min-width: 24px/);
      expect(rule).toMatch(/min-height: 24px/);
      expect(rule).not.toMatch(/outline: none/);
    });
  });
});
