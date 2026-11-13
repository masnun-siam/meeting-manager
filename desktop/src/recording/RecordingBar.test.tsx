import { act, render, screen, waitFor } from '@testing-library/react';
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

    await user.click(screen.getByRole('button', { name: 'Resume' }));
    expect(api.recorderResume).toHaveBeenCalledOnce();
    expect(screen.getByText('● REC')).toBeInTheDocument();
  });

  it('Pause and Stop follow the returned state without waiting for a poll', async () => {
    recorderStatus.mockResolvedValue(statusReply({ state: 'recording', elapsed_s: 10 }));
    vi.mocked(api.recorderPause).mockResolvedValue({ ok: true, state: 'paused' });
    vi.mocked(api.recorderStop).mockResolvedValue({ ok: true, state: 'saving' });
    const user = setup();

    await user.click(await screen.findByRole('button', { name: 'Pause' }));
    expect(api.recorderPause).toHaveBeenCalledOnce();
    expect(screen.getByRole('button', { name: 'Resume' })).toBeInTheDocument();

    await user.click(screen.getByRole('button', { name: 'Stop' }));
    expect(api.recorderStop).toHaveBeenCalledOnce();
    expect(status()).toHaveTextContent('Saving the recording…');
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
    await user.click(screen.getByRole('button', { name: 'Retry' }));
    expect(api.recorderRetry).toHaveBeenCalledWith({ folder: '/r/meeting-1' });
    expect(screen.getByRole('button', { name: 'Retry' })).toBeDisabled();

    await act(async () => retry.resolve({ ok: true, state: 'saving' }));
    expect(status()).toHaveTextContent('Saving the recording…');
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
});
