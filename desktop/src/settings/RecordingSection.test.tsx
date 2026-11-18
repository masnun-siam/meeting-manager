import { act, render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import * as api from '../api';
import { deferred } from '../recording/test-status';
import { RecordingSection } from './RecordingSection';

vi.mock('../api', async () => (await import('../api.mock')).mockApi());

const indicatorSettings = vi.mocked(api.indicatorSettings);
const setIndicatorEnabled = vi.mocked(api.setIndicatorEnabled);
type SetReply = Awaited<ReturnType<typeof api.setIndicatorEnabled>>;

const LOCKED = { ok: false as const, code: 'store_unavailable' as const, message: 'database is locked' };

beforeEach(() => {
  vi.resetAllMocks();
  indicatorSettings.mockResolvedValue({ ok: true, enabled: true });
  setIndicatorEnabled.mockImplementation(async ({ enabled }) => ({ ok: true, enabled }));
});

const box = () => screen.getByRole('checkbox', { name: 'Show floating indicator while recording' });
const statusLine = () => screen.getByRole('status');

async function setup() {
  const user = userEvent.setup();
  render(<RecordingSection />);
  await screen.findByRole('checkbox', { name: 'Show floating indicator while recording', checked: true });
  return user;
}

describe('RecordingSection', () => {
  it('is a labelled section whose box is disabled until the setting is read', async () => {
    const read = deferred<Awaited<ReturnType<typeof api.indicatorSettings>>>();
    indicatorSettings.mockReturnValue(read.promise);
    render(<RecordingSection />);

    expect(screen.getByRole('region', { name: 'Recording' })).toBeInTheDocument();
    expect(box()).toBeDisabled();

    await act(async () => read.resolve({ ok: true, enabled: true }));
    expect(box()).toBeEnabled();
    expect(box()).toBeChecked();
    expect(statusLine()).toBeEmptyDOMElement();
  });

  it('shows the box unchecked when the setting is off', async () => {
    indicatorSettings.mockResolvedValue({ ok: true, enabled: false });
    render(<RecordingSection />);

    await waitFor(() => expect(box()).toBeEnabled());
    expect(box()).not.toBeChecked();
  });

  it('saves the setting off and on, showing each change at once', async () => {
    const user = await setup();

    await user.click(box());
    expect(box()).not.toBeChecked();
    await user.click(box());

    expect(box()).toBeChecked();
    expect(setIndicatorEnabled.mock.calls).toEqual([[{ enabled: false }], [{ enabled: true }]]);
  });

  it('ends two quick toggles on the latest reply when the first fails late', async () => {
    const first = deferred<SetReply>();
    setIndicatorEnabled.mockReturnValueOnce(first.promise);
    const user = await setup();

    await user.click(box());
    await user.click(box());
    expect(box()).toBeChecked();
    await act(async () => first.resolve(LOCKED));

    expect(setIndicatorEnabled.mock.calls).toEqual([[{ enabled: false }], [{ enabled: true }]]);
    expect(box()).toBeChecked();
    expect(statusLine()).toBeEmptyDOMElement();
  });

  it('returns the box to the confirmed value with the reason when the store is unavailable', async () => {
    setIndicatorEnabled.mockResolvedValue(LOCKED);
    const user = await setup();

    await user.click(box());

    expect(await screen.findByText('database is locked')).toBe(statusLine());
    expect(box()).toBeChecked();
  });

  it('returns the box to the confirmed value when the save is rejected', async () => {
    setIndicatorEnabled.mockRejectedValue(new Error('ipc closed'));
    const user = await setup();

    await user.click(box());

    expect(await screen.findByText('Could not reach the Meeting app: ipc closed')).toBe(statusLine());
    expect(box()).toBeChecked();
  });

  it('shows the box checked with the reason when the setting cannot be read from the store', async () => {
    indicatorSettings.mockResolvedValue(LOCKED);
    const user = await setup();

    expect(statusLine()).toHaveTextContent('database is locked');
    await user.click(box());

    expect(box()).not.toBeChecked();
    expect(statusLine()).toBeEmptyDOMElement();
  });

  it('shows a rejected read and keeps the box disabled', async () => {
    indicatorSettings.mockRejectedValue(new Error('ipc closed'));
    render(<RecordingSection />);

    expect(await screen.findByText('Could not reach the Meeting app: ipc closed')).toBe(statusLine());
    expect(box()).toBeDisabled();
  });
});
