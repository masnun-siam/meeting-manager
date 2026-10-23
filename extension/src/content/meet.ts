// Meet content script: idle until the service worker sends `context_start`, then reads the roster, title and who
// is speaking once a second and reports them in batches. A content script lives as long as its page, so state is
// module-level.
import type {
  ContentCommand,
  ContextHello,
  ContextHelloReply,
  ContextUpdate,
  SpeakingChange,
} from '../shared/messages';
import { diffSpeaking, readRoster, readSpeaking, readTitle } from './meetReader';

const READ_MS = 1_000;
const BATCH_EVERY = 10; // reads, so 10 s
const GIVE_UP_AFTER = 60; // reads, so 60 s

type Batch = Omit<ContextUpdate, 'type'>;

interface Session {
  timer: ReturnType<typeof setInterval>;
  ticks: number;
  union: Set<string>;
  selfName: string | null;
  title: string | null;
  sent: string;
  speaking: string[];
  pendingSpeaking: SpeakingChange[];
  gaveUp: boolean;
}

let session: Session | null = null;

function guarded<T>(read: () => T, fallback: T): T {
  try {
    return read();
  } catch {
    return fallback;
  }
}

function readPage(s: Session): void {
  for (const p of guarded(() => readRoster(document), [])) {
    s.union.add(p.name);
    if (p.self && s.selfName === null) s.selfName = p.name;
  }
  s.title = guarded(() => readTitle(document), null) ?? s.title;
  const speaking = readSpeaking(document);
  const change = diffSpeaking(s.speaking, speaking, Date.now());
  if (change) {
    s.speaking = speaking;
    s.pendingSpeaking.push(change);
  }
}

function collected(s: Session): Batch {
  const batch: Batch = {};
  if (s.title) batch.meetingTitle = s.title;
  if (s.union.size > 0) batch.participants = [...s.union].map((name) => ({ name, self: name === s.selfName }));
  return batch;
}

/** What changed since the last batch sent, or null. Marks it sent. */
function takeUnsent(s: Session): Batch | null {
  const batch = collected(s);
  const json = JSON.stringify(batch);
  const changed = Object.keys(batch).length > 0 && json !== s.sent;
  if (!changed && s.pendingSpeaking.length === 0) return null;
  s.sent = json;
  if (s.pendingSpeaking.length > 0) batch.speaking = s.pendingSpeaking.splice(0);
  return batch;
}

function send(message: ContextUpdate | ContextHello): Promise<unknown> {
  // Throws synchronously once the extension is reloaded under this page.
  try {
    return chrome.runtime.sendMessage(message).catch(() => undefined);
  } catch {
    return Promise.resolve(undefined);
  }
}

function tick(s: Session): void {
  s.ticks++;
  if (!s.gaveUp || s.ticks % BATCH_EVERY === 0) readPage(s);
  if (s.ticks % BATCH_EVERY !== 0) return;
  const batch = takeUnsent(s);
  if (batch) {
    s.gaveUp = false;
    void send({ type: 'context_update', ...batch });
  } else if (!s.gaveUp && s.ticks >= GIVE_UP_AFTER && s.union.size === 0 && !s.title) {
    s.gaveUp = true;
    void send({ type: 'context_update', gaveUp: true });
    console.info('Meeting Manager: nothing readable on this Meet page yet; checking every 10 s.');
  }
}

function start(): void {
  if (session) clearInterval(session.timer);
  const s: Session = {
    timer: setInterval(() => tick(s), READ_MS),
    ticks: 0,
    union: new Set(),
    selfName: null,
    title: null,
    sent: '',
    speaking: [],
    pendingSpeaking: [],
    gaveUp: false,
  };
  session = s;
}

/** Stops and returns the final batch: everything not yet sent. */
function stop(): Batch {
  if (!session) return {};
  const s = session;
  session = null;
  clearInterval(s.timer);
  readPage(s);
  return takeUnsent(s) ?? {};
}

chrome.runtime.onMessage.addListener((message: ContentCommand, _sender, sendResponse) => {
  switch (message.type) {
    case 'context_start':
      start();
      sendResponse({ ok: true });
      break;
    case 'context_stop':
      sendResponse(stop());
      break;
  }
});

// After a reload mid-recording the service worker still records this tab: resume with an empty union.
void send({ type: 'context_hello' }).then((reply) => {
  if ((reply as ContextHelloReply | undefined)?.recording && !session) start();
});
