import { useState } from 'react';
import { JobsTab } from './JobsTab';
import styles from './Popup.module.css';
import { RecordTab } from './RecordTab';

type TabId = 'record' | 'jobs';

const TABS: { id: TabId; label: string }[] = [
  { id: 'record', label: 'Record' },
  { id: 'jobs', label: 'Jobs' },
];

export function Popup() {
  const [selected, setSelected] = useState<TabId>('record');

  return (
    <main className={styles.popup}>
      <div role="tablist" className={styles.tablist}>
        {TABS.map(({ id, label }) => (
          <button
            key={id}
            type="button"
            role="tab"
            id={`tab-${id}`}
            aria-selected={selected === id}
            aria-controls={`panel-${id}`}
            className={styles.tab}
            onClick={() => setSelected(id)}
          >
            {label}
          </button>
        ))}
      </div>
      <div role="tabpanel" id={`panel-${selected}`} aria-labelledby={`tab-${selected}`} className={styles.panel}>
        {selected === 'record' ? <RecordTab /> : <JobsTab />}
      </div>
    </main>
  );
}
