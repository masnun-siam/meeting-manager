import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { ContentCommand, ContextHelloReply } from '../shared/messages';
import { readRoster } from './meetReader';

vi.mock('./meetReader', async (importOriginal) => {
  const real = await importOriginal<typeof import('./meetReader')>();
  return { readRoster: vi.fn(real.readRoster), readTitle: vi.fn(real.readTitle) };
});

type Listener = (message: ContentCommand, sender: unknown, sendResponse: (reply: unknown) => void) => void;

let listener: Listener;
let sendMessage: ReturnType<typeof vi.fn>;

async function load(hello: ContextHelloReply = { recording: false }) {
  sendMessage = vi.fn(async (message: { type: string }) => (message.type === 'context_hello' ? hello : undefined));
  vi.stubGlobal('chrome', {
    runtime: {
      sendMessage,
      onMessage: { addListener: (l: Listener) => (listener = l) },
    },
  });
  vi.resetModules();
  await import('./meet');
  await vi.advanceTimersByTimeAsync(0);
}

function command(message: ContentCommand): unknown {
  let reply: unknown;
  listener(message, {}, (r) => (reply = r));
  return reply;
}

const updates = () => sendMessage.mock.calls.map(([m]) => m).filter((m) => m.type === 'context_update');

function setPage(names: string[], title?: string) {
  document.title = title ? `Meet - ${title}` : 'Meet';
  const items = names
    .map(
      (n, i) => `<div role="listitem" aria-label="${n}"><span>${n}</span>${i === 0 ? '<span>(You)</span>' : ''}</div>`,
    )
    .join('');
  document.body.innerHTML = names.length ? `<div role="list" aria-label="Participants">${items}</div>` : '';
}

beforeEach(() => {
  vi.useFakeTimers();
  setPage([]);
});

afterEach(() => {
  command({ type: 'context_stop' });
  vi.useRealTimers();
  vi.unstubAllGlobals();
  vi.mocked(readRoster).mockReset();
});

describe('Meet content script', () => {
  it('says hello on load and reads nothing until context_start', async () => {
    setPage(['Sam', 'Alex'], 'Sync');
    await load();
    expect(sendMessage).toHaveBeenCalledWith({ type: 'context_hello' });
    await vi.advanceTimersByTimeAsync(30_000);
    expect(readRoster).not.toHaveBeenCalled();
    expect(updates()).toEqual([]);
  });

  it('sends the first batch within 10 s and none while nothing changes', async () => {
    setPage(['Sam', 'Alex'], 'Sync');
    await load();
    expect(command({ type: 'context_start', startedAt: 1 })).toEqual({ ok: true });
    await vi.advanceTimersByTimeAsync(10_000);
    expect(updates()).toEqual([
      {
        type: 'context_update',
        meetingTitle: 'Sync',
        participants: [
          { name: 'Sam', self: true },
          { name: 'Alex', self: false },
        ],
      },
    ]);
    await vi.advanceTimersByTimeAsync(30_000);
    expect(updates()).toHaveLength(1);
  });

  it('keeps a name in the union after it leaves the page and adds later joiners', async () => {
    setPage(['Sam', 'Alex']);
    await load();
    command({ type: 'context_start', startedAt: 1 });
    await vi.advanceTimersByTimeAsync(10_000);
    setPage(['Sam', 'Riley']);
    await vi.advanceTimersByTimeAsync(10_000);
    expect(updates()[1].participants).toEqual([
      { name: 'Sam', self: true },
      { name: 'Alex', self: false },
      { name: 'Riley', self: false },
    ]);
    expect(updates()[1]).not.toHaveProperty('meetingTitle');
  });

  it('gives up after 60 s with an empty page, then recovers when data appears', async () => {
    const info = vi.spyOn(console, 'info').mockImplementation(() => {});
    await load();
    command({ type: 'context_start', startedAt: 1 });
    await vi.advanceTimersByTimeAsync(59_000);
    expect(updates()).toEqual([]);
    await vi.advanceTimersByTimeAsync(1_000);
    expect(updates()).toEqual([{ type: 'context_update', gaveUp: true }]);
    expect(info).toHaveBeenCalledTimes(1);
    expect(info.mock.calls[0].join(' ')).not.toMatch(/Sam|Sync/);
    vi.mocked(readRoster).mockClear();
    await vi.advanceTimersByTimeAsync(10_000);
    expect(readRoster).toHaveBeenCalledTimes(1);
    setPage(['Sam'], 'Sync');
    await vi.advanceTimersByTimeAsync(10_000);
    expect(updates()).toHaveLength(2);
    expect(updates()[1]).toEqual({
      type: 'context_update',
      meetingTitle: 'Sync',
      participants: [{ name: 'Sam', self: true }],
    });
    expect(info).toHaveBeenCalledTimes(1);
    info.mockRestore();
  });

  it('replies to context_stop with what is not yet sent, then goes idle', async () => {
    setPage(['Sam'], 'Sync');
    await load();
    command({ type: 'context_start', startedAt: 1 });
    await vi.advanceTimersByTimeAsync(10_000);
    setPage(['Sam', 'Alex'], 'Sync');
    expect(command({ type: 'context_stop' })).toEqual({
      meetingTitle: 'Sync',
      participants: [
        { name: 'Sam', self: true },
        { name: 'Alex', self: false },
      ],
    });
    vi.mocked(readRoster).mockClear();
    await vi.advanceTimersByTimeAsync(30_000);
    expect(readRoster).not.toHaveBeenCalled();
    expect(updates()).toHaveLength(1);
  });

  it('replies to context_stop with nothing when everything was sent, and when not started', async () => {
    setPage(['Sam']);
    await load();
    expect(command({ type: 'context_stop' })).toEqual({});
    command({ type: 'context_start', startedAt: 1 });
    await vi.advanceTimersByTimeAsync(10_000);
    expect(command({ type: 'context_stop' })).toEqual({});
  });

  it('resumes when the hello reply says this tab is recording', async () => {
    setPage(['Sam']);
    await load({ recording: true, startedAt: 1 });
    await vi.advanceTimersByTimeAsync(10_000);
    expect(updates()).toEqual([{ type: 'context_update', participants: [{ name: 'Sam', self: true }] }]);
  });

  it('never lets a reader error escape', async () => {
    setPage(['Sam'], 'Sync');
    vi.mocked(readRoster).mockImplementation(() => {
      throw new Error('Meet changed');
    });
    await load();
    command({ type: 'context_start', startedAt: 1 });
    await vi.advanceTimersByTimeAsync(10_000);
    expect(updates()).toEqual([{ type: 'context_update', meetingTitle: 'Sync' }]);
  });

  it('survives a sendMessage that throws (extension reloaded)', async () => {
    setPage(['Sam']);
    await load();
    sendMessage.mockImplementation(() => {
      throw new Error('Extension context invalidated.');
    });
    command({ type: 'context_start', startedAt: 1 });
    await expect(vi.advanceTimersByTimeAsync(10_000)).resolves.not.toThrow();
  });
});
