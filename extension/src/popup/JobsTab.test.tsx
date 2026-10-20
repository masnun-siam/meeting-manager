import { act, cleanup, render, screen, within } from '@testing-library/react';
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
    recording: '/rec/meeting-2026-10-19_1430/recording.webm',
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

  it('lists the title, local date, stage, and the folder name when untitled', async () => {
    sendNative.mockResolvedValue(
      jobs(
        job(),
        job({ job_id: 'j2', title: null }),
        job({ job_id: 'j3', title: null, recording: '/rec/x.webm' }),
        job({ job_id: 'j4', title: null, recording: '/rec/meeting-x/recording.mp4' }),
      ),
    );
    setup();

    const rows = await screen.findAllByRole('listitem');
    expect(rows[0]).toHaveTextContent('Weekly sync');
    expect(rows[0]).toHaveTextContent(new Date('2026-10-19T14:30:00Z').toLocaleString());
    expect(rows[0]).toHaveTextContent('transcribing');
    expect(rows[1]).toHaveTextContent('meeting-2026-10-19_1430');
    expect(rows[1]).not.toHaveTextContent('recording.webm');
    expect(rows[2]).toHaveTextContent('x.webm');
    expect(rows[3]).toHaveTextContent('meeting-x');
    expect(rows[3]).not.toHaveTextContent('recording.mp4');
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

  it('shows Archive on done and failed rows only', async () => {
    const stages = ['queued', 'extracting', 'transcribing', 'summarising', 'writing', 'done', 'failed'] as const;
    sendNative.mockResolvedValue(jobs(...stages.map((stage) => job({ job_id: stage, stage }))));
    setup();

    const rows = await screen.findAllByRole('listitem');
    stages.forEach((stage, i) => {
      const archive = within(rows[i]).queryByRole('button', { name: 'Archive' });
      if (stage === 'done' || stage === 'failed') expect(archive).toBeInTheDocument();
      else expect(archive).not.toBeInTheDocument();
    });
  });

  it('orders done buttons with a summary and failed buttons Retry first', async () => {
    sendNative.mockResolvedValue(
      jobs(job({ stage: 'done', summary_path: '/s.md' }), job({ job_id: 'j2', stage: 'failed', error: 'x' })),
    );
    setup();

    const rows = await screen.findAllByRole('listitem');
    const names = (row: HTMLElement) =>
      within(row)
        .getAllByRole('button')
        .map((b) => b.textContent);
    expect(names(rows[0])).toEqual(['Open summary', 'Open folder', 'Archive', 'Trash']);
    expect(names(rows[1])).toEqual(['Retry', 'Archive', 'Trash']);
  });

  it('archives and reloads the list at once', async () => {
    sendNative.mockImplementation(async (r) => (r.type === 'list_jobs' ? jobs(job({ stage: 'done' })) : { ok: true }));
    const user = setup();
    await screen.findByRole('button', { name: 'Archive' });
    const before = listCalls();

    await user.click(screen.getByRole('button', { name: 'Archive' }));

    expect(sendNative).toHaveBeenCalledWith({ type: 'archive', job_id: 'j1' });
    expect(listCalls()).toBe(before + 1);
  });

  it('shows an archive error in the status line', async () => {
    sendNative.mockImplementation(async (r) =>
      r.type === 'list_jobs'
        ? jobs(job({ stage: 'done' }))
        : { ok: false, code: 'already_processing', message: 'already processing' },
    );
    const user = setup();

    await user.click(await screen.findByRole('button', { name: 'Archive' }));

    expect(await screen.findByRole('status')).toHaveTextContent('already processing');
  });

  it('shows a thrown archive error in the status line', async () => {
    sendNative.mockImplementation(async (r) => {
      if (r.type === 'list_jobs') return jobs(job({ stage: 'done' }));
      throw new Error('boom');
    });
    const user = setup();

    await user.click(await screen.findByRole('button', { name: 'Archive' }));

    expect(await screen.findByRole('status')).toHaveTextContent('Could not reach the meeting tool: boom');
  });

  it('shows the empty state after the last job is archived', async () => {
    let archived = false;
    sendNative.mockImplementation(async (r) => {
      if (r.type === 'archive') {
        archived = true;
        return { ok: true };
      }
      return jobs(...(archived ? [] : [job({ stage: 'done' })]));
    });
    const user = setup();

    await user.click(await screen.findByRole('button', { name: 'Archive' }));

    expect(await screen.findByText('No meetings processed yet')).toBeInTheDocument();
  });

  it('shows Trash on done and failed rows only', async () => {
    const stages = ['queued', 'extracting', 'transcribing', 'summarising', 'writing', 'done', 'failed'] as const;
    sendNative.mockResolvedValue(jobs(...stages.map((stage) => job({ job_id: stage, stage }))));
    setup();

    const rows = await screen.findAllByRole('listitem');
    stages.forEach((stage, i) => {
      const trash = within(rows[i]).queryByRole('button', { name: 'Trash' });
      if (stage === 'done' || stage === 'failed') expect(trash).toBeInTheDocument();
      else expect(trash).not.toBeInTheDocument();
    });
  });

  const trashCalls = () => sendNative.mock.calls.filter(([r]) => r.type === 'trash').length;
  const doneThenOk = (...list: JobSummary[]) =>
    sendNative.mockImplementation(async (r) => (r.type === 'list_jobs' ? jobs(...list) : { ok: true }));

  it('asks for a second click before trashing', async () => {
    doneThenOk(job({ stage: 'done' }));
    const user = setup();
    await user.click(await screen.findByRole('button', { name: 'Trash' }));

    expect(screen.getByRole('button', { name: 'Confirm trash' })).toBeInTheDocument();
    expect(trashCalls()).toBe(0);
    const before = listCalls();

    await user.click(screen.getByRole('button', { name: 'Confirm trash' }));

    expect(sendNative).toHaveBeenCalledWith({ type: 'trash', job_id: 'j1' });
    expect(trashCalls()).toBe(1);
    expect(listCalls()).toBe(before + 1);
    expect(screen.getByRole('button', { name: 'Trash' })).toBeInTheDocument();
  });

  it('reverts to Trash after 3 s', async () => {
    doneThenOk(job({ stage: 'done' }));
    const user = setup();
    await user.click(await screen.findByRole('button', { name: 'Trash' }));

    await act(() => vi.advanceTimersByTimeAsync(3000));

    expect(screen.getByRole('button', { name: 'Trash' })).toBeInTheDocument();
    expect(trashCalls()).toBe(0);
    await user.click(screen.getByRole('button', { name: 'Trash' }));
    expect(screen.getByRole('button', { name: 'Confirm trash' })).toBeInTheDocument();
    expect(trashCalls()).toBe(0);
  });

  it('arms only the clicked row', async () => {
    doneThenOk(job({ stage: 'done' }), job({ job_id: 'j2', stage: 'failed', error: 'x' }));
    const user = setup();
    const rows = await screen.findAllByRole('listitem');

    await user.click(within(rows[1]).getByRole('button', { name: 'Trash' }));

    expect(within(rows[1]).getByRole('button', { name: 'Confirm trash' })).toBeInTheDocument();
    expect(within(rows[0]).getByRole('button', { name: 'Trash' })).toBeInTheDocument();
  });

  it('shows a trash error in the status line', async () => {
    sendNative.mockImplementation(async (r) =>
      r.type === 'list_jobs'
        ? jobs(job({ stage: 'done' }))
        : { ok: false, code: 'trash_failed', message: 'could not trash' },
    );
    const user = setup();
    await user.click(await screen.findByRole('button', { name: 'Trash' }));

    await user.click(screen.getByRole('button', { name: 'Confirm trash' }));

    expect(await screen.findByRole('status')).toHaveTextContent('could not trash');
  });

  it('clears the confirm timer on unmount', async () => {
    doneThenOk(job({ stage: 'done' }));
    const user = setup();
    await user.click(await screen.findByRole('button', { name: 'Trash' }));

    cleanup();

    expect(vi.getTimerCount()).toBe(0);
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
