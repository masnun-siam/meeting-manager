import { act, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { Api } from '../api.mock';

const api = vi.hoisted(() => ({ rescan: vi.fn(), rescanStatus: vi.fn() }));

vi.mock('../api', async () =>
  (await import('../api.mock')).mockApi({
    rescan: () => api.rescan(),
    rescanStatus: () => api.rescanStatus(),
  } satisfies Partial<Api>),
);

import { RescanSection } from './RescanSection';

const idle = { ok: true, running: false, done: 0, total: 0 };
const running = (done: number, total: number) => ({ ok: true, running: true, done, total });

beforeEach(() => {
  vi.useFakeTimers({ shouldAdvanceTime: true });
  api.rescan.mockReset().mockResolvedValue({ ok: true, started: true });
  api.rescanStatus.mockReset().mockResolvedValue(idle);
});

afterEach(() => {
  vi.useRealTimers();
});

function setup(storeProblem: string | null = null) {
  const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime });
  const onFinished = vi.fn();
  return { user, onFinished, ...render(<RescanSection storeProblem={storeProblem} onFinished={onFinished} />) };
}

const tick = (ms: number) => act(() => vi.advanceTimersByTimeAsync(ms));
const button = () => screen.getByRole('button', { name: 'Rescan folders' });

describe('RescanSection', () => {
  it('rescans, shows N of M every second while running, then Rescan finished', async () => {
    const { user, onFinished } = setup();
    await tick(0);
    api.rescanStatus.mockResolvedValueOnce(running(1, 4)).mockResolvedValueOnce(running(3, 4)).mockResolvedValue(idle);

    await user.click(button());

    expect(api.rescan).toHaveBeenCalledTimes(1);
    expect(await screen.findByText('Loading meetings… 1 of 4')).toHaveAttribute('role', 'status');
    expect(button()).toBeDisabled();
    await tick(1000);
    expect(await screen.findByText('Loading meetings… 3 of 4')).toBeInTheDocument();
    await tick(1000);

    expect(await screen.findByText('Rescan finished')).toHaveAttribute('role', 'status');
    expect(button()).toBeEnabled();
    expect(onFinished).toHaveBeenCalledTimes(1);
    const calls = api.rescanStatus.mock.calls.length;
    await tick(5000);
    expect(api.rescanStatus).toHaveBeenCalledTimes(calls);
  });

  it('shows a backfill already running on mount', async () => {
    api.rescanStatus.mockResolvedValueOnce(running(2, 9)).mockResolvedValue(idle);
    const { onFinished } = setup();

    expect(await screen.findByText('Loading meetings… 2 of 9')).toBeInTheDocument();
    expect(button()).toBeDisabled();
    await tick(1000);
    expect(await screen.findByText('Rescan finished')).toBeInTheDocument();
    expect(onFinished).toHaveBeenCalledTimes(1);
  });

  it('finishes a run that ended before its first status', async () => {
    const { user, onFinished } = setup();

    await user.click(button());

    expect(await screen.findByText('Rescan finished')).toBeInTheDocument();
    expect(onFinished).toHaveBeenCalledTimes(1);
  });

  it('does not poll when nothing runs', async () => {
    setup();
    await tick(5000);

    expect(api.rescanStatus).toHaveBeenCalledTimes(1);
  });

  it('shows an error reply as an alert and the button works again', async () => {
    api.rescan.mockResolvedValueOnce({ ok: false, code: 'not_configured', message: 'run meeting install' });
    const { user } = setup();

    await user.click(button());

    expect(await screen.findByRole('alert')).toHaveTextContent('run meeting install');
    expect(button()).toBeEnabled();
    await user.click(button());
    expect(api.rescan).toHaveBeenCalledTimes(2);
    expect(screen.queryByRole('alert')).not.toBeInTheDocument();
  });

  it('shows a rejected call as an alert', async () => {
    api.rescan.mockRejectedValueOnce(new Error('ipc closed'));
    const { user } = setup();

    await user.click(button());

    expect(await screen.findByRole('alert')).toHaveTextContent('Could not reach the Meeting app: ipc closed');
    expect(button()).toBeEnabled();
  });

  it('shows the store problem with the button', () => {
    setup('file is not a database');

    expect(screen.getByRole('alert')).toHaveTextContent("Notes database can't be opened: file is not a database");
    expect(button()).toBeInTheDocument();
  });

  it('stops polling on unmount and ignores a late reply', async () => {
    let finish!: (value: unknown) => void;
    api.rescanStatus.mockResolvedValueOnce(running(1, 2));
    const { unmount, onFinished } = setup();
    await screen.findByText('Loading meetings… 1 of 2');
    api.rescanStatus.mockReturnValueOnce(new Promise((r) => (finish = r))).mockResolvedValue(idle);
    await tick(1000);

    unmount();
    await act(async () => finish(idle));
    await tick(5000);

    expect(api.rescanStatus).toHaveBeenCalledTimes(2);
    expect(onFinished).not.toHaveBeenCalled();
  });
});
