import { render, screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it, vi } from 'vitest';

vi.mock('./api', async () => (await import('./api.mock')).mockApi());
vi.mock('./calendar/Upcoming', () => ({
  Upcoming: ({ onOpenNote }: { onOpenNote: (id: number) => void }) => (
    <button type="button" onClick={() => onOpenNote(7)}>
      Open earlier note
    </button>
  ),
}));

import { App } from './App';

const entry = (name: string) => within(screen.getByRole('navigation', { name: 'Main' })).getByRole('button', { name });

describe('App', () => {
  it('shows the sidebar, the recording bar and Upcoming first', () => {
    render(<App />);

    expect(entry('Upcoming')).toHaveAttribute('aria-current', 'page');
    expect(entry('All notes')).not.toHaveAttribute('aria-current');
    expect(entry('Settings')).not.toHaveAttribute('aria-current');
    expect(screen.getByRole('banner')).toHaveTextContent('Meeting');
    expect(screen.getByRole('button', { name: 'Open earlier note' })).toBeInTheDocument();
  });

  it('switches views from the sidebar and keeps the recording bar', async () => {
    const user = userEvent.setup();
    render(<App />);

    await user.click(entry('All notes'));
    expect(entry('All notes')).toHaveAttribute('aria-current', 'page');
    expect(entry('Upcoming')).not.toHaveAttribute('aria-current');
    expect(screen.getByRole('heading', { name: 'All notes' })).toBeInTheDocument();
    expect(screen.getByRole('banner')).toBeInTheDocument();

    await user.click(entry('Settings'));
    expect(screen.getByRole('heading', { name: 'Settings' })).toBeInTheDocument();
    expect(screen.queryByRole('heading', { name: 'All notes' })).not.toBeInTheDocument();
    expect(screen.getByRole('banner')).toBeInTheDocument();
  });

  it('reaches every sidebar entry with the keyboard', async () => {
    const user = userEvent.setup();
    render(<App />);

    await user.tab();
    expect(entry('Upcoming')).toHaveFocus();
    await user.tab();
    expect(entry('All notes')).toHaveFocus();
    await user.keyboard('{Enter}');
    expect(entry('All notes')).toHaveAttribute('aria-current', 'page');
    await user.tab();
    expect(entry('Settings')).toHaveFocus();
  });

  it('opens a note from Upcoming in All notes and goes back to the list', async () => {
    const user = userEvent.setup();
    render(<App />);

    await user.click(screen.getByRole('button', { name: 'Open earlier note' }));
    expect(entry('All notes')).toHaveAttribute('aria-current', 'page');
    expect(screen.getByRole('heading', { name: 'Note 7' })).toBeInTheDocument();

    await user.click(screen.getByRole('button', { name: 'Back' }));
    expect(screen.getByRole('heading', { name: 'All notes' })).toBeInTheDocument();
    expect(screen.queryByRole('heading', { name: 'Note 7' })).not.toBeInTheDocument();
  });
});
