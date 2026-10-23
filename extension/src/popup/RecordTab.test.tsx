import { act, cleanup, render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { MeetingContextState, Notice, RecorderStatus } from '../shared/messages';
import { initialRecordingState, type RecordingState } from '../shared/recordingState';
import { RecordTab } from './RecordTab';

const flag = vi.hoisted(() => ({ on: false }));
vi.mock('../shared/flags', () => ({ meetingContextEnabled: () => flag.on }));

const T0 = new Date('2026-01-01T10:00:00Z').getTime();

function status(state: RecordingState, extra: Partial<RecorderStatus> = {}): RecorderStatus {
  return { state, language: 'bn', micIncluded: null, notice: null, tabId: null, ...extra };
}

const recording = (extra: Partial<RecorderStatus> = {}) =>
  status({ phase: 'recording', startedAt: T0 - 240_000, recordedMs: 0, resumedAt: T0 - 240_000 }, extra);

let stored: RecorderStatus | null;
let storedContext: MeetingContextState | null;
let local: Record<string, unknown>;
let listeners: ((changes: Record<string, { newValue?: unknown }>) => void)[];
const sessionGet = vi.fn();

const listener = (changes: Record<string, { newValue?: unknown }>) => {
  for (const fn of listeners) fn(changes);
};
const sendMessage = vi.fn();
const localSet = vi.fn();

beforeEach(() => {
  vi.useFakeTimers({ shouldAdvanceTime: true });
  vi.setSystemTime(T0);
  flag.on = false;
  stored = null;
  storedContext = null;
  local = {};
  listeners = [];
  sessionGet.mockReset().mockImplementation(async (key: string) => {
    const value = { recorder: stored, meeting_context: storedContext }[key];
    return value ? { [key]: value } : {};
  });
  sendMessage.mockReset().mockResolvedValue(undefined);
  localSet.mockReset().mockResolvedValue(undefined);
  vi.stubGlobal('chrome', {
    storage: {
      session: {
        get: sessionGet,
        onChanged: {
          addListener: vi.fn((fn) => {
            listeners.push(fn);
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

    act(() => listener({ recorder: { newValue: recording() } }));

    expect(await screen.findByRole('timer')).toHaveTextContent('● REC');
    expect(screen.queryByRole('button', { name: 'Start' })).not.toBeInTheDocument();
  });

  it('shows why it could not reach the recorder', async () => {
    sendMessage.mockRejectedValue(new Error('Receiving end does not exist.'));
    const user = setup();

    await user.click(await screen.findByRole('button', { name: 'Start' }));

    expect(await screen.findByText('Could not reach the recorder: Receiving end does not exist.')).toBeInTheDocument();
  });

  describe('context line', () => {
    const paused = () => status({ phase: 'paused', startedAt: T0 - 300_000, recordedMs: 61_000 });

    function context(
      extra: Partial<MeetingContextState> = {},
      platform: 'meet' | 'zoom' = 'meet',
    ): MeetingContextState {
      return {
        identity: {
          platform,
          url: null,
          meeting_id: null,
          tab_title: 'Meet - abc-defg-hij',
          started_at_ms: T0,
          utc_offset_min: 360,
          time_zone: null,
        },
        segments: [],
        meetingTitle: null,
        participants: [],
        speaking: [],
        openSpeaking: null,
        truncated: [],
        capture: 'reading',
        reloadHint: false,
        ...extra,
      };
    }

    const people = (n: number) => Array.from({ length: n }, (_, i) => ({ name: `P${i}`, self: false }));

    function visibleLine() {
      return screen.getByRole('status').querySelector('[aria-hidden="true"]')?.textContent;
    }

    function announced() {
      return screen.getByRole('status').querySelector('span')?.textContent;
    }

    beforeEach(() => {
      flag.on = true;
    });

    it.each<[string, Partial<MeetingContextState>, string, string]>([
      ['reading', {}, 'Meet: reading participants…', 'Meet: reading participants…'],
      [
        'captured with a meeting title',
        { capture: 'captured', meetingTitle: 'Weekly sync', participants: people(3) },
        'Weekly sync · 3 seen',
        'Meet details captured',
      ],
      [
        'captured with only a tab title',
        { capture: 'captured', participants: people(2) },
        'Meet - abc-defg-hij · 2 seen',
        'Meet details captured',
      ],
      [
        'unavailable',
        { capture: 'unavailable' },
        "Meet details unavailable — names won't be captured. Recording is fine.",
        "Meet details unavailable — names won't be captured. Recording is fine.",
      ],
      [
        'unavailable with the reload hint',
        { capture: 'unavailable', reloadHint: true },
        "Meet details unavailable — names won't be captured. Recording is fine. Reload the Meet tab next time.",
        "Meet details unavailable — names won't be captured. Recording is fine. Reload the Meet tab next time.",
      ],
      [
        'captured after the reload hint was set',
        { capture: 'captured', reloadHint: true, participants: people(1) },
        'Meet - abc-defg-hij · 1 seen',
        'Meet details captured',
      ],
    ])('shows %s', async (_, extra, text, announce) => {
      stored = recording();
      storedContext = context(extra);
      setup();

      await waitFor(() => expect(visibleLine()).toBe(text));
      expect(announced()).toBe(announce);
    });

    it('shows just the count with no title', async () => {
      stored = recording();
      storedContext = context({ capture: 'captured', participants: people(4) });
      storedContext.identity.tab_title = null;
      setup();

      await waitFor(() => expect(visibleLine()).toBe('4 seen'));
    });

    it('shows the other-sites line, without a prefix when the tab title is null', async () => {
      stored = recording();
      storedContext = context({}, 'zoom');
      storedContext.identity.tab_title = 'Zoom Meeting';
      setup();

      await waitFor(() => expect(visibleLine()).toBe('Zoom Meeting · names are captured on Google Meet only'));
      expect(announced()).toBe('Zoom Meeting · names are captured on Google Meet only');

      const untitled = context({}, 'zoom');
      untitled.identity.tab_title = null;
      act(() => listener({ meeting_context: { newValue: untitled } }));
      await waitFor(() => expect(visibleLine()).toBe('names are captured on Google Meet only'));
    });

    it('announces only state changes, not a rising count', async () => {
      stored = recording();
      storedContext = context();
      setup();
      await waitFor(() => expect(visibleLine()).toBe('Meet: reading participants…'));

      act(() => listener({ meeting_context: { newValue: context({ capture: 'captured', participants: people(1) }) } }));
      await waitFor(() => expect(visibleLine()).toBe('Meet - abc-defg-hij · 1 seen'));
      const span = screen.getByRole('status').querySelector('span');
      expect(span?.textContent).toBe('Meet details captured');

      act(() => listener({ meeting_context: { newValue: context({ capture: 'captured', participants: people(5) }) } }));
      await waitFor(() => expect(visibleLine()).toBe('Meet - abc-defg-hij · 5 seen'));
      expect(screen.getByRole('status').querySelector('span')).toBe(span);
      expect(announced()).toBe('Meet details captured');

      act(() => listener({ meeting_context: { newValue: context({ capture: 'unavailable' }) } }));
      expect(announced()).toBe("Meet details unavailable — names won't be captured. Recording is fine.");
    });

    it('keeps the line while paused', async () => {
      stored = paused();
      storedContext = context({ capture: 'captured', participants: people(2) });
      setup();

      await waitFor(() => expect(visibleLine()).toBe('Meet - abc-defg-hij · 2 seen'));
    });

    it.each([
      ['idle', status(initialRecordingState)],
      ['saving', status({ phase: 'saving', startedAt: T0 - 60_000, recordedMs: 60_000, reason: 'user' })],
    ])('shows no line when %s', async (_, value) => {
      stored = value;
      storedContext = context({ capture: 'captured', participants: people(2) });
      setup();

      await waitFor(() => expect(sessionGet).toHaveBeenCalledWith('meeting_context'));
      await screen.findByRole('combobox', { name: 'Language' });
      expect(screen.getByRole('status')).toBeEmptyDOMElement();
    });

    it('clears the line when the recording leaves recording and paused', async () => {
      stored = recording();
      storedContext = context();
      setup();
      await waitFor(() => expect(visibleLine()).toBe('Meet: reading participants…'));

      act(() => listener({ recorder: { newValue: status(initialRecordingState) } }));

      await screen.findByRole('button', { name: 'Start' });
      expect(visibleLine()).toBeUndefined();
    });

    it('ignores a stored context it does not understand', async () => {
      stored = recording();
      storedContext = { ...context(), capture: 'lost' as MeetingContextState['capture'] };
      setup();

      await waitFor(() => expect(sessionGet).toHaveBeenCalledWith('meeting_context'));
      await screen.findByRole('timer');
      expect(screen.getByRole('status')).toBeEmptyDOMElement();
    });

    it('neither reads the context nor shows a line with the flag off', async () => {
      flag.on = false;
      stored = recording();
      storedContext = context({ capture: 'captured', participants: people(2) });
      setup();

      await screen.findByRole('timer');
      expect(sessionGet).not.toHaveBeenCalledWith('meeting_context');
      expect(screen.getByRole('status')).toBeEmptyDOMElement();
    });
  });
});
