import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it } from 'vitest';
import { Popup } from './Popup';

describe('Popup', () => {
  it('opens on the Record tab', () => {
    render(<Popup />);

    expect(screen.getByRole('tab', { name: 'Record' })).toHaveAttribute('aria-selected', 'true');
    expect(screen.getByRole('tab', { name: 'Jobs' })).toHaveAttribute('aria-selected', 'false');
    expect(screen.getByRole('tabpanel', { name: 'Record' })).toBeInTheDocument();
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
