import styles from './NoteView.module.css';

export function NoteView({ id, onBack }: { id: number; onBack: () => void }) {
  return (
    <section className={styles.note}>
      <button type="button" onClick={onBack}>
        Back
      </button>
      <h2>Note {id}</h2>
    </section>
  );
}
