import { useState } from 'react';
import styles from './App.module.css';
import { Upcoming } from './calendar/Upcoming';
import { NotesList } from './notes/list/NotesList';
import { NoteView } from './notes/view/NoteView';
import { RecordingBar } from './recording/RecordingBar';
import { Settings } from './settings/Settings';

type View = 'upcoming' | 'notes' | 'settings';

const VIEWS: { id: View; label: string }[] = [
  { id: 'upcoming', label: 'Upcoming' },
  { id: 'notes', label: 'All notes' },
  { id: 'settings', label: 'Settings' },
];

export function App() {
  const [view, setView] = useState<View>('upcoming');
  const [noteId, setNoteId] = useState<number | null>(null);

  const openNote = (id: number) => {
    setNoteId(id);
    setView('notes');
  };

  return (
    <div className={styles.app}>
      <RecordingBar />
      <div className={styles.body}>
        <nav aria-label="Main" className={styles.sidebar}>
          {VIEWS.map(({ id, label }) => (
            <button
              key={id}
              type="button"
              className={styles.entry}
              aria-current={view === id ? 'page' : undefined}
              onClick={() => setView(id)}
            >
              {label}
            </button>
          ))}
        </nav>
        <main className={styles.view}>
          {view === 'upcoming' && <Upcoming onOpenNote={openNote} />}
          {view === 'notes' &&
            (noteId === null ? (
              <NotesList onOpen={setNoteId} />
            ) : (
              <NoteView id={noteId} onBack={() => setNoteId(null)} />
            ))}
          {view === 'settings' && <Settings />}
        </main>
      </div>
    </div>
  );
}
