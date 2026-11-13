import { useCallback, useEffect, useReducer, useRef, useState } from 'react';
import { type Note, type NoteUpdate, noteUpdate } from '../../api';
import { attempt } from '../../recording/attempt';
import { problemText, TOO_LARGE, tooLarge } from './format';

export const SAVE_DELAY_MS = 1000;

/** One editable field: the title, the summary markdown, "My notes" or one speaker's name. */
export type FieldKey = 'title' | 'body_md' | 'my_notes' | `speaker:${string}`;

export type SaveState = 'idle' | 'saving' | 'saved' | 'error';

export type Autosave = {
  /** What the field shows: the unsaved draft if there is one, else the stored value. */
  value: (key: FieldKey) => string;
  change: (key: FieldKey, value: string) => void;
  focus: (key: FieldKey) => void;
  blur: (key: FieldKey) => void;
  /** Sends every unsaved draft now and resolves once every queued save has replied. */
  flushAll: () => Promise<void>;
  /** Whether the focused field holds text not yet stored (the poll then skips a tick). */
  focusedDirty: () => boolean;
  /** Whether any field holds text that is not stored (after a refused or failed save). */
  unsaved: () => boolean;
  /** The stored note as of the last reply, ahead of the next render. */
  latest: () => Note | null;
  /** Re-sends a save with no field, which rewrites the vault note of a `done` note. */
  retryVault: () => void;
  saveState: SaveState;
  vaultError: string | null;
  problem: string | null;
};

const speakerLabel = (key: FieldKey) => (key.startsWith('speaker:') ? key.slice('speaker:'.length) : null);

export function stored(note: Note, key: FieldKey): string {
  const label = speakerLabel(key);
  if (label !== null) return note.speakers.find((speaker) => speaker.label === label)?.name ?? '';
  if (key === 'title') return note.title ?? '';
  if (key === 'body_md') return note.body_md ?? '';
  return note.my_notes;
}

function request(id: number, key: FieldKey, value: string): NoteUpdate {
  const label = speakerLabel(key);
  if (label !== null) return { id, speaker: { label, name: value.trim() } };
  return { id, [key]: value };
}

/**
 * Autosave for one note (FR-18): a field is saved 1 s after typing stops or when it loses focus.
 * Saves go out one at a time and each sends the field's latest draft. A reply replaces the stored
 * note, but a field typed into after its request left keeps its draft, so nothing typed is lost.
 */
export function useAutosave(opts: {
  id: number;
  note: Note | null;
  apply: (note: Note) => void;
  reload: () => void;
}): Autosave {
  const { id } = opts;
  const [, render] = useReducer((n: number) => n + 1, 0);
  const [saveState, setSaveState] = useState<SaveState>('idle');
  const [vaultError, setVaultError] = useState<string | null>(null);
  const [problem, setProblem] = useState<string | null>(null);

  const drafts = useRef(new Map<FieldKey, string>());
  const timers = useRef(new Map<FieldKey, ReturnType<typeof setTimeout>>());
  const queued = useRef(new Set<FieldKey>());
  const chain = useRef<Promise<void>>(Promise.resolve());
  const focused = useRef<FieldKey | null>(null);
  const active = useRef(true);
  const note = useRef(opts.note);
  note.current = opts.note;
  const callbacks = useRef({ apply: opts.apply, reload: opts.reload });
  callbacks.current = { apply: opts.apply, reload: opts.reload };

  const send = useCallback(async (req: NoteUpdate, key: FieldKey | null, value: string | null) => {
    if (active.current) setSaveState('saving');
    const result = await attempt(() => noteUpdate(req));
    if (!active.current) return;
    if (result.ok) {
      note.current = result.reply.note;
      if (key !== null && drafts.current.get(key) === value) drafts.current.delete(key);
      callbacks.current.apply(result.reply.note);
      setVaultError(result.reply.vault === 'failed' ? (result.reply.vault_error ?? 'unknown error') : null);
      setProblem(null);
      setSaveState('saved');
      return;
    }
    setProblem(problemText(result.code, result.text));
    setSaveState('error');
    if (result.code === 'not_editable') {
      if (key !== null) drafts.current.delete(key);
      callbacks.current.reload();
    }
    render();
  }, []);

  /** Sends `key`'s draft when its turn in the queue comes, reading the draft only then. */
  const sendField = useCallback(
    async (key: FieldKey) => {
      queued.current.delete(key);
      const value = drafts.current.get(key);
      const current = note.current;
      if (value === undefined || current === null) return;
      if (value === stored(current, key)) {
        drafts.current.delete(key);
        if (active.current) render();
        return;
      }
      if (speakerLabel(key) !== null && value.trim() === '') return;
      if (key === 'my_notes' && tooLarge(value)) {
        if (active.current) {
          setProblem(TOO_LARGE);
          setSaveState('error');
        }
        return;
      }
      await send(request(id, key, value), key, value);
    },
    [id, send],
  );

  const enqueue = useCallback((task: () => Promise<void>) => {
    chain.current = chain.current.then(task, task);
    return chain.current;
  }, []);

  const flush = useCallback(
    (key: FieldKey) => {
      const timer = timers.current.get(key);
      if (timer !== undefined) clearTimeout(timer);
      timers.current.delete(key);
      if (!drafts.current.has(key) || queued.current.has(key)) return chain.current;
      queued.current.add(key);
      return enqueue(() => sendField(key));
    },
    [enqueue, sendField],
  );

  const flushAll = useCallback(() => {
    // Renames go last: the Rust side rewrites the title and body with the new name.
    const keys = [...drafts.current.keys()];
    for (const key of keys) if (speakerLabel(key) === null) flush(key);
    for (const key of keys) if (speakerLabel(key) !== null) flush(key);
    return chain.current;
  }, [flush]);

  const change = useCallback(
    (key: FieldKey, value: string) => {
      drafts.current.set(key, value);
      const timer = timers.current.get(key);
      if (timer !== undefined) clearTimeout(timer);
      timers.current.set(
        key,
        setTimeout(() => {
          timers.current.delete(key);
          // A rename rewrites the title and body on the Rust side: send their edits first.
          if (speakerLabel(key) !== null) flushAll();
          else flush(key);
        }, SAVE_DELAY_MS),
      );
      render();
    },
    [flush, flushAll],
  );

  const blur = useCallback(
    (key: FieldKey) => {
      if (focused.current === key) focused.current = null;
      if (speakerLabel(key) !== null) {
        // An emptied name is never sent; leaving the field shows the stored name again.
        if (drafts.current.get(key)?.trim() === '') {
          clearTimeout(timers.current.get(key));
          timers.current.delete(key);
          drafts.current.delete(key);
          render();
          return;
        }
        flushAll();
      } else flush(key);
    },
    [flush, flushAll],
  );

  const focus = useCallback((key: FieldKey) => {
    focused.current = key;
  }, []);

  const focusedDirty = useCallback(() => focused.current !== null && drafts.current.has(focused.current), []);

  const retryVault = useCallback(() => {
    enqueue(() => send({ id }, null, null));
  }, [enqueue, id, send]);

  const value = useCallback(
    (key: FieldKey) => drafts.current.get(key) ?? (note.current ? stored(note.current, key) : ''),
    [],
  );

  // Leaving the view sends what is still unsaved; its replies are then ignored.
  useEffect(() => {
    active.current = true;
    return () => {
      active.current = false;
      flushAll();
    };
  }, [flushAll]);

  const latest = useCallback(() => note.current, []);
  const unsaved = useCallback(() => drafts.current.size > 0, []);

  return {
    value,
    change,
    focus,
    blur,
    flushAll,
    focusedDirty,
    unsaved,
    latest,
    retryVault,
    saveState,
    vaultError,
    problem,
  };
}
