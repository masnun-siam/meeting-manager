import { act, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { UpdateStatus } from '../api';
import type { Api } from '../api.mock';

const api = vi.hoisted(() => ({
  updateStatus: vi.fn(),
  updateCheck: vi.fn(),
  updateInstall: vi.fn(),
  setAutoUpdate: vi.fn(),
  recorderStatus: vi.fn(),
}));

vi.mock('../api', async () =>
  (await import('../api.mock')).mockApi({
    updateStatus: () => api.updateStatus(),
    updateCheck: () => api.updateCheck(),
    updateInstall: () => api.updateInstall(),
    setAutoUpdate: (req) => api.setAutoUpdate(req),
    recorderStatus: () => api.recorderStatus(),
  } satisfies Partial<Api>),
);

import { FINISH_RECORDING, UpdatesSection } from './UpdatesSection';

const base: UpdateStatus = {
  version: '0.1.0',
  auto_update: false,
  state: 'idle',
  progress: null,
  available: null,
  error: null,
  tool_error: null,
};
const status = (over: Partial<UpdateStatus> = {}) => ({ ok: true, ...base, ...over });
const found = { version: '0.2.0', notes: 'Faster saves.\nFixed the tray.' };
const recorder = (state: string) => ({
  ok: true,
  state,
  elapsed_s: 0,
  language: 'en',
  folder: null,
  notice: null,
});

beforeEach(() => {
  vi.useFakeTimers({ shouldAdvanceTime: true });
  api.updateStatus.mockReset().mockResolvedValue(status());
  api.updateCheck.mockReset().mockResolvedValue({ ok: true, started: true });
  api.updateInstall.mockReset().mockResolvedValue({ ok: true, started: true });
  api.setAutoUpdate
    .mockReset()
    .mockImplementation(async ({ enabled }: { enabled: boolean }) => ({ ok: true, auto_update: enabled }));
  api.recorderStatus.mockReset().mockResolvedValue(recorder('idle'));
});

afterEach(() => {
  vi.useRealTimers();
});

async function setup() {
  const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime });
  render(<UpdatesSection />);
  await screen.findByText('Version 0.1.0');
  return user;
}

const tick = (ms: number) => act(() => vi.advanceTimersByTimeAsync(ms));
const checkButton = () => screen.getByRole('button', { name: 'Check for updates' });
const installButton = () => screen.getByRole('button', { name: 'Install and relaunch' });
const autoBox = () => screen.getByRole('checkbox', { name: 'Install updates automatically' });
const statusLine = () => screen.getAllByRole('status')[1];
const alerts = () => screen.queryAllByRole('alert').map((a) => a.textContent);

describe('UpdatesSection', () => {
  it('shows the version and the setting when idle, with nothing to report', async () => {
    api.updateStatus.mockResolvedValue(status({ auto_update: true }));
    await setup();

    expect(screen.getByText('Version 0.1.0')).toBeInTheDocument();
    expect(autoBox()).toBeChecked();
    expect(checkButton()).toBeEnabled();
    expect(screen.queryByRole('button', { name: 'Install and relaunch' })).not.toBeInTheDocument();
    expect(statusLine()).toBeEmptyDOMElement();
    expect(alerts()).toEqual([]);
  });

  it('shows Checking… while the check runs, then Meeting is up to date, and stops polling', async () => {
    const user = await setup();
    api.updateStatus.mockResolvedValueOnce(status({ state: 'checking' })).mockResolvedValue(status());

    await user.click(checkButton());

    expect(api.updateCheck).toHaveBeenCalledTimes(1);
    expect(await screen.findByText('Checking…')).toHaveAttribute('role', 'status');
    expect(checkButton()).toBeDisabled();
    await tick(1000);

    expect(await screen.findByText('Meeting is up to date')).toHaveAttribute('role', 'status');
    expect(checkButton()).toBeEnabled();
    const calls = api.updateStatus.mock.calls.length;
    await tick(5000);
    expect(api.updateStatus).toHaveBeenCalledTimes(calls);
  });

  it('does not say up to date for a check it did not start', async () => {
    api.updateStatus.mockResolvedValueOnce(status({ state: 'checking' })).mockResolvedValue(status());
    await setup();
    expect(screen.getByText('Checking…')).toBeInTheDocument();

    await tick(1000);

    expect(statusLine()).toBeEmptyDOMElement();
  });

  it('shows the release found with its notes and Install and relaunch', async () => {
    const user = await setup();
    api.updateStatus.mockResolvedValue(status({ available: found }));

    await user.click(checkButton());

    expect(await screen.findByText('Version 0.2.0 is available')).toBeInTheDocument();
    expect(screen.getByText(/Faster saves\./)).toHaveTextContent('Faster saves. Fixed the tray.');
    expect(installButton()).toBeEnabled();
    expect(screen.queryByText('Meeting is up to date')).not.toBeInTheDocument();
  });

  it('shows release notes containing HTML as text', async () => {
    const notes = '<img src=x onerror=alert(1)>';
    api.updateStatus.mockResolvedValue(status({ available: { version: '0.2.0', notes } }));
    const { container } = render(<UpdatesSection />);

    expect(await screen.findByText(notes)).toBeInTheDocument();
    expect(container.querySelector('img')).toBeNull();
  });

  it('installs and shows Downloading… N% from progress, with both buttons disabled', async () => {
    api.updateStatus.mockResolvedValue(status({ available: found }));
    const user = await setup();
    api.updateStatus
      .mockResolvedValueOnce(status({ available: found, state: 'downloading', progress: 42 }))
      .mockResolvedValue(status({ available: found, state: 'installing' }));

    await user.click(installButton());

    expect(api.updateInstall).toHaveBeenCalledTimes(1);
    expect(await screen.findByText('Downloading… 42%')).toHaveAttribute('role', 'status');
    expect(installButton()).toBeDisabled();
    expect(checkButton()).toBeDisabled();
    await tick(1000);

    expect(statusLine()).toBeEmptyDOMElement();
    expect(installButton()).toBeDisabled();
    expect(checkButton()).toBeDisabled();
  });

  it.each(['recording', 'paused', 'saving'])(
    'disables Install with its reason while the recorder is %s',
    async (state) => {
      api.recorderStatus.mockResolvedValue(recorder(state));
      api.updateStatus.mockResolvedValue(status({ available: found }));
      await setup();

      expect(await screen.findByText(FINISH_RECORDING)).toBeInTheDocument();
      expect(installButton()).toBeDisabled();
    },
  );

  it('disables both buttons and keeps polling while a downloaded update waits for the recorder', async () => {
    api.updateStatus.mockResolvedValue(status({ available: found, state: 'ready' }));
    await setup();

    expect(installButton()).toBeDisabled();
    expect(checkButton()).toBeDisabled();
    expect(screen.queryByText(FINISH_RECORDING)).not.toBeInTheDocument();
    expect(statusLine()).toBeEmptyDOMElement();
    const calls = api.updateStatus.mock.calls.length;
    await tick(2000);
    expect(api.updateStatus.mock.calls.length).toBeGreaterThan(calls);
  });

  it('describes the disabled Install with the reason and clears a stale reply once the recording ends', async () => {
    api.updateStatus.mockResolvedValue(status({ available: found }));
    api.updateInstall.mockResolvedValue({ ok: false, code: 'recording_active', message: FINISH_RECORDING });
    const user = await setup();
    api.recorderStatus.mockResolvedValue(recorder('recording'));

    await user.click(installButton());
    await tick(1000);

    expect(installButton()).toHaveAccessibleDescription(FINISH_RECORDING);
    expect(alerts()).toEqual([FINISH_RECORDING]);
    api.recorderStatus.mockResolvedValue(recorder('idle'));
    await tick(1000);

    expect(installButton()).toBeEnabled();
    expect(alerts()).toEqual([]);
  });

  it('shows a no_update reply', async () => {
    const message = 'No update is available; check for updates first';
    api.updateStatus.mockResolvedValue(status({ available: found }));
    api.updateInstall.mockResolvedValue({ ok: false, code: 'no_update', message });
    const user = await setup();

    await user.click(installButton());

    expect(await screen.findByRole('alert')).toHaveTextContent(message);
  });

  it('shows a recording_active reply', async () => {
    api.updateStatus.mockResolvedValue(status({ available: found }));
    api.updateInstall.mockResolvedValue({ ok: false, code: 'recording_active', message: FINISH_RECORDING });
    const user = await setup();

    await user.click(installButton());

    expect(await screen.findByRole('alert')).toHaveTextContent(FINISH_RECORDING);
  });

  it('shows error and tool_error verbatim as alerts', async () => {
    api.updateStatus.mockResolvedValue(
      status({
        error: "Couldn't check for updates: offline",
        tool_error: "The meeting tool wasn't updated: disk full",
      }),
    );
    await setup();

    expect(alerts()).toEqual(["Couldn't check for updates: offline", "The meeting tool wasn't updated: disk full"]);
    expect(statusLine()).toBeEmptyDOMElement();
  });

  it('shows a failed check request', async () => {
    api.updateCheck.mockRejectedValue(new Error('ipc closed'));
    const user = await setup();

    await user.click(checkButton());

    expect(await screen.findByRole('alert')).toHaveTextContent('Could not reach the Meeting app: ipc closed');
    expect(checkButton()).toBeEnabled();
    expect(statusLine()).toBeEmptyDOMElement();
  });

  it('shows a failed status read', async () => {
    api.updateStatus.mockRejectedValue(new Error('ipc closed'));
    render(<UpdatesSection />);

    expect(await screen.findByRole('alert')).toHaveTextContent('Could not reach the Meeting app: ipc closed');
    expect(checkButton()).toBeDisabled();
  });

  it('saves the setting on and off', async () => {
    const user = await setup();

    await user.click(autoBox());
    expect(autoBox()).toBeChecked();
    await user.click(autoBox());
    await tick(0);

    expect(autoBox()).not.toBeChecked();
    expect(api.setAutoUpdate.mock.calls).toEqual([[{ enabled: true }], [{ enabled: false }]]);
  });

  it('returns the setting to the saved value with the reason when the store is unavailable', async () => {
    api.setAutoUpdate.mockResolvedValue({ ok: false, code: 'store_unavailable', message: 'database is locked' });
    const user = await setup();

    await user.click(autoBox());
    await tick(0);

    expect(autoBox()).not.toBeChecked();
    expect(screen.getByText('database is locked')).toHaveAttribute('role', 'status');
  });

  it('keeps the latest of two quick toggles when the first save fails late', async () => {
    let failFirst!: () => void;
    api.setAutoUpdate
      .mockImplementationOnce(
        () =>
          new Promise((resolve) => {
            failFirst = () => resolve({ ok: false, code: 'store_unavailable', message: 'database is locked' });
          }),
      )
      .mockImplementationOnce(async () => ({ ok: true, auto_update: false }));
    const user = await setup();

    await user.click(autoBox());
    await user.click(autoBox());
    expect(autoBox()).not.toBeChecked();
    failFirst();
    await tick(0);

    expect(api.setAutoUpdate.mock.calls).toEqual([[{ enabled: true }], [{ enabled: false }]]);
    expect(autoBox()).not.toBeChecked();
    expect(screen.queryByText('database is locked')).not.toBeInTheDocument();
  });

  it('returns the setting when the save is rejected', async () => {
    api.setAutoUpdate.mockRejectedValue(new Error('ipc closed'));
    const user = await setup();

    await user.click(autoBox());
    await tick(0);

    expect(autoBox()).not.toBeChecked();
    expect(screen.getByText('Could not reach the Meeting app: ipc closed')).toHaveAttribute('role', 'status');
  });
});
