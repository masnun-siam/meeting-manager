import { useEffect, useState } from 'react';
import { indicatorSettings, setIndicatorEnabled } from '../api';
import { attempt } from '../recording/attempt';
import { type Saved, SettingCheckbox } from './SettingCheckbox';
import styles from './Settings.module.css';

async function saveIndicator(enabled: boolean): Promise<Saved> {
  const reply = await attempt(() => setIndicatorEnabled({ enabled }));
  return reply.ok ? { ok: true, value: reply.reply.enabled } : reply;
}

/** Quick-record FR-10: Show floating indicator while recording, on by default; applies from the next start. */
export function RecordingSection() {
  const [enabled, setEnabled] = useState<boolean | null>(null);
  const [problem, setProblem] = useState<string | null>(null);

  useEffect(() => {
    let active = true;
    attempt(indicatorSettings).then((reply) => {
      if (!active) return;
      if (reply.ok) {
        setEnabled(reply.reply.enabled);
        return;
      }
      setProblem(reply.text);
      // The contract answers `store_unavailable` with the default, on; a rejected call leaves the box disabled.
      if (reply.code === 'store_unavailable') setEnabled(true);
    });
    return () => {
      active = false;
    };
  }, []);

  return (
    <section aria-labelledby="settings-recording" className={styles.section}>
      <h3 id="settings-recording">Recording</h3>
      <SettingCheckbox
        label="Show floating indicator while recording"
        initial={enabled}
        initialProblem={problem}
        save={saveIndicator}
      />
    </section>
  );
}
