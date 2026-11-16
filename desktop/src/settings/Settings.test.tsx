import { act, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { Api } from '../api.mock';

const api = vi.hoisted(() => ({
  settingsInfo: vi.fn(),
  rescan: vi.fn(),
  rescanStatus: vi.fn(),
  health: vi.fn(),
  calendarStatus: vi.fn(),
  calendars: vi.fn(),
}));

vi.mock('../api', async () =>
  (await import('../api.mock')).mockApi({
    settingsInfo: () => api.settingsInfo(),
    rescan: () => api.rescan(),
    rescanStatus: () => api.rescanStatus(),
    health: () => api.health(),
    calendarStatus: () => api.calendarStatus(),
    calendars: () => api.calendars(),
  } satisfies Partial<Api>),
);

import { Settings } from './Settings';

const info = (store: { ok: true } | { ok: false; message: string }) => ({
  ok: true,
  recordings_dir: '/recordings',
  vault_dir: '/vault',
  vault_folder: 'Meetings',
  store,
});
const idle = { ok: true, running: false, done: 0, total: 0 };

beforeEach(() => {
  vi.useFakeTimers({ shouldAdvanceTime: true });
  api.settingsInfo.mockReset().mockResolvedValue(info({ ok: true }));
  api.rescan.mockReset().mockResolvedValue({ ok: true, started: true });
  api.rescanStatus.mockReset().mockResolvedValue(idle);
  api.health.mockReset().mockResolvedValue({ ok: true, checks: [{ name: 'config', ok: true, fix: '' }] });
  api.calendarStatus.mockReset().mockResolvedValue({ ok: true, access: 'granted' });
  api.calendars
    .mockReset()
    .mockResolvedValue({ ok: true, calendars: [{ id: 'w', title: 'Work', account: 'me', hidden: false }] });
});

afterEach(() => {
  vi.useRealTimers();
});

const tick = (ms: number) => act(() => vi.advanceTimersByTimeAsync(ms));

describe('Settings', () => {
  it('renders every section in visual order and reaches the controls with the keyboard', async () => {
    const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime });
    render(<Settings />);
    await screen.findByRole('checkbox', { name: /Work/ });
    await screen.findByRole('list', { name: 'Health checks' });
    await screen.findByText('Version 0.1.0');

    expect(screen.getAllByRole('heading', { level: 3 }).map((h) => h.textContent)).toEqual([
      'Calendars',
      'Updates',
      'Meetings',
      'Folders',
      'Health',
    ]);
    await user.tab();
    expect(screen.getByRole('checkbox', { name: /Work/ })).toHaveFocus();
    await user.tab();
    expect(screen.getByRole('checkbox', { name: 'Install updates automatically' })).toHaveFocus();
    await user.tab();
    expect(screen.getByRole('button', { name: 'Check for updates' })).toHaveFocus();
    await user.tab();
    expect(screen.getByRole('button', { name: 'Rescan folders' })).toHaveFocus();
    await user.tab();
    expect(screen.getByRole('button', { name: 'Check again' })).toHaveFocus();
  });

  it('shows the store problem with Rescan folders and clears it after a rescan with an ok store', async () => {
    api.settingsInfo.mockResolvedValue(info({ ok: false, message: 'file is not a database' }));
    const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime });
    render(<Settings />);

    expect(await screen.findByRole('alert')).toHaveTextContent(
      "Notes database can't be opened: file is not a database",
    );
    expect(screen.getByText('/recordings')).toBeInTheDocument();
    expect(await screen.findByRole('list', { name: 'Health checks' })).toBeInTheDocument();

    api.settingsInfo.mockResolvedValue(info({ ok: true }));
    api.rescanStatus.mockResolvedValueOnce({ ok: true, running: true, done: 1, total: 2 }).mockResolvedValue(idle);
    await user.click(screen.getByRole('button', { name: 'Rescan folders' }));
    expect(await screen.findByText('Loading meetings… 1 of 2')).toBeInTheDocument();
    await tick(1000);

    expect(await screen.findByText('Rescan finished')).toBeInTheDocument();
    expect(screen.queryByRole('alert')).not.toBeInTheDocument();
    expect(api.settingsInfo).toHaveBeenCalledTimes(2);
  });

  it('keeps the other sections usable when one fails', async () => {
    api.settingsInfo.mockResolvedValue({ ok: false, code: 'not_configured', message: 'Run meeting install' });
    api.health.mockRejectedValue(new Error('ipc closed'));
    render(<Settings />);

    expect(await screen.findByText('Run meeting install')).toBeInTheDocument();
    expect(await screen.findByText('Could not reach the Meeting app: ipc closed')).toBeInTheDocument();
    expect(await screen.findByRole('checkbox', { name: /Work/ })).toBeEnabled();
    expect(screen.getByRole('button', { name: 'Rescan folders' })).toBeEnabled();
  });

  it('writes nothing to the console', async () => {
    const spies = (['log', 'info', 'warn', 'error', 'debug'] as const).map((m) => vi.spyOn(console, m));
    render(<Settings />);
    await screen.findByRole('checkbox', { name: /Work/ });
    await screen.findByRole('list', { name: 'Health checks' });

    for (const spy of spies) {
      expect(spy).not.toHaveBeenCalled();
      spy.mockRestore();
    }
  });
});
