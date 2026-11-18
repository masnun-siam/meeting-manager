import { CalendarsSection } from './CalendarsSection';
import { FoldersSection } from './FoldersSection';
import { HealthSection } from './HealthSection';
import { RecordingSection } from './RecordingSection';
import { RescanSection } from './RescanSection';
import styles from './Settings.module.css';
import { UpdatesSection } from './UpdatesSection';
import { useSettingsInfo } from './useSettingsInfo';

/** FR-20, plus quick-record FR-10's Recording section first. Each section loads and fails on its own, so one failing leaves the others usable. */
export function Settings() {
  const { view, reload } = useSettingsInfo();
  const store = view.kind === 'info' ? view.info.store : null;

  return (
    <section className={styles.settings}>
      <h2>Settings</h2>
      <RecordingSection />
      <CalendarsSection />
      <UpdatesSection />
      <RescanSection storeProblem={store?.ok === false ? store.message : null} onFinished={reload} />
      <FoldersSection view={view} />
      <HealthSection />
    </section>
  );
}
