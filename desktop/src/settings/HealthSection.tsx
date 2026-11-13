import { useCallback, useEffect, useRef, useState } from 'react';
import { type HealthCheck, health } from '../api';
import { attempt } from '../recording/attempt';
import styles from './Settings.module.css';

type View = { kind: 'loading' } | { kind: 'failed'; message: string } | { kind: 'checks'; checks: HealthCheck[] };

/** FR-20: the `meeting` tool's health checks (`meeting doctor`, D38), each failing one with its fix. */
export function HealthSection() {
  const [view, setView] = useState<View>({ kind: 'loading' });
  const [checking, setChecking] = useState(true);
  const active = useRef(true);

  const check = useCallback(async () => {
    setChecking(true);
    const reply = await attempt(health);
    if (!active.current) return;
    setChecking(false);
    setView(reply.ok ? { kind: 'checks', checks: reply.reply.checks } : { kind: 'failed', message: reply.text });
  }, []);

  useEffect(() => {
    active.current = true;
    check();
    return () => {
      active.current = false;
    };
  }, [check]);

  return (
    <section aria-labelledby="settings-health" className={styles.section}>
      <h3 id="settings-health">Health</h3>
      {view.kind === 'loading' && <p>Checking…</p>}
      {view.kind === 'failed' && <p className={styles.problem}>{view.message}</p>}
      {view.kind === 'checks' && view.checks.length === 0 && <p>No checks reported</p>}
      {view.kind === 'checks' && view.checks.length > 0 && (
        <ul aria-label="Health checks" className={styles.plain}>
          {view.checks.map((item) => (
            <li key={item.name} className={styles.check}>
              <span className={styles.checkName}>{item.name}</span>{' '}
              <span className={item.ok ? undefined : styles.problem}>{item.ok ? 'OK' : 'Failing'}</span>
              {!item.ok && item.fix && <span className={styles.fix}> {item.fix}</span>}
            </li>
          ))}
        </ul>
      )}
      <button type="button" disabled={checking} onClick={check}>
        Check again
      </button>
    </section>
  );
}
