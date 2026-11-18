import { type KeyboardEvent, type ReactNode, type RefObject, useEffect, useId, useRef, useState } from 'react';
import { type Note, noteArchive, noteCopy, noteOpen, noteRerun, noteTrash } from '../../api';
import { ArchiveIcon, CheckIcon, CopyIcon, FolderIcon, SparkIcon, TrashIcon, VaultIcon } from '../../icons';
import { attempt } from '../../recording/attempt';
import styles from './Actions.module.css';
import { hasEdits, NOT_WHILE_PROCESSING, problemText, RECORDING_MISSING } from './format';
import { RerunDialog } from './RerunDialog';

export const CONFIRM_MS = 3000;
export const COPIED_MS = 2000;
export const COPIED = 'Copied';
export const RERUN_STARTED = 'Summary rerun started';
export const ARCHIVED = 'Archived';

type CopyFormat = 'markdown' | 'rich';
const COPY_LABELS: Record<CopyFormat, string> = { markdown: 'Copy markdown', rich: 'Copy formatted' };

type Props = {
  note: Note;
  /** The latest stored note, read after pending saves are flushed. */
  latest: () => Note;
  flushAll: () => Promise<void>;
  reload: () => Promise<{ ok: boolean; code?: string | null }>;
  markRerun: () => void;
  onBack: () => void;
  say: (message: string | null) => void;
};

/** One entry of the More menu; later actions are inserted into the list in the PRD's order. */
type MenuItem = {
  id: string;
  label: string;
  icon: ReactNode;
  disabled: boolean;
  /** Shown inside the menu and read as the item's description while it is disabled. */
  reason: string | null;
  danger?: boolean;
  armed?: boolean;
  /** A first confirm step: the menu stays open so the "Confirm …" item can be chosen. */
  keepOpen?: boolean;
  run: () => void;
};

/** Copy markdown, Copy formatted and Open in Obsidian, then a More menu with Open folder, Rerun summary, Archive and Trash (FR-13). */
export function Actions(props: Props) {
  const { note, say } = props;
  const [armed, setArmed] = useState(false);
  const [asking, setAsking] = useState(false);
  const [busy, setBusy] = useState(false);
  // A new object on every success, so a second copy within 2 s restarts the timer.
  const [copied, setCopied] = useState<{ format: CopyFormat } | null>(null);
  const [copying, setCopying] = useState(false);
  const more = useRef<HTMLButtonElement>(null);
  // Not `busy`: a second Rerun waits on this instead of being disabled.
  const rerunning = useRef(false);
  // `copying` is a render behind a fast second click; this ref is not.
  const copyInFlight = useRef(false);

  useEffect(() => {
    if (!armed) return;
    const timer = setTimeout(() => setArmed(false), CONFIRM_MS);
    return () => clearTimeout(timer);
  }, [armed]);

  useEffect(() => {
    if (!copied) return;
    const timer = setTimeout(() => setCopied(null), COPIED_MS);
    return () => clearTimeout(timer);
  }, [copied]);

  const processing = note.status === 'processing';
  const missing = note.recording_missing;

  const open = async (what: 'obsidian' | 'folder') => {
    say(null);
    const result = await attempt(() => noteOpen({ id: note.id, what }));
    if (!result.ok) say(problemText(result.code, result.text));
  };

  /** Flushes pending saves first: the Rust side copies the stored note (D14: even if a save failed). */
  const copy = async (format: CopyFormat) => {
    if (copyInFlight.current) return;
    copyInFlight.current = true;
    setCopying(true);
    say(null);
    await props.flushAll();
    const result = await attempt(() => noteCopy({ id: note.id, format }));
    copyInFlight.current = false;
    setCopying(false);
    if (result.ok) {
      setCopied({ format });
      return;
    }
    setCopied(null);
    say(problemText(result.code, result.text));
  };

  const rerun = async (replaceEdits: boolean) => {
    setBusy(true);
    const result = await attempt(() => noteRerun({ id: note.id, replace_edits: replaceEdits }));
    setBusy(false);
    if (!result.ok) {
      say(problemText(result.code, result.text));
      return;
    }
    say(RERUN_STARTED);
    props.markRerun();
    void props.reload();
  };

  const askRerun = async () => {
    if (rerunning.current || asking) return;
    rerunning.current = true;
    say(null);
    await props.flushAll();
    if (hasEdits(props.latest())) setAsking(true);
    else await rerun(false);
    rerunning.current = false;
  };

  const closeDialog = () => {
    setAsking(false);
    more.current?.focus();
  };

  const archive = async () => {
    say(null);
    setBusy(true);
    await props.flushAll();
    const result = await attempt(() => noteArchive({ id: note.id }));
    setBusy(false);
    if (!result.ok) {
      say(problemText(result.code, result.text));
      return;
    }
    // Archiving a note whose recording is gone removes its row (recording-folders FR-9).
    const reloaded = await props.reload();
    if (!reloaded.ok && reloaded.code === 'unknown_note') props.onBack();
    else say(ARCHIVED);
  };

  const trash = async () => {
    if (!armed) {
      say(null);
      setArmed(true);
      return;
    }
    setArmed(false);
    setBusy(true);
    await props.flushAll();
    const result = await attempt(() => noteTrash({ id: note.id }));
    setBusy(false);
    if (result.ok) props.onBack();
    else say(problemText(result.code, result.text));
  };

  const missingReason = missing ? RECORDING_MISSING : null;
  const processingReason = processing ? NOT_WHILE_PROCESSING : null;
  const items: MenuItem[] = [
    {
      id: 'folder',
      label: 'Open folder',
      icon: <FolderIcon size={14} />,
      disabled: missing,
      reason: missingReason,
      run: () => void open('folder'),
    },
    {
      id: 'rerun',
      label: 'Rerun summary',
      icon: <SparkIcon size={14} />,
      disabled: missing,
      reason: missingReason,
      run: () => void askRerun(),
    },
    {
      id: 'archive',
      label: 'Archive',
      icon: <ArchiveIcon size={14} />,
      disabled: processing || busy,
      reason: processingReason,
      run: () => void archive(),
    },
    {
      id: 'trash',
      label: armed ? 'Confirm trash' : 'Trash',
      icon: <TrashIcon size={14} />,
      disabled: processing || busy,
      reason: processingReason,
      danger: true,
      armed,
      keepOpen: !armed,
      run: () => void trash(),
    },
  ];

  return (
    <div className={styles.actions}>
      <div className={styles.toolbar}>
        {(['markdown', 'rich'] as const).map((format) => {
          const done = copied?.format === format;
          return (
            <button
              key={format}
              type="button"
              // Not `disabled`: that would drop focus from the button just pressed.
              aria-disabled={copying || undefined}
              onClick={() => void copy(format)}
            >
              {done ? <CheckIcon size={14} /> : <CopyIcon size={14} />}
              {/* The hidden label keeps the width, so "Copied" does not shift the buttons after it. */}
              <span className={styles.label}>
                <span>{done ? COPIED : COPY_LABELS[format]}</span>
                <span aria-hidden="true">{COPY_LABELS[format]}</span>
              </span>
            </button>
          );
        })}
        <button type="button" onClick={() => open('obsidian')}>
          <VaultIcon size={14} />
          Open in Obsidian
        </button>
        <MoreMenu trigger={more} items={items} />
      </div>
      {/* Always mounted, so the change to "Copied" is announced without moving focus. */}
      <div role="status" className={styles.visuallyHidden}>
        {copied ? COPIED : null}
      </div>
      {asking ? (
        <RerunDialog
          onCancel={closeDialog}
          onChoose={(replaceEdits) => {
            closeDialog();
            void rerun(replaceEdits);
          }}
        />
      ) : null}
    </div>
  );
}

/**
 * The WAI-ARIA menu button pattern: Enter, Space or the arrows open it, the arrows (wrapping),
 * Home and End move, Escape closes it and returns focus to More, Tab or a click outside closes it.
 */
function MoreMenu({ trigger, items }: { trigger: RefObject<HTMLButtonElement | null>; items: MenuItem[] }) {
  const [open, setOpen] = useState(false);
  const [active, setActive] = useState(0);
  const base = useId();
  const wrapper = useRef<HTMLDivElement>(null);
  const menu = useRef<HTMLDivElement>(null);
  const refs = useRef<(HTMLButtonElement | null)[]>([]);

  useEffect(() => {
    if (open) refs.current[active]?.focus();
  }, [open, active]);

  useEffect(() => {
    if (!open) return;
    // A click outside, or focus leaving the items (Tab, or Shift+Tab onto More), closes it.
    // Not `focusout`: WebKit blurs to nothing on a click on a button, which would close the
    // menu before the click lands.
    const onClick = (e: Event) => {
      if (!wrapper.current?.contains(e.target as Node)) setOpen(false);
    };
    const onFocus = (e: Event) => {
      if (!menu.current?.contains(e.target as Node)) setOpen(false);
    };
    document.addEventListener('mousedown', onClick);
    document.addEventListener('focusin', onFocus);
    return () => {
      document.removeEventListener('mousedown', onClick);
      document.removeEventListener('focusin', onFocus);
    };
  }, [open]);

  const show = (at: number) => {
    setActive(at);
    setOpen(true);
  };

  const close = () => {
    setOpen(false);
    trigger.current?.focus();
  };

  const onTriggerKey = (e: KeyboardEvent) => {
    const at: Record<string, number> = { ArrowDown: 0, ArrowUp: items.length - 1 };
    if (!(e.key in at)) return;
    e.preventDefault();
    show(at[e.key]);
  };

  const onMenuKey = (e: KeyboardEvent) => {
    if (e.key === 'Escape') {
      e.preventDefault();
      close();
      return;
    }
    const next: Record<string, number> = {
      ArrowDown: (active + 1) % items.length,
      ArrowUp: (active - 1 + items.length) % items.length,
      Home: 0,
      End: items.length - 1,
    };
    if (!(e.key in next)) return;
    e.preventDefault();
    setActive(next[e.key]);
  };

  const choose = (item: MenuItem) => {
    if (item.disabled) return;
    if (!item.keepOpen) close();
    item.run();
  };

  const menuId = `${base}-menu`;
  return (
    <div ref={wrapper} className={styles.more}>
      <button
        ref={trigger}
        type="button"
        id={`${base}-trigger`}
        aria-haspopup="menu"
        aria-expanded={open}
        aria-controls={menuId}
        // Keeps focus on the item until the click closes the menu (else focusin closes it first).
        onMouseDown={(e) => open && e.preventDefault()}
        onClick={() => (open ? close() : show(0))}
        onKeyDown={onTriggerKey}
      >
        More
      </button>
      {open ? (
        <div
          ref={menu}
          role="menu"
          id={menuId}
          aria-labelledby={`${base}-trigger`}
          tabIndex={-1}
          className={styles.menu}
          onKeyDown={onMenuKey}
        >
          {items.map((item, i) => (
            <button
              key={item.id}
              ref={(el) => {
                refs.current[i] = el;
              }}
              type="button"
              role="menuitem"
              tabIndex={i === active ? 0 : -1}
              aria-disabled={item.disabled || undefined}
              aria-labelledby={`${base}-${item.id}`}
              aria-describedby={item.disabled && item.reason ? `${base}-${item.id}-reason` : undefined}
              data-variant={item.danger ? 'danger' : undefined}
              data-armed={item.armed}
              className={styles.item}
              onFocus={() => setActive(i)}
              onClick={() => choose(item)}
            >
              {item.icon}
              <span className={styles.text}>
                <span id={`${base}-${item.id}`}>{item.label}</span>
                {item.disabled && item.reason ? (
                  <span id={`${base}-${item.id}-reason`} className={styles.reason}>
                    {item.reason}
                  </span>
                ) : null}
              </span>
            </button>
          ))}
        </div>
      ) : null}
    </div>
  );
}
