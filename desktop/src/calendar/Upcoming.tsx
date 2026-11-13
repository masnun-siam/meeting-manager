import styles from './Upcoming.module.css';

// `onOpenNote` opens one of an event's earlier notes; nothing calls it until the event view exists.
export function Upcoming(_props: { onOpenNote: (id: number) => void }) {
  return (
    <section className={styles.upcoming}>
      <h2>Upcoming</h2>
    </section>
  );
}
