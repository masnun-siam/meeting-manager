import { useEffect, useState } from 'react';
import { type Note, speakerSuggestions } from '../../api';
import { attempt } from '../../recording/attempt';
import { shownName } from './format';
import styles from './NoteView.module.css';
import { NO_SUGGESTIONS, SpeakerCombobox, type Suggestions } from './SpeakerCombobox';
import type { Autosave } from './useAutosave';

/**
 * The rename suggestions for note `id` (FR-4), fetched once while `enabled`. A failed call counts
 * as no suggestions, with no message (D14); a reply arriving after the view left is ignored.
 */
export function useSpeakerSuggestions(id: number, enabled: boolean): Suggestions {
  const [suggestions, setSuggestions] = useState<Suggestions>(NO_SUGGESTIONS);
  useEffect(() => {
    if (!enabled) return;
    let active = true;
    void attempt(() => speakerSuggestions({ id })).then((result) => {
      if (!active || !result.ok) return;
      setSuggestions({ attendees: result.reply.attendees, earlier: result.reply.earlier });
    });
    return () => {
      active = false;
    };
  }, [id, enabled]);
  return enabled ? suggestions : NO_SUGGESTIONS;
}

/** Each speaker's label, shown name and evidence, with a rename combobox for a `done` note. */
export function SpeakersPanel({ note, autosave, editable }: { note: Note; autosave: Autosave; editable: boolean }) {
  const suggestions = useSpeakerSuggestions(note.id, editable && note.speakers.length > 0);
  if (note.speakers.length === 0) return null;
  return (
    <section aria-labelledby="note-speakers" className={`${styles.section} ${styles.card}`}>
      <h3 id="note-speakers" className={styles.cardTitle}>
        Speakers{' '}
        <span className={styles.count} aria-hidden="true">
          {note.speakers.length}
        </span>
      </h3>
      <ul className={styles.speakers}>
        {note.speakers.map((speaker) => {
          const key = `speaker:${speaker.label}` as const;
          return (
            <li key={speaker.label} className={styles.speaker}>
              <span className={styles.label}>{speaker.label}</span>
              {editable ? (
                <SpeakerCombobox
                  label={speaker.label}
                  value={autosave.value(key)}
                  suggestions={suggestions}
                  onChange={(value) => autosave.change(key, value)}
                  onFocus={() => autosave.focus(key)}
                  onBlur={() => autosave.blur(key)}
                  onPick={(name) => {
                    // A pick saves at once, as a typed rename would on leaving the field.
                    autosave.change(key, name);
                    void autosave.flushAll();
                  }}
                />
              ) : null}
              <span className={styles.detail} title={speaker.evidence || undefined}>
                <span className={styles.shown}>{shownName(speaker)}</span>
                {speaker.evidence ? <span className={styles.evidence}>{speaker.evidence}</span> : null}
              </span>
            </li>
          );
        })}
      </ul>
    </section>
  );
}
