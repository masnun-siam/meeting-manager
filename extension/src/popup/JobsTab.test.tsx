import { act, cleanup, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { JobSummary } from '../shared/native';
import { JobsTab } from './JobsTab';

const sendNative = vi.fn();
vi.mock('../shared/nativeClient', async (importOriginal) => ({
  ...(await importOriginal<typeof import('../shared/nativeClient')>()),
  sendNative: (request: unknown) => sendNative(request),
}));

import { HOST_MISSING_MESSAGE } from '../shared/nativeClient';

function job(extra: Partial<JobSummary> = {}): JobSummary {
  return {
    job_id: 'j1',
    recording: '/rec/meeting-2026-10-19_1430.webm',
    title: 'Weekly sync',
    stage: 'transcribing',
    error: null,
    started_at: '2026-10-19T14:30:00Z',
    updated_at: '2026-10-19T14:31:00Z',
    summary_path: null,
    ...extra,
  };
}

const jobs = (...list: JobSummary[]) => ({ ok: true, jobs: list });
const listCalls = () => sendNative.mock.calls.filter(([r]) => r.type === 'list_jobs').length;

beforeEach(() => {
  vi.useFakeTimers({ shouldAdvanceTime: true });
  sendNative.mockReset();
});

afterEach(() => {
  cleanup();
  vi.useRealTimers();
});

function setup() {
  const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime });
  render(<JobsTab />);
  return user;
}

describe('JobsTab', () => {
  it('shows Loading… until the first reply', async () => {
    sendNative.mockReturnValue(new Promise(() => {}));
    setup();

    expect(screen.getByText('Loading…')).toBeInTheDocument();
    expect(sendNative).toHaveBeenCalledWith({ type: 'list_jobs', limit: 10 });
  });

  it('shows the empty state', async () => {
    sendNative.mockResolvedValue(jobs());
    setup();

    expect(await screen.findByText('No meetings processed yet')).toBeInTheDocument();
  });

  it('shows the host-missing message', async () => {
    sendNative.mockResolvedValue({ ok: false, code: 'host_missing', message: HOST_MISSING_MESSAGE });
    setup();

    expect(await screen.findByRole('alert')).toHaveTextContent(HOST_MISSING_MESSAGE);
  });

  it('shows another error reply’s message', async () => {
    sendNative.mockResolvedValue({ ok: false, code: 'bad_request', message: 'bad origin' });
    setup();

    expect(await screen.findByRole('alert')).toHaveTextContent('bad origin');
  });

  it('shows a thrown error', async () => {
    sendNative.mockRejectedValue(new Error('boom'));
    setup();

    expect(await screen.findByRole('alert')).toHaveTextContent('Could not reach the meeting tool: boom');
  });

  it('lists the title, local date, stage, and the file name when untitled', async () => {
    sendNative.mockResolvedValue(jobs(job(), job({ job_id: 'j2', title: null })));
    setup();

    const rows = await screen.findAllByRole('listitem');
    expect(rows[0]).toHaveTextContent('Weekly sync');
    expect(rows[0]).toHaveTextContent(new Date('2026-10-19T14:30:00Z').toLocaleString());
    expect(rows[0]).toHaveTextContent('transcribing');
    expect(rows[1]).toHaveTextContent('meeting-2026-10-19_1430.webm');
  });

  it('shows a failed job’s reason and a Retry button, no open buttons', async () => {
    sendNative.mockResolvedValue(jobs(job({ stage: 'failed', error: 'no api key' })));
    setup();

    expect(await screen.findByText('no api key')).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Retry' })).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: /Open/ })).not.toBeInTheDocument();
  });

  it('shows a stage change on the next poll', async () => {
    sendNative.mockResolvedValueOnce(jobs(job())).mockResolvedValue(jobs(job({ stage: 'summarising' })));
    setup();
    expect(await screen.findByText(/transcribing/)).toBeInTheDocument();

    await act(() => vi.advanceTimersByTimeAsync(3000));

    expect(await screen.findByText(/summarising/)).toBeInTheDocument();
  });

  it('sends open requests for the summary and the folder', async () => {
    sendNative.mockImplementation(async (r) =>
      r.type === 'list_jobs' ? jobs(job({ stage: 'done', summary_path: '/s.md' })) : { ok: true },
    );
    const user = setup();

    await user.click(await screen.findByRole('button', { name: 'Open summary' }));
    await user.click(screen.getByRole('button', { name: 'Open folder' }));

    expect(sendNative).toHaveBeenCalledWith({ type: 'open', job_id: 'j1', what: 'summary' });
    expect(sendNative).toHaveBeenCalledWith({ type: 'open', job_id: 'j1', what: 'folder' });
  });

  it('offers no Open summary without a summary path', async () => {
    sendNative.mockResolvedValue(jobs(job({ stage: 'done', summary_path: null })));
    setup();

    expect(await screen.findByRole('button', { name: 'Open folder' })).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'Open summary' })).not.toBeInTheDocument();
  });

  it('retries and reloads the list at once', async () => {
    sendNative.mockImplementation(async (r) =>
      r.type === 'list_jobs' ? jobs(job({ stage: 'failed', error: 'x' })) : { ok: true, job_id: 'j9' },
    );
    const user = setup();
    await screen.findByRole('button', { name: 'Retry' });
    const before = listCalls();

    await user.click(screen.getByRole('button', { name: 'Retry' }));

    expect(sendNative).toHaveBeenCalledWith({ type: 'retry', job_id: 'j1' });
    expect(listCalls()).toBe(before + 1);
  });

  it('shows an open error in the status line', async () => {
    sendNative.mockImplementation(async (r) =>
      r.type === 'list_jobs'
        ? jobs(job({ stage: 'done' }))
        : { ok: false, code: 'missing_file', message: 'Summary file is gone' },
    );
    const user = setup();

    await user.click(await screen.findByRole('button', { name: 'Open folder' }));

    expect(await screen.findByRole('status')).toHaveTextContent('Summary file is gone');
  });

  it('ignores a slower, older reply', async () => {
    let resolveFirst: (v: unknown) => void = () => {};
    sendNative
      .mockReturnValueOnce(new Promise((r) => (resolveFirst = r)))
      .mockResolvedValue(jobs(job({ stage: 'summarising' })));
    setup();

    await act(() => vi.advanceTimersByTimeAsync(3000));
    expect(await screen.findByText(/summarising/)).toBeInTheDocument();
    await act(async () => resolveFirst(jobs(job({ stage: 'queued' }))));

    expect(screen.queryByText(/queued/)).not.toBeInTheDocument();
  });

  it('stops polling after unmount', async () => {
    sendNative.mockResolvedValue(jobs());
    setup();
    await screen.findByText('No meetings processed yet');
    cleanup();
    const before = listCalls();

    await vi.advanceTimersByTimeAsync(9000);

    expect(listCalls()).toBe(before);
  });
});
