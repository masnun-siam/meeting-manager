import { cleanup, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { Tabs } from './TranscriptTab';

afterEach(cleanup);

const TRANSCRIPT = [
  '[00:12:30] Ayesha: see <b>Speaker 1:</b>',
  'A line with no time',
  '[01:02:03] Rahim: Hi.',
  '[1:02:03] Rahim: a malformed time',
].join('\n');

/** Renders the tabs with the Transcript tab selected. */
async function setup(transcript: string | null, onSeek?: (seconds: number) => void) {
  const user = userEvent.setup();
  const view = render(
    <Tabs
      summary={<p>summary</p>}
      transcript={transcript}
      myNotes={<p>mine</p>}
      hasMyNotes={false}
      aside={null}
      onSeek={onSeek}
    />,
  );
  await user.click(screen.getByRole('tab', { name: 'Transcript' }));
  return { user, ...view };
}

const panel = () => screen.getByRole('tabpanel', { name: 'Transcript' });
const stamps = () => screen.queryAllByRole('button', { name: /^Jump to / });

describe('Transcript times (FR-6)', () => {
  it('makes each matching line’s time a button named for its action, other lines stay text', async () => {
    await setup(TRANSCRIPT, vi.fn());

    expect(stamps().map((b) => b.getAttribute('aria-label'))).toEqual(['Jump to 00:12:30', 'Jump to 01:02:03']);
    expect(screen.getByRole('button', { name: 'Jump to 00:12:30' })).toHaveTextContent('[00:12:30]');
    expect(screen.getByText('A line with no time').querySelector('button')).toBeNull();
    expect(screen.getByText('[1:02:03] Rahim: a malformed time').querySelector('button')).toBeNull();
  });

  it('keeps every line’s text as it was, as text', async () => {
    await setup(TRANSCRIPT, vi.fn());

    const lines = Array.from(panel().querySelectorAll('p')).map((p) => p.textContent);
    expect(lines).toEqual([
      '[00:12:30] Ayesha: see <b>Speaker 1:</b>',
      'A line with no time',
      '[01:02:03] Rahim: Hi.',
      '[1:02:03] Rahim: a malformed time',
    ]);
    expect(panel().querySelector('b')).toBeNull();
  });

  it('seeks to the time on click, Enter and Space', async () => {
    const onSeek = vi.fn();
    const { user } = await setup(TRANSCRIPT, onSeek);

    await user.click(screen.getByRole('button', { name: 'Jump to 00:12:30' }));
    expect(onSeek).toHaveBeenLastCalledWith(750);

    screen.getByRole('button', { name: 'Jump to 01:02:03' }).focus();
    await user.keyboard('{Enter}');
    expect(onSeek).toHaveBeenLastCalledWith(3723);

    screen.getByRole('button', { name: 'Jump to 00:12:30' }).focus();
    await user.keyboard(' ');
    expect(onSeek).toHaveBeenLastCalledWith(750);
    expect(onSeek).toHaveBeenCalledTimes(3);
  });

  it('shows plain times without a player', async () => {
    await setup(TRANSCRIPT);

    expect(stamps()).toHaveLength(0);
    expect(panel().querySelector('button')).toBeNull();
    expect(panel()).toHaveTextContent('[00:12:30] Ayesha:');
  });

  it('turns the times into buttons when a player becomes ready, without remounting the tab', async () => {
    const { rerender } = await setup(TRANSCRIPT);
    const before = panel();

    rerender(
      <Tabs
        summary={<p>summary</p>}
        transcript={TRANSCRIPT}
        myNotes={<p>mine</p>}
        hasMyNotes={false}
        aside={null}
        onSeek={vi.fn()}
      />,
    );

    expect(panel()).toBe(before);
    expect(screen.getByRole('tab', { name: 'Transcript' })).toHaveAttribute('aria-selected', 'true');
    expect(stamps()).toHaveLength(2);
  });

  it('says No transcript. with nothing to click', async () => {
    await setup(null, vi.fn());

    expect(panel()).toHaveTextContent('No transcript.');
    expect(panel().querySelector('button')).toBeNull();
  });
});
