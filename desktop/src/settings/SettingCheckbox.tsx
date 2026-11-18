import { useEffect, useRef, useState } from 'react';
import styles from './Settings.module.css';

/** A save's outcome: the value the app confirmed, or the text the user sees. */
export type Saved = { ok: true; value: boolean } | { ok: false; text: string };

type Props = {
  label: string;
  /** The value the app last reported; null until it has, which keeps the box disabled. */
  initial: boolean | null;
  /** Shown in the status line until the first toggle here, e.g. why the value could not be read. */
  initialProblem?: string | null;
  save: (enabled: boolean) => Promise<Saved>;
};

/**
 * A saved on/off setting: shows a toggle at once, saves the latest one, and on a failed save returns to the last value
 * the app confirmed with the reason in a `role="status"` line. Once toggled here, the box shows this view's value.
 */
export function SettingCheckbox({ label, initial, initialProblem = null, save }: Props) {
  // Null until toggled here: until then the box follows `initial`.
  const [own, setOwn] = useState<boolean | null>(null);
  // Undefined until toggled here: until then the line shows `initialProblem`.
  const [problem, setProblem] = useState<string | null | undefined>(undefined);
  const wanted = useRef<boolean | null>(null);
  const confirmed = useRef<boolean | null>(null);
  const queue = useRef<Promise<void>>(Promise.resolve());
  const latest = useRef(0);
  const active = useRef(true);

  useEffect(() => {
    active.current = true;
    return () => {
      active.current = false;
    };
  }, []);

  const show = (value: boolean) => {
    wanted.current = value;
    setOwn(value);
  };

  function toggle() {
    const next = !(wanted.current ?? initial);
    show(next);
    setProblem(null);
    const request = ++latest.current;
    queue.current = queue.current.then(async () => {
      const reply = await save(next);
      if (!active.current) return;
      if (reply.ok) confirmed.current = reply.value;
      if (request !== latest.current) return;
      if (reply.ok) {
        show(reply.value);
      } else {
        show(confirmed.current ?? initial === true);
        setProblem(reply.text);
      }
    });
  }

  return (
    <>
      <label className={styles.toggle}>
        <input type="checkbox" checked={own ?? initial === true} disabled={initial === null} onChange={toggle} />{' '}
        {label}
      </label>
      <p role="status" className={styles.problem}>
        {problem === undefined ? initialProblem : problem}
      </p>
    </>
  );
}
