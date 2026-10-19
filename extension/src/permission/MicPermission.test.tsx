import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { MicPermission } from './MicPermission';

const getUserMedia = vi.fn();

beforeEach(() => {
  getUserMedia.mockReset();
  vi.stubGlobal('navigator', { mediaDevices: { getUserMedia } });
});

afterEach(() => {
  vi.unstubAllGlobals();
});

const click = () => userEvent.click(screen.getByRole('button', { name: 'Allow microphone' }));

describe('MicPermission', () => {
  it('explains why the microphone is needed', () => {
    render(<MicPermission />);
    expect(screen.getByRole('heading', { name: 'Allow the microphone' })).toBeInTheDocument();
    expect(
      screen.getByText('Meeting Manager needs your microphone so your voice is in recordings.'),
    ).toBeInTheDocument();
    expect(getUserMedia).not.toHaveBeenCalled();
  });

  it('stops every track and confirms on success', async () => {
    const stops = [vi.fn(), vi.fn()];
    getUserMedia.mockResolvedValue({ getTracks: () => stops.map((stop) => ({ stop })) });
    render(<MicPermission />);
    await click();
    expect(getUserMedia).toHaveBeenCalledWith({ audio: true });
    for (const stop of stops) expect(stop).toHaveBeenCalledOnce();
    expect(await screen.findByRole('status')).toHaveTextContent(
      'Microphone allowed. Go back to your meeting tab and press Start again.',
    );
    expect(screen.getByRole('button', { name: 'Allow microphone' })).toBeDisabled();
  });

  it('says the microphone is blocked on NotAllowedError', async () => {
    getUserMedia.mockRejectedValue(new DOMException('', 'NotAllowedError'));
    render(<MicPermission />);
    await click();
    expect(await screen.findByRole('status')).toHaveTextContent(
      "Microphone blocked. Recordings will include tab audio only. To change this, allow the microphone for Meeting Manager in Chrome's site settings.",
    );
  });

  it('shows the message of any other error', async () => {
    getUserMedia.mockRejectedValue(new Error('boom'));
    render(<MicPermission />);
    await click();
    expect(await screen.findByRole('status')).toHaveTextContent('Could not get the microphone: boom');
  });
});
