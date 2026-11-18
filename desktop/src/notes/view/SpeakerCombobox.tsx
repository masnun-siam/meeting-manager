import { type KeyboardEvent, useEffect, useId, useRef, useState } from 'react';
import styles from './NoteView.module.css';

export type Suggestions = { attendees: string[]; earlier: string[] };

export const NO_SUGGESTIONS: Suggestions = { attendees: [], earlier: [] };

type Group = { id: 'attendees' | 'earlier'; title: string; names: string[] };

/** The names in each group that contain `text`, ignoring case; empty groups are left out. */
export function filterSuggestions(suggestions: Suggestions, text: string): Group[] {
  const needle = text.trim().toLowerCase();
  const keep = (names: string[]) => names.filter((name) => name.toLowerCase().includes(needle));
  const groups: Group[] = [
    { id: 'attendees', title: 'Attendees', names: keep(suggestions.attendees) },
    { id: 'earlier', title: 'Used before', names: keep(suggestions.earlier) },
  ];
  return groups.filter((group) => group.names.length > 0);
}

/**
 * A speaker's rename field (FR-4): an ARIA combobox with a listbox popup and list autocomplete.
 * Typing stays free text. Focusing or typing opens the suggestions in two labelled groups,
 * filtered by what was typed since focus (the stored name shown on focus does not filter).
 * ArrowDown/ArrowUp move the highlight (no wrap), Enter or a click picks, Escape closes the list
 * and keeps the text. With no matching suggestion no list is shown.
 */
export function SpeakerCombobox({
  label,
  value,
  suggestions,
  onChange,
  onFocus,
  onBlur,
  onPick,
}: {
  label: string;
  value: string;
  suggestions: Suggestions;
  onChange: (value: string) => void;
  onFocus: () => void;
  onBlur: () => void;
  onPick: (name: string) => void;
}) {
  const base = useId();
  const inputId = `${base}-input`;
  const listId = `${base}-list`;
  const [open, setOpen] = useState(false);
  // Whether the user typed since focus: until then every suggestion is shown.
  const [typed, setTyped] = useState(false);
  const [active, setActive] = useState(-1);
  const options = useRef(new Map<number, HTMLDivElement>());

  const groups = filterSuggestions(suggestions, typed ? value : '');
  const names = groups.flatMap((group) => group.names);
  const shown = open && names.length > 0;
  const optionId = (index: number) => `${base}-option-${index}`;

  useEffect(() => {
    if (shown && active >= 0) options.current.get(active)?.scrollIntoView?.({ block: 'nearest' });
  }, [shown, active]);

  const close = () => {
    setOpen(false);
    setActive(-1);
  };

  const pick = (name: string) => {
    close();
    setTyped(false);
    onPick(name);
  };

  const onKeyDown = (e: KeyboardEvent<HTMLInputElement>) => {
    if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
      if (names.length === 0) return;
      e.preventDefault();
      if (!open) {
        setOpen(true);
        setActive(e.key === 'ArrowDown' ? 0 : names.length - 1);
      } else if (e.key === 'ArrowDown') setActive((i) => Math.min(i + 1, names.length - 1));
      else setActive((i) => (i < 0 ? i : Math.max(i - 1, 0)));
    } else if (e.key === 'Enter') {
      if (!shown || active < 0 || active >= names.length) return;
      e.preventDefault();
      pick(names[active]);
    } else if (e.key === 'Escape') {
      if (!shown) return;
      // Only the list closes: nothing else on the page reacts to this Escape.
      e.preventDefault();
      e.stopPropagation();
      close();
    }
  };

  let index = 0;
  return (
    <div className={`${styles.field} ${styles.combobox}`}>
      <label htmlFor={inputId} className="sr-only">
        Name for {label}
      </label>
      <input
        id={inputId}
        type="text"
        role="combobox"
        autoComplete="off"
        aria-autocomplete="list"
        aria-expanded={shown}
        aria-controls={listId}
        aria-activedescendant={shown && active >= 0 ? optionId(active) : undefined}
        value={value}
        onChange={(e) => {
          setTyped(true);
          setOpen(true);
          setActive(-1);
          onChange(e.target.value);
        }}
        onFocus={() => {
          setTyped(false);
          setOpen(true);
          setActive(-1);
          onFocus();
        }}
        onBlur={() => {
          close();
          onBlur();
        }}
        onClick={() => setOpen(true)}
        onKeyDown={onKeyDown}
      />
      {shown ? (
        <div id={listId} role="listbox" aria-label={`Names for ${label}`} className={styles.suggestions}>
          {groups.map((group) => {
            const headingId = `${base}-${group.id}`;
            return (
              // biome-ignore lint/a11y/useSemanticElements: a <fieldset>'s implicit group does not own role="option" children in Chromium's accessibility tree (they read as plain text); only an explicit role="group" exposes them (#206).
              <div key={group.id} role="group" aria-labelledby={headingId} className={styles.suggestionGroup}>
                <div role="presentation" id={headingId} className={styles.suggestionTitle}>
                  {group.title}
                </div>
                {group.names.map((name) => {
                  const i = index++;
                  return (
                    <div
                      key={i}
                      ref={(el) => {
                        if (el) options.current.set(i, el);
                        else options.current.delete(i);
                      }}
                      id={optionId(i)}
                      role="option"
                      // Never focused: the field keeps focus and points here with aria-activedescendant.
                      tabIndex={-1}
                      aria-selected={i === active}
                      className={styles.suggestion}
                      // Picks on press and keeps focus in the field, so the typed text is not
                      // blur-saved first.
                      onMouseDown={(e) => {
                        e.preventDefault();
                        pick(name);
                      }}
                      onMouseMove={() => setActive(i)}
                    >
                      {name}
                    </div>
                  );
                })}
              </div>
            );
          })}
        </div>
      ) : null}
    </div>
  );
}
