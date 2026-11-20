import { act, cleanup, render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { RecorderStatus } from '../shared/messages';
import { Popup } from './Popup';

describe('Popup', () => {
  beforeEach(() => {
    vi.stubGlobal('chrome', {
      storage: {
        session: {
          get: vi.fn().mockResolvedValue({}),
          onChanged: { addListener: vi.fn(), removeListener: vi.fn() },
        },
        local: { get: vi.fn().mockResolvedValue({}) },
      },
      runtime: { sendMessage: vi.fn() },
    });
  });

  afterEach(() => {
    cleanup(); // unmount while `chrome` is still stubbed
    vi.unstubAllGlobals();
  });

  it('shows the wordmark with its dot hidden from screen readers', () => {
    render(<Popup />);

    const wordmark = screen.getByText('meeting', { exact: false, selector: 'span' });
    expect(wordmark).toHaveTextContent('meeting.');
    expect(wordmark.querySelector('[aria-hidden="true"]')).toHaveTextContent('.');
  });

  it('opens on the Record tab', () => {
    render(<Popup />);

    expect(screen.getByRole('tab', { name: 'Record' })).toHaveAttribute('aria-selected', 'true');
    expect(screen.getByRole('tab', { name: 'Jobs' })).toHaveAttribute('aria-selected', 'false');
    expect(screen.getByRole('tabpanel', { name: 'Record' })).toBeInTheDocument();
  });

  it('points only the selected tab at the rendered panel', () => {
    render(<Popup />);

    const panel = screen.getByRole('tabpanel');
    expect(screen.getByRole('tab', { name: 'Record' })).toHaveAttribute('aria-controls', panel.id);
    expect(screen.getByRole('tab', { name: 'Jobs' })).not.toHaveAttribute('aria-controls');
  });

  it('switches to the Jobs panel when Jobs is clicked', async () => {
    render(<Popup />);

    await userEvent.click(screen.getByRole('tab', { name: 'Jobs' }));

    expect(screen.getByRole('tab', { name: 'Jobs' })).toHaveAttribute('aria-selected', 'true');
    expect(screen.getByRole('tab', { name: 'Record' })).toHaveAttribute('aria-selected', 'false');
    expect(screen.getByRole('tabpanel', { name: 'Jobs' })).toBeInTheDocument();
    expect(screen.queryByRole('tabpanel', { name: 'Record' })).not.toBeInTheDocument();
  });

  it('moves focus to the Record tab when the last message closes while saving', async () => {
    const saving = (notice: RecorderStatus['notice']): RecorderStatus => ({
      state: { phase: 'saving', startedAt: 0, recordedMs: 60_000, reason: 'user' },
      language: 'bn',
      micIncluded: null,
      notice,
      tabId: null,
    });
    let onChanged: (changes: Record<string, { newValue?: unknown }>) => void = () => {};
    vi.stubGlobal('chrome', {
      storage: {
        session: {
          get: vi.fn().mockResolvedValue({ recorder: saving({ kind: 'error', message: 'Disk full' }) }),
          onChanged: {
            addListener: vi.fn((fn) => {
              onChanged = fn;
            }),
            removeListener: vi.fn(),
          },
        },
        local: { get: vi.fn().mockResolvedValue({}) },
      },
      runtime: { sendMessage: vi.fn().mockResolvedValue(undefined) },
    });
    render(<Popup />);

    await userEvent.click(await screen.findByRole('button', { name: 'Dismiss message' }));
    act(() => onChanged({ recorder: { newValue: saving(null) } }));

    await waitFor(() => expect(screen.getByRole('tab', { name: 'Record' })).toHaveFocus());
  });
});
