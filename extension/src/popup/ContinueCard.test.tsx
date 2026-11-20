import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { cleanup, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';
import type { LastRecording } from '../shared/lastRecording';
import { ContinueCard } from './ContinueCard';

const T0 = new Date('2026-01-01T10:00:00Z').getTime();
const MIN = 60_000;

function last(extra: Partial<LastRecording> = {}): LastRecording {
  return {
    download_id: 7,
    path: '/Users/me/Downloads/Meetings/meeting-2026-01-01_0959/recording.webm',
    folder: 'meeting-2026-01-01_0959',
    ext: 'webm',
    recorded_ms: MIN,
    language: 'en',
    stopped_at: T0 - MIN,
    stop_reason: 'tab_closed',
    parts: 1,
    ...extra,
  };
}

afterEach(cleanup);

describe('ContinueCard', () => {
  it('shows the folder, recorded time, minutes since the stop and why it stopped', () => {
    render(<ContinueCard last={last()} now={T0} onContinue={() => {}} />);

    expect(screen.getByRole('region', { name: 'Continue last recording' })).toBeInTheDocument();
    expect(screen.getByText('meeting-2026-01-01_0959')).toBeInTheDocument();
    expect(screen.getByText('0:01:00')).toBeInTheDocument();
    expect(screen.getByText('stopped 1 min ago · tab closed')).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Continue' })).toBeInTheDocument();
  });

  it('says 0 min under a minute', () => {
    render(<ContinueCard last={last({ stopped_at: T0 - 59_999 })} now={T0} onContinue={() => {}} />);

    expect(screen.getByText('stopped 0 min ago · tab closed')).toBeInTheDocument();
  });

  it('never shows a negative age when the clock is behind the stop', () => {
    render(<ContinueCard last={last({ stopped_at: T0 + 5_000 })} now={T0} onContinue={() => {}} />);

    expect(screen.getByText('stopped 0 min ago · tab closed')).toBeInTheDocument();
  });

  it('shows "capture ended" for a capture stop', () => {
    render(<ContinueCard last={last({ stop_reason: 'capture_ended' })} now={T0} onContinue={() => {}} />);

    expect(screen.getByText('stopped 1 min ago · capture ended')).toBeInTheDocument();
  });

  it.each([
    ['user', 'user' as const],
    ['a recovered recording', null],
  ])('adds no reason for %s', (_, stop_reason) => {
    render(<ContinueCard last={last({ stop_reason })} now={T0} onContinue={() => {}} />);

    expect(screen.getByText('stopped 1 min ago')).toBeInTheDocument();
  });

  it('calls onContinue from the keyboard', async () => {
    const onContinue = vi.fn();
    const user = userEvent.setup();
    render(<ContinueCard last={last()} now={T0} onContinue={onContinue} />);

    await user.tab();
    expect(screen.getByRole('button', { name: 'Continue' })).toHaveFocus();
    await user.keyboard('{Enter}');

    expect(onContinue).toHaveBeenCalledTimes(1);
  });
});

describe('ContinueCard.module.css', () => {
  const css = readFileSync(join(import.meta.dirname, 'ContinueCard.module.css'), 'utf8');

  it('uses no colour literals', () => {
    expect(css).not.toMatch(/#[0-9a-f]{3,8}\b|rgba?\(|hsla?\(/i);
  });

  it('gives Continue the 36 px height of the other controls', () => {
    expect(css).toMatch(/\.continue\s*\{[^}]*min-height: 36px/);
  });
});
