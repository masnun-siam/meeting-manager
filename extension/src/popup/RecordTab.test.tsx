import { act, cleanup, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { Notice, RecorderStatus } from '../shared/messages';
import { initialRecordingState, type RecordingState } from '../shared/recordingState';
import { RecordTab } from './RecordTab';

const T0 = new Date('2026-01-01T10:00:00Z').getTime();

function status(state: RecordingState, extra: Partial<RecorderStatus> = {}): RecorderStatus {
  return { state, language: 'bn', micIncluded: null, notice: null, ...extra };
}

const recording = (extra: Partial<RecorderStatus> = {}) =>
  status({ phase: 'recording', startedAt: T0 - 240_000, recordedMs: 0, resumedAt: T0 - 240_000 }, extra);

let stored: RecorderStatus | null;
let local: Record<string, unknown>;
let listener: ((changes: Record<string, { newValue?: unknown }>) => void) | undefined;
const sendMessage = vi.fn();
const localSet = vi.fn();

beforeEach(() => {
  vi.useFakeTimers({ shouldAdvanceTime: true });
  vi.setSystemTime(T0);
  stored = null;
  local = {};
  listener = undefined;
  sendMessage.mockReset().mockResolvedValue(undefined);
  localSet.mockReset().mockResolvedValue(undefined);
  vi.stubGlobal('chrome', {
    storage: {
      session: {
        get: vi.fn(async () => (stored ? { recorder: stored } : {})),
        onChanged: {
          addListener: vi.fn((fn) => {
            listener = fn;
          }),
          removeListener: vi.fn(),
        },
      },
      local: { get: vi.fn(async () => local), set: localSet },
    },
    runtime: { sendMessage },
  });
});

afterEach(() => {
  cleanup(); // unmount while `chrome` is still stubbed: the hook's cleanup uses it
  vi.useRealTimers();
  vi.unstubAllGlobals();
});

function setup() {
  const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime });
  render(<RecordTab />);
  return user;
}

describe('RecordTab', () => {
  it('starts idle on Bengali when nothing is stored', async () => {
    setup();

    expect(await screen.findByRole('combobox', { name: 'Language' })).toHaveValue('bn');
    expect(screen.getByRole('button', { name: 'Start' })).toBeEnabled();
  });

  it('shows the stored language', async () => {
    local = { language: 'en' };
    setup();

    expect(await screen.findByDisplayValue('English')).toBeInTheDocument();
  });

  it('ignores a stored language it does not know', async () => {
    local = { language: 'fr' };
    setup();

    expect(await screen.findByRole('combobox', { name: 'Language' })).toHaveValue('bn');
  });

  it('sends start with the chosen language and stores it', async () => {
    const user = setup();

    await user.selectOptions(await screen.findByRole('combobox', { name: 'Language' }), 'English');
    await user.click(screen.getByRole('button', { name: 'Start' }));

    expect(localSet).toHaveBeenCalledWith({ language: 'en' });
    expect(sendMessage).toHaveBeenCalledWith({ type: 'start', language: 'en' });
  });

  it('shows the running timer, ticking each second, and the recording language', async () => {
    stored = recording({ language: 'en' });
    const user = setup();

    expect(await screen.findByRole('timer', { name: 'Recorded time' })).toHaveTextContent('● REC 00:04:00');
    expect(screen.getByRole('combobox', { name: 'Language' })).toBeDisabled();
    expect(screen.getByRole('combobox', { name: 'Language' })).toHaveValue('en');
    await act(async () => {
      vi.advanceTimersByTime(1000);
    });
    expect(screen.getByRole('timer')).toHaveTextContent('00:04:01');

    await user.click(screen.getByRole('button', { name: 'Pause' }));
    expect(sendMessage).toHaveBeenCalledWith({ type: 'pause' });
    await user.click(screen.getByRole('button', { name: 'Stop & save' }));
    expect(sendMessage).toHaveBeenCalledWith({ type: 'stop' });
  });

  it('shows the frozen time and Resume when paused', async () => {
    stored = status({ phase: 'paused', startedAt: T0 - 300_000, recordedMs: 61_000 });
    const user = setup();

    expect(await screen.findByRole('timer')).toHaveTextContent('II Paused 00:01:01');
    await user.click(screen.getByRole('button', { name: 'Resume' }));
    expect(sendMessage).toHaveBeenCalledWith({ type: 'resume' });
    expect(screen.getByRole('button', { name: 'Stop & save' })).toBeInTheDocument();
  });

  it('shows Saving… with no buttons', async () => {
    stored = status({ phase: 'saving', startedAt: T0 - 60_000, recordedMs: 60_000, reason: 'user' });
    setup();

    expect(await screen.findByText('Saving…')).toBeInTheDocument();
    expect(screen.queryByRole('button')).not.toBeInTheDocument();
  });

  it.each<[Notice, string]>([
    [{ kind: 'saved' }, 'Saved, sent for processing'],
    [{ kind: 'too_short' }, 'Under 10 seconds, so it was saved but not processed.'],
    [{ kind: 'setup', message: 'Run `meeting install`' }, 'Run `meeting install`'],
    [{ kind: 'error', message: 'Disk full' }, 'Disk full'],
  ])('shows the %j notice', async (notice, text) => {
    stored = status(initialRecordingState, { notice });
    setup();

    expect(await screen.findByText(text)).toBeInTheDocument();
    expect(screen.getByRole('status')).toHaveTextContent(text);
  });

  it('warns when the microphone is not included', async () => {
    stored = recording({ micIncluded: false });
    setup();

    expect(await screen.findByText('Microphone not included: recording tab audio only.')).toBeInTheDocument();
  });

  it('shows no mic warning while micIncluded is null', async () => {
    stored = recording();
    setup();

    await screen.findByRole('timer');
    expect(screen.getByRole('status')).toBeEmptyDOMElement();
  });

  it.each([
    ['cap', 'Stopped at the 3-hour limit.'],
    ['tab_closed', 'Stopped because the recorded tab was closed.'],
    ['capture_ended', 'Stopped because tab capture ended.'],
  ] as const)('explains a %s stop', async (lastStop, text) => {
    stored = status({ phase: 'idle', lastStop });
    setup();

    expect(await screen.findByText(text)).toBeInTheDocument();
  });

  it('shows nothing extra after a user stop', async () => {
    stored = status({ phase: 'idle', lastStop: 'user' });
    setup();

    await screen.findByRole('button', { name: 'Start' });
    expect(screen.getByRole('status')).toBeEmptyDOMElement();
  });

  it('re-renders when the status in session storage changes', async () => {
    setup();
    await screen.findByRole('button', { name: 'Start' });

    act(() => listener?.({ recorder: { newValue: recording() } }));

    expect(await screen.findByRole('timer')).toHaveTextContent('● REC');
    expect(screen.queryByRole('button', { name: 'Start' })).not.toBeInTheDocument();
  });

  it('shows why it could not reach the recorder', async () => {
    sendMessage.mockRejectedValue(new Error('Receiving end does not exist.'));
    const user = setup();

    await user.click(await screen.findByRole('button', { name: 'Start' }));

    expect(await screen.findByText('Could not reach the recorder: Receiving end does not exist.')).toBeInTheDocument();
  });
});
