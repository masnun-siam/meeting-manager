import { render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import * as api from './api';

vi.mock('./api', async () => (await import('./api.mock')).mockApi());
vi.mock('./calendar/Upcoming', () => ({
  Upcoming: ({ onOpenNote }: { onOpenNote: (id: number) => void }) => (
    <button type="button" onClick={() => onOpenNote(7)}>
      Open earlier note
    </button>
  ),
}));

import { App } from './App';
import { stubMedia } from './notes/view/mediaStub';

// An opened note mounts the recording player, which jsdom cannot play.
beforeEach(() => stubMedia());

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

    // The sidebar comes first, as in a Mac app; then the recording bar's Language select and Start button.
    const start = screen.getByRole('button', { name: 'Start recording' });
    await waitFor(() => expect(start).toBeEnabled());
    await user.tab();
    expect(entry('Upcoming')).toHaveFocus();
    await user.tab();
    expect(entry('All notes')).toHaveFocus();
    await user.keyboard('{Enter}');
    expect(entry('All notes')).toHaveAttribute('aria-current', 'page');
    await user.tab();
    expect(entry('Settings')).toHaveFocus();
    await user.tab();
    expect(screen.getByRole('combobox', { name: 'Language' })).toHaveFocus();
    await user.tab();
    expect(start).toHaveFocus();
  });

  it('goes back to the list when All notes is chosen while a note is open', async () => {
    const user = userEvent.setup();
    render(<App />);

    await user.click(screen.getByRole('button', { name: 'Open earlier note' }));
    expect(await screen.findByRole('heading', { level: 2, name: 'Weekly sync' })).toBeInTheDocument();
    await user.click(entry('All notes'));
    expect(await screen.findByRole('heading', { name: 'All notes' })).toBeInTheDocument();
  });

  it('opens a note from Upcoming in All notes and goes back to the list', async () => {
    const user = userEvent.setup();
    render(<App />);

    await user.click(screen.getByRole('button', { name: 'Open earlier note' }));
    expect(entry('All notes')).toHaveAttribute('aria-current', 'page');
    expect(await screen.findByRole('heading', { level: 2, name: 'Weekly sync' })).toBeInTheDocument();
    expect(api.noteGet).toHaveBeenCalledWith({ id: 7 });

    await user.click(screen.getByRole('button', { name: 'Back' }));
    expect(screen.getByRole('heading', { name: 'All notes' })).toBeInTheDocument();
    expect(screen.queryByRole('heading', { name: 'Weekly sync' })).not.toBeInTheDocument();
  });
});
