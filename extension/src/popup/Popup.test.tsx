import { cleanup, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
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
});
