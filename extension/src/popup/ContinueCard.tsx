import type { LastRecording } from '../shared/lastRecording';
import type { StopReason } from '../shared/recordingState';
import styles from './ContinueCard.module.css';
import { formatElapsed } from './formatElapsed';

/** Only stops the user did not make get a reason; `cap` never shows, the rule refuses a 3 h total. */
const STOP_REASONS: Partial<Record<StopReason, string>> = {
  tab_closed: 'tab closed',
  capture_ended: 'capture ended',
};

interface Props {
  last: LastRecording;
  now: number;
  onContinue: () => void;
}

export function ContinueCard({ last, now, onContinue }: Props) {
  const minutes = Math.max(0, Math.floor((now - last.stopped_at) / 60_000));
  const reason = last.stop_reason ? STOP_REASONS[last.stop_reason] : undefined;
  const stopped = `stopped ${minutes} min ago${reason ? ` · ${reason}` : ''}`;

  return (
    <section aria-labelledby="continue-title" className={styles.card}>
      <div className={styles.text}>
        <p id="continue-title" className={styles.title}>
          Continue last recording
        </p>
        <p className={styles.folder} title={last.folder}>
          {last.folder}
        </p>
        <p className={styles.meta}>
          <span className={styles.time}>{formatElapsed(last.recorded_ms)}</span>
          <span>{stopped}</span>
        </p>
      </div>
      <button type="button" className={styles.continue} onClick={onContinue}>
        Continue
      </button>
    </section>
  );
}
