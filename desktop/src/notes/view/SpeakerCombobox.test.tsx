import { render, screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { useState } from 'react';
import { describe, expect, it, vi } from 'vitest';
import { filterSuggestions, NO_SUGGESTIONS, SpeakerCombobox, type Suggestions } from './SpeakerCombobox';

const SUGGESTIONS: Suggestions = { attendees: ['Rahim', 'Nadia'], earlier: ['Rahnat', 'Ayesha'] };

type Spies = { onPick: (name: string) => void; onBlur: () => void; onFocus: () => void };

/** The combobox with its value held in state, as `SpeakersPanel` holds it in the autosave draft. */
function Harness({
  suggestions,
  initial,
  onPick,
  onBlur,
  onFocus,
}: Spies & { suggestions: Suggestions; initial: string }) {
  const [value, setValue] = useState(initial);
  return (
    <>
      <SpeakerCombobox
        label="Speaker 1"
        value={value}
        suggestions={suggestions}
        onChange={setValue}
        onFocus={onFocus}
        onBlur={onBlur}
        onPick={(name) => {
          setValue(name);
          onPick(name);
        }}
      />
      <button type="button">elsewhere</button>
    </>
  );
}

function setup({ suggestions = SUGGESTIONS, initial = '' }: { suggestions?: Suggestions; initial?: string } = {}) {
  const onPick = vi.fn();
  const onBlur = vi.fn();
  const onFocus = vi.fn();
  const user = userEvent.setup();
  render(<Harness suggestions={suggestions} initial={initial} onPick={onPick} onBlur={onBlur} onFocus={onFocus} />);
  const field = screen.getByRole('combobox', { name: 'Name for Speaker 1' });
  return { user, field, onPick, onBlur, onFocus };
}

const groupNames = () =>
  screen.getAllByRole('group').map((group) => ({
    title: document.getElementById(group.getAttribute('aria-labelledby') ?? '')?.textContent,
    names: within(group)
      .getAllByRole('option')
      .map((option) => option.textContent),
  }));

describe('filterSuggestions', () => {
  it('matches anywhere, ignoring case, keeps the order and drops empty groups', () => {
    expect(filterSuggestions(SUGGESTIONS, 'NA')).toEqual([
      { id: 'attendees', title: 'Attendees', names: ['Nadia'] },
      { id: 'earlier', title: 'Used before', names: ['Rahnat'] },
    ]);
    expect(filterSuggestions(SUGGESTIONS, 'yes')).toEqual([{ id: 'earlier', title: 'Used before', names: ['Ayesha'] }]);
    expect(filterSuggestions(SUGGESTIONS, '')).toHaveLength(2);
    expect(filterSuggestions(SUGGESTIONS, 'zz')).toEqual([]);
  });
});

describe('SpeakerCombobox', () => {
  it('is a combobox with list autocomplete and no list before focus', () => {
    const { field } = setup();
    expect(field).toHaveAttribute('aria-autocomplete', 'list');
    expect(field).toHaveAttribute('aria-expanded', 'false');
    expect(field).toHaveAttribute('aria-controls');
    expect(screen.queryByRole('listbox')).not.toBeInTheDocument();
  });

  it('focus opens both labelled groups in order, unfiltered by the stored name', async () => {
    const { user, field, onFocus } = setup({ initial: 'Rahim' });
    await user.click(field);
    expect(onFocus).toHaveBeenCalled();
    expect(field).toHaveAttribute('aria-expanded', 'true');
    const listbox = screen.getByRole('listbox');
    expect(field).toHaveAttribute('aria-controls', listbox.id);
    expect(within(listbox).getByRole('group', { name: 'Attendees' })).toBeInTheDocument();
    expect(within(listbox).getByRole('group', { name: 'Used before' })).toBeInTheDocument();
    // An explicit role: Chromium does not expose options under a <fieldset>'s implicit group.
    for (const group of within(listbox).getAllByRole('group')) {
      expect(group).toHaveAttribute('role', 'group');
      expect(group.tagName).toBe('DIV');
    }
    expect(groupNames()).toEqual([
      { title: 'Attendees', names: ['Rahim', 'Nadia'] },
      { title: 'Used before', names: ['Rahnat', 'Ayesha'] },
    ]);
  });

  it('typing filters case-insensitively anywhere in the name and leaves out an empty group', async () => {
    const { user, field } = setup();
    await user.type(field, 'nA');
    expect(groupNames()).toEqual([
      { title: 'Attendees', names: ['Nadia'] },
      { title: 'Used before', names: ['Rahnat'] },
    ]);
    await user.clear(field);
    await user.type(field, 'ayes');
    expect(screen.queryByRole('group', { name: 'Attendees' })).not.toBeInTheDocument();
    expect(groupNames()).toEqual([{ title: 'Used before', names: ['Ayesha'] }]);
  });

  it('arrows move the highlight without wrapping and Enter picks it', async () => {
    const { user, field, onPick } = setup();
    await user.click(field);
    expect(field).not.toHaveAttribute('aria-activedescendant');

    await user.keyboard('{ArrowUp}');
    expect(field).not.toHaveAttribute('aria-activedescendant');
    await user.keyboard('{ArrowDown}');
    const first = screen.getByRole('option', { name: 'Rahim' });
    expect(first).toHaveAttribute('aria-selected', 'true');
    expect(field).toHaveAttribute('aria-activedescendant', first.id);
    await user.keyboard('{ArrowUp}');
    expect(first).toHaveAttribute('aria-selected', 'true');

    await user.keyboard('{ArrowDown}{ArrowDown}{ArrowDown}{ArrowDown}{ArrowDown}');
    const last = screen.getByRole('option', { name: 'Ayesha' });
    expect(last).toHaveAttribute('aria-selected', 'true');
    expect(first).toHaveAttribute('aria-selected', 'false');
    await user.keyboard('{ArrowUp}{Enter}');

    expect(onPick).toHaveBeenCalledExactlyOnceWith('Rahnat');
    expect(field).toHaveValue('Rahnat');
    expect(field).toHaveFocus();
    expect(screen.queryByRole('listbox')).not.toBeInTheDocument();
    expect(field).toHaveAttribute('aria-expanded', 'false');
  });

  it('a click picks without blurring the field first', async () => {
    const { user, field, onPick, onBlur } = setup();
    await user.type(field, 'Nad');
    await user.click(screen.getByRole('option', { name: 'Nadia' }));
    expect(onPick).toHaveBeenCalledExactlyOnceWith('Nadia');
    expect(onBlur).not.toHaveBeenCalled();
    expect(field).toHaveFocus();
    expect(screen.queryByRole('listbox')).not.toBeInTheDocument();
  });

  it('Enter with nothing highlighted picks nothing and typing goes on', async () => {
    const { user, field, onPick } = setup();
    await user.type(field, 'Ra{Enter}h');
    expect(onPick).not.toHaveBeenCalled();
    expect(field).toHaveValue('Rah');
    expect(screen.getByRole('listbox')).toBeInTheDocument();
  });

  it('Escape closes the list, keeps the text and stops there', async () => {
    const onKey = vi.fn();
    document.addEventListener('keydown', onKey);
    const { user, field, onPick } = setup();
    await user.type(field, 'Ra{ArrowDown}{Escape}');
    document.removeEventListener('keydown', onKey);

    expect(screen.queryByRole('listbox')).not.toBeInTheDocument();
    expect(field).toHaveValue('Ra');
    expect(field).toHaveFocus();
    expect(onPick).not.toHaveBeenCalled();
    expect(onKey.mock.calls.some(([e]) => (e as KeyboardEvent).key === 'Escape')).toBe(false);

    // Typing again reopens it.
    await user.type(field, 'h');
    expect(screen.getByRole('listbox')).toBeInTheDocument();
  });

  it('free text is kept and leaving the field calls onBlur and closes the list', async () => {
    const { user, field, onBlur, onPick } = setup();
    await user.type(field, 'Someone New');
    expect(screen.queryByRole('listbox')).not.toBeInTheDocument();
    await user.tab();
    expect(field).toHaveValue('Someone New');
    expect(onBlur).toHaveBeenCalledOnce();
    expect(onPick).not.toHaveBeenCalled();
  });

  it('with no suggestions never opens a list', async () => {
    const { user, field } = setup({ suggestions: NO_SUGGESTIONS });
    await user.click(field);
    await user.type(field, 'Ra{ArrowDown}{Enter}');
    expect(screen.queryByRole('listbox')).not.toBeInTheDocument();
    expect(field).toHaveAttribute('aria-expanded', 'false');
    expect(field).not.toHaveAttribute('aria-activedescendant');
    expect(field).toHaveValue('Ra');
  });

  it('renders names as text', async () => {
    const { user, field } = setup({ suggestions: { attendees: ['<b>Bold</b>'], earlier: [] } });
    await user.click(field);
    const option = screen.getByRole('option', { name: '<b>Bold</b>' });
    expect(option.querySelector('b')).toBeNull();
  });
});
