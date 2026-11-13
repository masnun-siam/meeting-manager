import styles from './NotesList.module.css';

// `onOpen` opens a note from the list; nothing calls it until the list has rows.
export function NotesList(_props: { onOpen: (id: number) => void }) {
  return (
    <section className={styles.list}>
      <h2>All notes</h2>
    </section>
  );
}
