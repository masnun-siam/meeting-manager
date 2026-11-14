import { type ReactNode, useState } from 'react';
import styles from './App.module.css';
import { Upcoming } from './calendar/Upcoming';
import { AppMark, CalendarIcon, NotesIcon, SettingsIcon } from './icons';
import { NotesList } from './notes/list/NotesList';
import { NoteView } from './notes/view/NoteView';
import { RecordingBar } from './recording/RecordingBar';
import { Settings } from './settings/Settings';

type View = 'upcoming' | 'notes' | 'settings';

const VIEWS: { id: View; label: string; icon: ReactNode }[] = [
  { id: 'upcoming', label: 'Upcoming', icon: <CalendarIcon /> },
  { id: 'notes', label: 'All notes', icon: <NotesIcon /> },
  { id: 'settings', label: 'Settings', icon: <SettingsIcon /> },
];

export function App() {
  const [view, setView] = useState<View>('upcoming');
  const [noteId, setNoteId] = useState<number | null>(null);

  const openNote = (id: number) => {
    setNoteId(id);
    setView('notes');
  };

  // All notes while it is already shown goes back to the list, as a sidebar does in a Mac app.
  const go = (id: View) => {
    if (id === 'notes' && view === 'notes') setNoteId(null);
    setView(id);
  };

  return (
    <div className={styles.app}>
      <aside className={styles.sidebar}>
        <div className={styles.brand} aria-hidden="true">
          <AppMark />
          <span>Meeting</span>
        </div>
        <nav aria-label="Main" className={styles.nav}>
          {VIEWS.map(({ id, label, icon }) => (
            <button
              key={id}
              type="button"
              className={styles.entry}
              aria-current={view === id ? 'page' : undefined}
              onClick={() => go(id)}
            >
              {icon}
              {label}
            </button>
          ))}
        </nav>
      </aside>
      <div className={styles.body}>
        <RecordingBar />
        <main className={styles.view}>
          <div className={styles.content}>
            {view === 'upcoming' && <Upcoming onOpenNote={openNote} />}
            {view === 'notes' &&
              (noteId === null ? (
                <NotesList onOpen={setNoteId} />
              ) : (
                <NoteView id={noteId} onBack={() => setNoteId(null)} />
              ))}
            {view === 'settings' && <Settings />}
          </div>
        </main>
      </div>
    </div>
  );
}
