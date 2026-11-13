import { act, render, screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import type { Api } from '../api.mock';

const api = vi.hoisted(() => ({ health: vi.fn() }));

vi.mock('../api', async () =>
  (await import('../api.mock')).mockApi({ health: () => api.health() } satisfies Partial<Api>),
);

import { HealthSection } from './HealthSection';

const checks = (...list: { name: string; ok: boolean; fix: string }[]) => ({ ok: true, checks: list });

beforeEach(() => {
  api.health
    .mockReset()
    .mockResolvedValue(
      checks(
        { name: 'config', ok: true, fix: '' },
        { name: 'claude', ok: false, fix: 'install Claude Code: npm i -g @anthropic-ai/claude-code' },
      ),
    );
});

const list = () => screen.findByRole('list', { name: 'Health checks' });

describe('HealthSection', () => {
  it('lists every check with a text status and the fix of a failing one', async () => {
    render(<HealthSection />);

    const [config, claude] = within(await list()).getAllByRole('listitem');
    expect(config).toHaveTextContent('config OK');
    expect(claude).toHaveTextContent('claude Failing');
    expect(claude).toHaveTextContent('install Claude Code: npm i -g @anthropic-ai/claude-code');
  });

  it('shows a failing meeting_path check with its fix', async () => {
    api.health.mockResolvedValue(checks({ name: 'meeting_path', ok: false, fix: 'run `meeting install`' }));
    render(<HealthSection />);

    expect(within(await list()).getByRole('listitem')).toHaveTextContent('meeting_path Failing run `meeting install`');
  });

  it('checks again, disabled while the call is in flight, and replaces the list', async () => {
    const user = userEvent.setup();
    render(<HealthSection />);
    await list();
    let answer!: (value: unknown) => void;
    api.health.mockReturnValueOnce(new Promise((r) => (answer = r)));

    await user.click(screen.getByRole('button', { name: 'Check again' }));
    expect(screen.getByRole('button', { name: 'Check again' })).toBeDisabled();
    await act(async () => answer(checks({ name: 'claude', ok: true, fix: '' })));

    expect(api.health).toHaveBeenCalledTimes(2);
    expect(within(await list()).getAllByRole('listitem')).toHaveLength(1);
    expect(screen.queryByText('Failing')).not.toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Check again' })).toBeEnabled();
  });

  it('disables Check again until the first answer', () => {
    api.health.mockReturnValue(new Promise(() => {}));
    render(<HealthSection />);

    expect(screen.getByRole('button', { name: 'Check again' })).toBeDisabled();
  });

  it('shows a rejected call instead of the list', async () => {
    api.health.mockRejectedValue(new Error('ipc closed'));
    render(<HealthSection />);

    expect(await screen.findByText('Could not reach the Meeting app: ipc closed')).toBeInTheDocument();
    expect(screen.queryByRole('list')).not.toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Check again' })).toBeEnabled();
  });
});
