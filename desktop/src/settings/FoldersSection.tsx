import styles from './Settings.module.css';
import type { InfoView } from './useSettingsInfo';

const joinPath = (dir: string, folder: string) => (folder === '' ? dir : `${dir.replace(/\/+$/, '')}/${folder}`);

/** FR-20: where recordings and notes go, read-only; `meeting install` changes them. */
export function FoldersSection({ view }: { view: InfoView }) {
  return (
    <section aria-labelledby="settings-folders" className={styles.section}>
      <h3 id="settings-folders">Folders</h3>
      {view.kind === 'loading' && <p>Loading folders…</p>}
      {view.kind === 'failed' && <p className={styles.problem}>{view.message}</p>}
      {view.kind === 'info' && (
        <>
          <dl className={styles.folders}>
            <dt>Recordings directory</dt>
            <dd>{view.info.recordings_dir}</dd>
            <dt>Vault folder</dt>
            <dd>{joinPath(view.info.vault_dir, view.info.vault_folder)}</dd>
          </dl>
          <p className={styles.muted}>
            Change with <code>meeting install</code>
          </p>
        </>
      )}
    </section>
  );
}
