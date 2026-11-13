import type { Ok, Reply } from '../api';
import { errorText, failureText } from './notices';

export type Attempt<T> = { ok: true; reply: Ok<T> } | { ok: false; code: string | null; text: string };

/** Runs one command and turns an error reply or a rejected call into the text the user sees. */
export async function attempt<T>(call: () => Promise<Reply<T>>): Promise<Attempt<T>> {
  try {
    const reply = await call();
    return reply.ok ? { ok: true, reply } : { ok: false, code: reply.code, text: errorText(reply) };
  } catch (error) {
    return { ok: false, code: null, text: failureText(error) };
  }
}
