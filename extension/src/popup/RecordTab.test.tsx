import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { act, cleanup, render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { MeetingContextState, Notice, RecorderStatus } from '../shared/messages';
import { initialRecordingState, type RecordingState } from '../shared/recordingState';
import { formatElapsed, RecordTab } from './RecordTab';

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

function readCss(): string {
  return readFileSync(join(import.meta.dirname, 'RecordTab.module.css'), 'utf8');
}
const css = readCss;

function setup() {
  const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime });
  render(<RecordTab />);
  return user;
}

describe('RecordTab', () => {
  it('starts idle on Bengali when nothing is stored', async () => {
    setup();

    expect(await screen.findByRole('radio', { name: 'বাংলা' })).toBeChecked();
    expect(screen.getByRole('radiogroup', { name: 'Language' })).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Start recording' })).toBeEnabled();
  });

  it('shows a dimmed, decorative 0:00:00 when idle', async () => {
    setup();

    await screen.findByRole('button', { name: 'Start recording' });
    const idle = screen.getByText('0:00:00');
    expect(idle).toHaveAttribute('aria-hidden', 'true');
    expect(screen.queryByRole('timer')).not.toBeInTheDocument();
    expect(screen.getByRole('status')).not.toContainElement(idle);
  });

  it('labels the Bengali choice as Bengali text', async () => {
    setup();

    expect((await screen.findByRole('radio', { name: 'বাংলা' })).closest('label')).toHaveAttribute('lang', 'bn');
  });

  it('moves between the language choices with the arrow keys', async () => {
    const user = setup();

    await user.click(await screen.findByRole('radio', { name: 'বাংলা' }));
    await user.keyboard('{ArrowRight}');

    expect(screen.getByRole('radio', { name: 'English' })).toBeChecked();
    expect(screen.getByRole('radio', { name: 'English' })).toHaveFocus();
  });

  it('styles a problem differently from information', async () => {
    stored = recording({ micIncluded: false, notice: { kind: 'error', message: 'Disk full' } });
    setup();

    const problem = await screen.findByText('Disk full');
    const info = screen.getByText('Microphone not included: recording tab audio only.');
    expect(problem.className).not.toBe(info.className);
  });

  it('shows the stored language', async () => {
    local = { language: 'en' };
    setup();

    await waitFor(() => expect(screen.getByRole('radio', { name: 'English' })).toBeChecked());
  });

  it('ignores a stored language it does not know', async () => {
    local = { language: 'fr' };
    setup();

    await waitFor(() => expect(chrome.storage.local.get).toHaveBeenCalled());
    expect(await screen.findByRole('radio', { name: 'বাংলা' })).toBeChecked();
  });

  it('sends start with the chosen language and stores it', async () => {
    const user = setup();

    await user.click(await screen.findByRole('radio', { name: 'English' }));
    await user.click(screen.getByRole('button', { name: 'Start recording' }));

    expect(localSet).toHaveBeenCalledWith({ language: 'en' });
    expect(sendMessage).toHaveBeenCalledWith({ type: 'start', language: 'en' });
  });

  it('shows the running timer, ticking each second, and the recording language', async () => {
    stored = recording({ language: 'en' });
    const user = setup();

    expect(await screen.findByRole('timer', { name: 'Recorded time' })).toHaveTextContent(/^0:04:00$/);
    expect(screen.getByText('REC')).toBeInTheDocument();
    expect(screen.getByRole('radio', { name: 'English' })).toBeDisabled();
    expect(screen.getByRole('radio', { name: 'বাংলা' })).toBeDisabled();
    expect(screen.getByRole('radio', { name: 'English' })).toBeChecked();
    await act(async () => {
      vi.advanceTimersByTime(1000);
    });
    expect(screen.getByRole('timer')).toHaveTextContent(/^0:04:01$/);

    await user.click(screen.getByRole('button', { name: 'Pause' }));
    expect(sendMessage).toHaveBeenCalledWith({ type: 'pause' });
    await user.click(screen.getByRole('button', { name: 'Stop & save' }));
    expect(sendMessage).toHaveBeenCalledWith({ type: 'stop' });
  });

  it('shows the frozen time and Resume when paused', async () => {
    stored = status({ phase: 'paused', startedAt: T0 - 300_000, recordedMs: 61_000 });
    const user = setup();

    expect(await screen.findByRole('timer')).toHaveTextContent(/^0:01:01$/);
    expect(screen.getByText('Paused')).toBeInTheDocument();
    expect(screen.queryByText('REC')).not.toBeInTheDocument();
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

    await screen.findByRole('button', { name: 'Start recording' });
    expect(screen.getByRole('status')).toBeEmptyDOMElement();
  });

  it('re-renders when the status in session storage changes', async () => {
    setup();
    await screen.findByRole('button', { name: 'Start recording' });

    act(() => listener({ recorder: { newValue: recording() } }));

    expect(await screen.findByRole('timer')).toBeInTheDocument();
    expect(screen.getByText('REC')).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'Start recording' })).not.toBeInTheDocument();
  });

  it('shows why it could not reach the recorder', async () => {
    sendMessage.mockRejectedValue(new Error('Receiving end does not exist.'));
    const user = setup();

    await user.click(await screen.findByRole('button', { name: 'Start recording' }));

    expect(await screen.findByText('Could not reach the recorder: Receiving end does not exist.')).toBeInTheDocument();
  });

  describe('dismissing messages', () => {
    const SAVED = 'Saved, sent for processing';
    const TAB_CLOSED = 'Stopped because the recorded tab was closed.';
    const MIC = 'Microphone not included: recording tab audio only.';
    const UNREACHABLE = 'Could not reach the recorder: Receiving end does not exist.';
    const stoppedByTab = (extra: Partial<RecorderStatus> = {}) =>
      status({ phase: 'idle', lastStop: 'tab_closed' }, { notice: { kind: 'saved' }, ...extra });
    const dismissButtons = () => screen.queryAllByRole('button', { name: 'Dismiss message' });

    /** What the service worker does after a `dismiss`: writes the status, which fires `onChanged`. */
    function write(next: RecorderStatus) {
      stored = next;
      act(() => listener({ recorder: { newValue: next } }));
    }

    function failOnce() {
      sendMessage.mockRejectedValueOnce(new Error('Receiving end does not exist.'));
    }

    it('puts a 24 px × named "Dismiss message" on each line', async () => {
      stored = stoppedByTab();
      failOnce();
      const user = setup();

      await user.click(await screen.findByRole('button', { name: 'Start recording' }));
      await screen.findByText(UNREACHABLE);

      const buttons = dismissButtons();
      expect(buttons).toHaveLength(3);
      for (const button of buttons) {
        expect(button).toHaveAttribute('type', 'button');
        expect(button.className).toBe(buttons[0].className);
      }
      expect(css()).toMatch(/\.dismiss\s*\{[^}]*width: 24px;[^}]*height: 24px;/);
    });

    it('puts a × on the mic warning', async () => {
      stored = recording({ micIncluded: false });
      setup();

      await screen.findByText(MIC);
      expect(dismissButtons()).toHaveLength(1);
    });

    it.each<[string, RecorderStatus, string, 'notice' | 'stop' | 'mic']>([
      ['the notice', status(initialRecordingState, { notice: { kind: 'saved' } }), SAVED, 'notice'],
      ['the stop reason', status({ phase: 'idle', lastStop: 'tab_closed' }), TAB_CLOSED, 'stop'],
      ['the mic warning', recording({ micIncluded: false }), MIC, 'mic'],
    ])('sends dismiss for %s', async (_, value, text, what) => {
      stored = value;
      const user = setup();

      await screen.findByText(text);
      await user.click(screen.getByRole('button', { name: 'Dismiss message' }));

      expect(sendMessage).toHaveBeenCalledWith({ type: 'dismiss', what });
      expect(screen.getByText(text)).toBeInTheDocument(); // until the status changes
    });

    it('drops closed lines when the status changes and does not show them on reopening', async () => {
      stored = stoppedByTab();
      const user = setup();

      await screen.findByText(SAVED);
      await user.click(dismissButtons()[0]);
      write(stoppedByTab({ notice: null }));
      await waitFor(() => expect(screen.queryByText(SAVED)).not.toBeInTheDocument());
      await user.click(dismissButtons()[0]);
      write(status(initialRecordingState));
      await waitFor(() => expect(screen.getByRole('status')).toBeEmptyDOMElement());

      cleanup();
      setup();
      await screen.findByRole('button', { name: 'Start recording' });
      expect(screen.getByRole('status')).toBeEmptyDOMElement();
    });

    it('does not show a dismissed mic warning', async () => {
      stored = recording({ micIncluded: false, micWarningDismissed: true });
      setup();

      await screen.findByRole('timer');
      expect(screen.getByRole('status')).toBeEmptyDOMElement();
    });

    it('hides a send error without sending anything, and shows the next one', async () => {
      failOnce();
      const user = setup();
      const start = await screen.findByRole('button', { name: 'Start recording' });

      await user.click(start);
      await user.click(await screen.findByRole('button', { name: 'Dismiss message' }));

      expect(screen.queryByText(UNREACHABLE)).not.toBeInTheDocument();
      expect(sendMessage).toHaveBeenCalledTimes(1);

      failOnce();
      await user.click(start);
      expect(await screen.findByText(UNREACHABLE)).toBeInTheDocument();
    });

    it('keeps a send error while another line is dismissed', async () => {
      stored = status(initialRecordingState, { notice: { kind: 'saved' } });
      failOnce();
      const user = setup();

      await user.click(await screen.findByRole('button', { name: 'Start recording' }));
      await screen.findByText(UNREACHABLE);
      await user.click(dismissButtons()[1]);

      expect(sendMessage).toHaveBeenLastCalledWith({ type: 'dismiss', what: 'notice' });
      expect(screen.getByText(UNREACHABLE)).toBeInTheDocument();
    });

    it('shows the reach error and keeps the line when the dismiss cannot be sent', async () => {
      stored = status(initialRecordingState, { notice: { kind: 'saved' } });
      failOnce();
      const user = setup();

      await screen.findByText(SAVED);
      await user.click(screen.getByRole('button', { name: 'Dismiss message' }));

      expect(await screen.findByText(UNREACHABLE)).toBeInTheDocument();
      expect(screen.getByText(SAVED)).toBeInTheDocument();
      expect(dismissButtons()).toHaveLength(2);
    });

    it('moves focus to the next ×, then to Start recording', async () => {
      stored = stoppedByTab();
      const user = setup();

      await screen.findByText(SAVED);
      await user.click(dismissButtons()[0]);
      write(stoppedByTab({ notice: null }));
      await waitFor(() => expect(dismissButtons()).toHaveLength(1));
      expect(dismissButtons()[0]).toHaveFocus();

      await user.click(dismissButtons()[0]);
      write(status(initialRecordingState));
      await waitFor(() => expect(screen.getByRole('button', { name: 'Start recording' })).toHaveFocus());
    });

    it('moves focus to the last × when the last of several lines closes', async () => {
      stored = stoppedByTab();
      const user = setup();

      await screen.findByText(TAB_CLOSED);
      await user.click(dismissButtons()[1]);
      write(stoppedByTab({ state: { phase: 'idle', lastStop: null } }));

      await waitFor(() => expect(dismissButtons()).toHaveLength(1));
      expect(dismissButtons()[0]).toHaveFocus();
    });

    it('moves focus to Pause while recording', async () => {
      stored = recording({ micIncluded: false });
      const user = setup();

      await user.click(await screen.findByRole('button', { name: 'Dismiss message' }));
      write(recording({ micIncluded: false, micWarningDismissed: true }));

      await waitFor(() => expect(screen.getByRole('button', { name: 'Pause' })).toHaveFocus());
    });

    it('moves focus to the Record tab while saving', async () => {
      const saving = (extra: Partial<RecorderStatus> = {}) =>
        status({ phase: 'saving', startedAt: T0 - 60_000, recordedMs: 60_000, reason: 'user' }, extra);
      stored = saving({ notice: { kind: 'error', message: 'Disk full' } });
      const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime });
      render(
        <>
          <button type="button" id="tab-record">
            Record
          </button>
          <RecordTab />
        </>,
      );

      await user.click(await screen.findByRole('button', { name: 'Dismiss message' }));
      write(saving());

      await waitFor(() => expect(screen.getByRole('button', { name: 'Record' })).toHaveFocus());
    });

    it.each(['{Enter}', ' '])('closes with the keyboard: %s', async (key) => {
      stored = status(initialRecordingState, { notice: { kind: 'saved' } });
      const user = setup();

      await screen.findByText(SAVED);
      await user.tab();
      expect(screen.getByRole('button', { name: 'Start recording' })).toHaveFocus();
      await user.tab(); // the language choices, one stop
      await user.tab();
      expect(screen.getByRole('button', { name: 'Dismiss message' })).toHaveFocus();
      await user.keyboard(key);

      expect(sendMessage).toHaveBeenCalledWith({ type: 'dismiss', what: 'notice' });
    });
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
        'captured with a title and nobody seen',
        { capture: 'captured', meetingTitle: 'Weekly sync' },
        'Weekly sync · 0 seen · open the people panel to capture names',
        'Meet details captured · open the people panel to capture names',
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
        "Meet details unavailable — names won't be captured. Recording is fine. Open the people panel to capture names.",
        "Meet details unavailable — names won't be captured. Recording is fine. Open the people panel to capture names.",
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

    it('shows the panel hint with no title and nobody seen', async () => {
      stored = recording();
      storedContext = context({ capture: 'captured' });
      storedContext.identity.tab_title = null;
      setup();

      await waitFor(() => expect(visibleLine()).toBe('0 seen · open the people panel to capture names'));
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

      act(() => listener({ meeting_context: { newValue: context({ capture: 'captured' }) } }));
      await waitFor(() =>
        expect(visibleLine()).toBe('Meet - abc-defg-hij · 0 seen · open the people panel to capture names'),
      );
      expect(screen.getByRole('status').querySelector('span')?.textContent).toBe(
        'Meet details captured · open the people panel to capture names',
      );

      act(() => listener({ meeting_context: { newValue: context({ capture: 'captured', participants: people(1) }) } }));
      await waitFor(() => expect(visibleLine()).toBe('Meet - abc-defg-hij · 1 seen'));
      const span = screen.getByRole('status').querySelector('span');
      expect(span?.textContent).toBe('Meet details captured');

      act(() => listener({ meeting_context: { newValue: context({ capture: 'captured', participants: people(5) }) } }));
      await waitFor(() => expect(visibleLine()).toBe('Meet - abc-defg-hij · 5 seen'));
      expect(screen.getByRole('status').querySelector('span')).toBe(span);
      expect(announced()).toBe('Meet details captured');

      act(() => listener({ meeting_context: { newValue: context({ capture: 'unavailable' }) } }));
      expect(announced()).toBe(
        "Meet details unavailable — names won't be captured. Recording is fine. Open the people panel to capture names.",
      );
    });

    it('has no × on the context line', async () => {
      stored = recording();
      storedContext = context({ capture: 'captured', participants: people(2) });
      setup();

      await waitFor(() => expect(visibleLine()).toBe('Meet - abc-defg-hij · 2 seen'));
      expect(screen.queryByRole('button', { name: 'Dismiss message' })).not.toBeInTheDocument();
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
      await screen.findByRole('radiogroup', { name: 'Language' });
      expect(screen.getByRole('status')).toBeEmptyDOMElement();
    });

    it('clears the line when the recording leaves recording and paused', async () => {
      stored = recording();
      storedContext = context();
      setup();
      await waitFor(() => expect(visibleLine()).toBe('Meet: reading participants…'));

      act(() => listener({ recorder: { newValue: status(initialRecordingState) } }));

      await screen.findByRole('button', { name: 'Start recording' });
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

describe('formatElapsed', () => {
  it.each([
    [0, '0:00:00'],
    [999, '0:00:00'],
    [61_000, '0:01:01'],
    [3_599_000, '0:59:59'],
    [3 * 3_600_000, '3:00:00'],
  ])('formats %i ms as %s', (ms, text) => {
    expect(formatElapsed(ms)).toBe(text);
  });
});

describe('RecordTab.module.css', () => {
  const css = readCss();

  it('stops the REC pulse under reduced motion', () => {
    expect(css).toMatch(/\.rec::before\s*\{[^}]*animation: pulse/);
    expect(css).toMatch(/@media \(prefers-reduced-motion: reduce\)\s*\{\s*\.rec::before\s*\{\s*animation: none;/);
  });

  it('uses no colour literals', () => {
    expect(css).not.toMatch(/#[0-9a-f]{3,8}\b|rgba?\(|hsla?\(/i);
  });
});
