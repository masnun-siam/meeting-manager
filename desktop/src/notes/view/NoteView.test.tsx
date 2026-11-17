import { act, render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { Note } from '../../api';
import * as api from '../../api';
import { ARCHIVED, CONFIRM_MS } from './Actions';
import { MY_NOTES_LIMIT, RECORDING_MISSING, TOO_LARGE, TOOL_MISSING } from './format';
import { NoteView, UNSAVED } from './NoteView';
import { Tabs } from './TranscriptTab';
import { deferred, testNote } from './testNote';
import { POLL_MS, RERUN_FOLLOW_MS } from './useNote';

vi.mock('../../api', async () => (await import('../../api.mock')).mockApi());

const noteGet = vi.mocked(api.noteGet);
const noteUpdate = vi.mocked(api.noteUpdate);
const noteRerun = vi.mocked(api.noteRerun);

const got = (note: Note) => ({ ok: true as const, note });

beforeEach(() => {
  vi.useFakeTimers({ shouldAdvanceTime: true });
  vi.resetAllMocks();
  noteGet.mockResolvedValue(got(testNote()));
  noteUpdate.mockImplementation(async (req) => ({
    ok: true,
    note: testNote({ ...req, id: 7 } as Partial<Note>),
    vault: 'ok',
    vault_error: null,
  }));
  vi.mocked(api.noteRerun).mockResolvedValue({ ok: true });
  vi.mocked(api.noteOpen).mockResolvedValue({ ok: true });
  vi.mocked(api.noteArchive).mockResolvedValue({ ok: true });
  vi.mocked(api.noteTrash).mockResolvedValue({ ok: true });
});

afterEach(() => {
  vi.restoreAllMocks();
  vi.useRealTimers();
});

function setup(note: Note | null = testNote()) {
  if (note) noteGet.mockResolvedValue(got(note));
  const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime });
  const onBack = vi.fn();
  const view = render(<NoteView id={7} onBack={onBack} />);
  return { user, onBack, ...view };
}

/** The note is shown and its heading has taken focus (so typing that follows is not blurred). */
async function ready(title = 'Weekly sync') {
  const heading = await screen.findByRole('heading', { level: 2, name: title });
  await waitFor(() => expect(heading).toHaveFocus());
  return heading;
}
const button = (name: string | RegExp) => screen.getByRole('button', { name });
const tick = (ms: number) => act(() => vi.advanceTimersByTimeAsync(ms));

describe('NoteView display', () => {
  it('shows Loading… until note_get replies', () => {
    noteGet.mockReturnValue(new Promise(() => {}));
    setup(null);

    expect(screen.getByText('Loading…')).toBeInTheDocument();
    expect(noteGet).toHaveBeenCalledWith({ id: 7 });
  });

  it('shows unknown_note with a Back button', async () => {
    noteGet.mockResolvedValue({ ok: false, code: 'unknown_note', message: 'No note 7' });
    const { user, onBack } = setup(null);

    expect(await screen.findByText('No note 7')).toBeInTheDocument();
    await waitFor(() => expect(button('Back')).toHaveFocus());
    await user.click(button('Back'));
    expect(onBack).toHaveBeenCalledOnce();
  });

  it('shows a rejected call as a message', async () => {
    noteGet.mockRejectedValue(new Error('ipc closed'));
    setup(null);

    expect(await screen.findByText('Could not reach the Meeting app: ipc closed')).toBeInTheDocument();
  });

  it('shows store_unavailable with its prefix', async () => {
    noteGet.mockResolvedValue({ ok: false, code: 'store_unavailable', message: 'disk I/O error' });
    setup(null);

    expect(await screen.findByText("Notes database can't be opened: disk I/O error")).toBeInTheDocument();
  });

  it('shows the metadata and the linked event, and no email address', async () => {
    const { container } = setup(
      testNote({
        language: 'bn',
        source: 'browser',
        event: {
          occurrence_id: 'o',
          series_id: 's',
          title: 'Design review',
          start: '2026-11-12T10:00:00+06:00',
          end: '2026-11-12T11:00:00+06:00',
          calendar: 'ayesha@example.com',
        },
      }),
    );
    await ready();

    const facts = within(container.querySelector('dl') as HTMLElement);
    expect(facts.getByText('Bengali')).toBeInTheDocument();
    expect(facts.getByText('Browser')).toBeInTheDocument();
    expect(facts.getByText('30:30')).toBeInTheDocument();
    expect(facts.getAllByText(/2026/, { selector: 'dd' })).toHaveLength(2);
    expect(facts.getByText(/Design review/)).toBeInTheDocument();
    expect(container.textContent).not.toContain('@');
  });

  it('shows the event calendar name', async () => {
    setup(
      testNote({
        event: {
          occurrence_id: 'o',
          series_id: 's',
          title: 'Design review',
          start: '2026-11-12T10:00:00+06:00',
          end: '2026-11-12T11:00:00+06:00',
          calendar: 'Work',
        },
      }),
    );
    expect(await screen.findByText(/Design review · .* · Work/)).toBeInTheDocument();
  });

  it('shows raw HTML in the summary as text and runs nothing', async () => {
    const alert = vi.spyOn(window, 'alert').mockImplementation(() => {});
    const { container } = setup(
      testNote({
        body_md: '<img src=x onerror=alert(1)>',
        body_html: '<p>&lt;img src=x onerror=alert(1)&gt;</p>\n',
      }),
    );
    await ready();

    expect(screen.getByText('<img src=x onerror=alert(1)>')).toBeInTheDocument();
    expect(container.querySelector('img')).toBeNull();
    expect(container.querySelector('[onerror]')).toBeNull();
    expect(alert).not.toHaveBeenCalled();
  });

  it('has exactly one HTML sink, the summary in Summary.tsx', () => {
    const sources = import.meta.glob<string>('./*.tsx', { query: '?raw', import: 'default', eager: true });
    const sinks = Object.entries(sources)
      .filter(([path]) => !path.endsWith('.test.tsx'))
      .flatMap(([path, text]) => (text.match(/dangerouslySetInnerHTML=/g) ?? []).map(() => path));
    expect(sinks).toEqual(['./Summary.tsx']);
    expect(sources['./Summary.tsx']).toMatch(/biome-ignore lint\/security\/noDangerouslySetInnerHtml: .*#143/);
    for (const [path, text] of Object.entries(sources)) {
      if (!path.endsWith('.test.tsx')) expect(text, path).not.toMatch(/innerHTML\s*=|eval\(|console\./);
    }
  });

  it('does not follow a link in the summary', async () => {
    const { user } = setup(testNote({ body_html: '<p><a href="https://example.com/deck">deck</a></p>' }));
    await ready();

    const link = screen.getByRole('link', { name: 'deck' });
    let followed = true;
    link.addEventListener('click', (e) => {
      followed = !e.defaultPrevented;
    });
    // The listener on the summary runs after the link's own, so check once the event is done.
    document.addEventListener('click', (e) => {
      followed = !e.defaultPrevented;
    });
    await user.click(link);
    expect(followed).toBe(false);
  });

  it('lists each speaker with label, shown name and evidence as text', async () => {
    setup(
      testNote({
        speakers: [
          { label: 'Speaker 0', name: 'Ayesha', evidence: 'said <b>hi</b>', name_edited: true },
          { label: 'Speaker 1', name: 'Rahim', evidence: 'Called Rahim', name_edited: false },
          { label: 'Speaker 2', name: null, evidence: '', name_edited: false },
        ],
      }),
    );
    await ready();

    const items = within(screen.getByRole('region', { name: 'Speakers' })).getAllByRole('listitem');
    expect(items[0]).toHaveTextContent('Speaker 0');
    expect(within(items[0]).getByText('Ayesha')).toBeInTheDocument();
    expect(within(items[0]).getByText('said <b>hi</b>')).toBeInTheDocument();
    expect(items[0].querySelector('b')).toBeNull();
    expect(within(items[1]).getByText('Rahim (inferred)')).toBeInTheDocument();
    expect(items[2]).toHaveTextContent('Speaker 2');
  });

  it('shows the transcript as text in a keyboard-operable tab', async () => {
    const { user } = setup(testNote({ transcript: '[00:00:01] Ayesha: see <b>Speaker 1:</b>' }));
    await ready();

    const summaryTab = screen.getByRole('tab', { name: 'Summary' });
    const transcriptTab = screen.getByRole('tab', { name: 'Transcript' });
    expect(summaryTab).toHaveAttribute('aria-selected', 'true');
    expect(screen.queryByText(/see <b>/)).not.toBeVisible();

    summaryTab.focus();
    await user.keyboard('{ArrowRight}');
    expect(transcriptTab).toHaveFocus();
    expect(transcriptTab).toHaveAttribute('aria-selected', 'true');
    const panel = screen.getByRole('tabpanel', { name: 'Transcript' });
    expect(panel).toHaveTextContent('[00:00:01] Ayesha: see <b>Speaker 1:</b>');
    expect(panel.querySelector('b')).toBeNull();

    await user.keyboard('{Home}');
    expect(summaryTab).toHaveFocus();
    await user.keyboard('{ArrowLeft}');
    expect(transcriptTab).toHaveFocus();
  });

  it.each(['processing', 'failed', 'no_speech'] as const)(
    'a %s note is read-only except "My notes"',
    async (status) => {
      setup(testNote({ status, failed_stage: 'transcribe', error: 'Deepgram said no' }));
      await ready();

      expect(screen.queryByRole('textbox', { name: 'Title' })).not.toBeInTheDocument();
      expect(screen.queryByRole('button', { name: 'Edit' })).not.toBeInTheDocument();
      expect(screen.queryByRole('textbox', { name: /Name for/ })).not.toBeInTheDocument();
      expect(screen.getByRole('textbox', { name: 'My notes' })).toBeInTheDocument();
    },
  );

  it('shows a failed note’s stage and error as text', async () => {
    setup(testNote({ status: 'failed', failed_stage: 'transcribe', error: 'Deepgram <i>401</i>' }));
    await ready();

    expect(screen.getByText('Failed at transcribe: Deepgram <i>401</i>')).toBeInTheDocument();
  });

  it('shows no_speech status', async () => {
    setup(testNote({ status: 'no_speech', title: 'No speech detected' }));
    await ready('No speech detected');
    expect(screen.getAllByText('No speech detected')).toHaveLength(2);
  });
});

describe('NoteView editing', () => {
  it('has labelled editors for a done note and toggles the summary into markdown', async () => {
    const { user } = setup();
    await ready();

    expect(screen.getByRole('textbox', { name: 'Title' })).toHaveValue('Weekly sync');
    expect(screen.getByRole('textbox', { name: 'Name for Speaker 1' })).toHaveValue('Rahim');
    expect(screen.getByRole('textbox', { name: 'My notes' })).toHaveValue('');
    expect(screen.getByRole('heading', { level: 2, name: 'Action items' })).toBeInTheDocument();

    await user.click(button('Edit'));
    const body = screen.getByRole('textbox', { name: 'Summary (markdown)' });
    expect(body).toHaveValue(testNote().body_md);
    await user.type(body, '!');
    await user.click(button('Done'));
    expect(noteUpdate).toHaveBeenCalledWith({ id: 7, body_md: `${testNote().body_md}!` });
    expect(screen.queryByRole('textbox', { name: 'Summary (markdown)' })).not.toBeInTheDocument();
  });

  it('autosaves after 1 s and reports it in the save line', async () => {
    const { user } = setup();
    await ready();

    await user.type(screen.getByRole('textbox', { name: 'My notes' }), 'deck');
    expect(noteUpdate).not.toHaveBeenCalled();
    await tick(1000);
    expect(noteUpdate).toHaveBeenCalledExactlyOnceWith({ id: 7, my_notes: 'deck' });
    expect(await screen.findByText('All changes saved')).toBeInTheDocument();
  });

  it('Back sends pending edits first', async () => {
    const { user, onBack } = setup();
    await ready();

    await user.type(screen.getByRole('textbox', { name: 'My notes' }), 'x');
    await user.click(button('Back'));
    expect(noteUpdate).toHaveBeenCalledWith({ id: 7, my_notes: 'x' });
    expect(onBack).toHaveBeenCalledOnce();
  });

  it('renames a speaker and shows the reply’s body, title and transcript', async () => {
    noteUpdate.mockResolvedValue({
      ok: true,
      note: testNote({
        title: 'Weekly sync with Rahim Uddin',
        body_html: '<ul><li>Rahim Uddin sends the deck</li></ul>',
        speakers: [
          testNote().speakers[0],
          { label: 'Speaker 1', name: 'Rahim Uddin', evidence: 'Called Rahim', name_edited: true },
        ],
        transcript: '[00:00:01] Ayesha (inferred): Hello.\n[00:00:04] Rahim Uddin: Hi.',
      }),
      vault: 'ok',
      vault_error: null,
    });
    const { user } = setup();
    await ready();

    const name = screen.getByRole('textbox', { name: 'Name for Speaker 1' });
    await user.clear(name);
    await user.type(name, 'Rahim Uddin');
    await user.tab();

    expect(noteUpdate).toHaveBeenCalledExactlyOnceWith({ id: 7, speaker: { label: 'Speaker 1', name: 'Rahim Uddin' } });
    expect(await screen.findByRole('heading', { level: 2, name: 'Weekly sync with Rahim Uddin' })).toBeInTheDocument();
    expect(screen.getByText('Rahim Uddin sends the deck')).toBeInTheDocument();
    expect(screen.getByText('Rahim Uddin', { selector: 'span' })).toBeInTheDocument();
    expect(screen.queryByText('Rahim (inferred)')).not.toBeInTheDocument();
    await user.click(screen.getByRole('tab', { name: 'Transcript' }));
    expect(screen.getByRole('tabpanel', { name: 'Transcript' })).toHaveTextContent('[00:00:04] Rahim Uddin: Hi.');
  });

  it('shows the vault banner with Retry until the vault write succeeds', async () => {
    noteUpdate.mockResolvedValueOnce({
      ok: true,
      note: testNote({ title: 'New' }),
      vault: 'failed',
      vault_error: 'vault folder is missing',
    });
    const { user } = setup();
    await ready();

    const title = screen.getByRole('textbox', { name: 'Title' });
    await user.clear(title);
    await user.type(title, 'New');
    await user.tab();
    const banner = await screen.findByRole('alert');
    expect(banner).toHaveTextContent('Vault note not updated: vault folder is missing');

    await user.click(within(banner).getByRole('button', { name: 'Retry' }));
    expect(noteUpdate).toHaveBeenLastCalledWith({ id: 7 });
    await waitFor(() => expect(screen.getByRole('alert')).toBeEmptyDOMElement());
  });

  it('saves "My notes" of a processing note without a banner', async () => {
    const { user } = setup(testNote({ status: 'processing' }));
    await ready();

    await user.type(screen.getByRole('textbox', { name: 'My notes' }), 'x');
    await user.tab();
    expect(noteUpdate).toHaveBeenCalledWith({ id: 7, my_notes: 'x' });
    await screen.findByText('All changes saved');
    expect(screen.getByRole('alert')).toBeEmptyDOMElement();
  });

  it('shows the too-large message for a too_large reply', async () => {
    noteUpdate.mockResolvedValueOnce({ ok: false, code: 'too_large', message: 'my_notes is too large' });
    const { user } = setup();
    await ready();

    await user.type(screen.getByRole('textbox', { name: 'My notes' }), 'x');
    await user.tab();
    expect(await screen.findByText(TOO_LARGE)).toBeInTheDocument();
  });

  it('refuses "My notes" over 1 MB locally and warns before Back drops it', async () => {
    const { user, onBack } = setup();
    await ready();

    const notes = screen.getByRole('textbox', { name: 'My notes' });
    await user.click(notes);
    await user.paste('x'.repeat(MY_NOTES_LIMIT + 1));
    await user.click(button('Back'));
    expect(noteUpdate).not.toHaveBeenCalled();
    expect(screen.getByText(TOO_LARGE)).toBeInTheDocument();
    expect(await screen.findByText(UNSAVED)).toBeInTheDocument();
    expect(onBack).not.toHaveBeenCalled();

    await user.click(button('Back'));
    expect(onBack).toHaveBeenCalledOnce();
  });

  it('shows not_editable and reloads the note', async () => {
    noteUpdate.mockResolvedValueOnce({ ok: false, code: 'not_editable', message: 'This note is processing' });
    const { user } = setup();
    await ready();

    await user.type(screen.getByRole('textbox', { name: 'Title' }), '!');
    await user.tab();
    expect(await screen.findByText('This note is processing')).toBeInTheDocument();
    expect(noteGet).toHaveBeenCalledTimes(2);
  });
});

describe('NoteView actions', () => {
  it('opens the note in Obsidian and the folder, and shows missing_file', async () => {
    vi.mocked(api.noteOpen).mockResolvedValueOnce({ ok: true }).mockResolvedValueOnce({
      ok: false,
      code: 'missing_file',
      message: 'No vault note found',
    });
    const { user } = setup();
    await ready();

    await user.click(button('Open in Obsidian'));
    expect(api.noteOpen).toHaveBeenCalledWith({ id: 7, what: 'obsidian' });
    await user.click(button('Open folder'));
    expect(api.noteOpen).toHaveBeenLastCalledWith({ id: 7, what: 'folder' });
    expect(await screen.findByText('No vault note found')).toBeInTheDocument();
  });

  it('disables Open folder and Rerun with a reason when the recording is missing', async () => {
    setup(testNote({ recording_missing: true }));
    await ready();

    expect(button('Open folder')).toBeDisabled();
    expect(button('Rerun summary')).toBeDisabled();
    expect(button('Rerun summary')).toHaveAccessibleDescription(RECORDING_MISSING);
    expect(button('Archive')).toBeEnabled();
    expect(button('Trash')).toBeEnabled();
  });

  it('reruns at once when nothing was edited', async () => {
    const { user } = setup(testNote({ my_notes: 'mine' }));
    await ready();

    await user.click(button('Rerun summary'));
    expect(noteRerun).toHaveBeenCalledExactlyOnceWith({ id: 7, replace_edits: false });
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
    expect(await screen.findByText('Summary rerun started')).toBeInTheDocument();
  });

  const speakers = testNote().speakers.map((s) => ({ ...s, name_edited: true }));
  it.each([
    ['title', { title_edited: true }],
    ['body', { body_edited: true }],
    ['speaker', { speakers }],
  ])('asks before rerunning when the %s was edited', async (_, edited) => {
    const { user } = setup(testNote(edited));
    await ready();

    await user.click(button('Rerun summary'));
    const dialog = await screen.findByRole('dialog', { name: 'Rerun summary' });
    expect(dialog).toHaveAttribute('aria-modal', 'true');
    expect(within(dialog).getByRole('button', { name: 'Keep my edits' })).toHaveFocus();
    expect(noteRerun).not.toHaveBeenCalled();

    await user.click(within(dialog).getByRole('button', { name: 'Keep my edits' }));
    expect(noteRerun).toHaveBeenCalledExactlyOnceWith({ id: 7, replace_edits: false });
  });

  it('Replace with new summary sends replace_edits', async () => {
    const { user } = setup(testNote({ title_edited: true }));
    await ready();

    await user.click(button('Rerun summary'));
    await user.click(await screen.findByRole('button', { name: 'Replace with new summary' }));
    expect(noteRerun).toHaveBeenCalledExactlyOnceWith({ id: 7, replace_edits: true });
  });

  it('Cancel and Escape close the dialog, send nothing and return focus', async () => {
    const { user } = setup(testNote({ title_edited: true }));
    await ready();

    await user.click(button('Rerun summary'));
    await user.click(await screen.findByRole('button', { name: 'Cancel' }));
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
    expect(button('Rerun summary')).toHaveFocus();

    await user.click(button('Rerun summary'));
    await screen.findByRole('dialog');
    await user.keyboard('{Tab}{Tab}{Tab}');
    expect(screen.getByRole('button', { name: 'Keep my edits' })).toHaveFocus();
    await user.keyboard('{Escape}');
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
    expect(button('Rerun summary')).toHaveFocus();
    expect(noteRerun).not.toHaveBeenCalled();
  });

  it('Escape closes the dialog when focus left it', async () => {
    const { user } = setup(testNote({ title_edited: true }));
    await ready();

    await user.click(button('Rerun summary'));
    await screen.findByRole('dialog');
    (document.activeElement as HTMLElement).blur();
    await user.keyboard('{Escape}');
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
  });

  it('sends one rerun while the first is still in flight', async () => {
    const reply = deferred<Awaited<ReturnType<typeof api.noteRerun>>>();
    noteRerun.mockReturnValue(reply.promise);
    const { user } = setup();
    await ready();

    await user.dblClick(button('Rerun summary'));
    await act(async () => reply.resolve({ ok: true }));
    await screen.findByText('Summary rerun started');
    expect(noteRerun).toHaveBeenCalledOnce();
  });

  it('asks about an edit still being typed when Rerun is pressed', async () => {
    const { user } = setup();
    await ready();

    noteUpdate.mockResolvedValueOnce({
      ok: true,
      note: testNote({ title: 'T', title_edited: true }),
      vault: 'ok',
      vault_error: null,
    });
    await user.type(screen.getByRole('textbox', { name: 'Title' }), '!');
    await user.click(button('Rerun summary'));
    expect(await screen.findByRole('dialog')).toBeInTheDocument();
  });

  it.each([
    ['tool_missing', 'Processing tool not found (meeting_path is unset); run `meeting install`', TOOL_MISSING],
    ['already_processing', 'already processing', 'already processing'],
    ['recording_missing', 'The recording is missing', 'The recording is missing'],
  ] as const)('shows the rerun error %s', async (code, message, text) => {
    noteRerun.mockResolvedValue({ ok: false, code, message });
    const { user } = setup();
    await ready();

    await user.click(button('Rerun summary'));
    expect(await screen.findByText(text)).toBeInTheDocument();
  });

  it('archives, reloads and confirms', async () => {
    const { user } = setup();
    await ready();

    await user.click(button('Archive'));
    expect(api.noteArchive).toHaveBeenCalledWith({ id: 7 });
    expect(await screen.findByText(ARCHIVED)).toBeInTheDocument();
    expect(noteGet).toHaveBeenCalledTimes(2);
  });

  it('goes back when the archived note is gone', async () => {
    const { user, onBack } = setup();
    await ready();
    noteGet.mockResolvedValue({ ok: false, code: 'unknown_note', message: 'No note 7' });

    await user.click(button('Archive'));
    await waitFor(() => expect(onBack).toHaveBeenCalledOnce());
  });

  it.each([
    ['already_processing', 'already processing'],
    ['invalid_path', 'The recording is not inside the recordings folder'],
    ['archive_failed', 'could not move the folder'],
  ] as const)('shows the archive error %s', async (code, message) => {
    vi.mocked(api.noteArchive).mockResolvedValue({ ok: false, code, message });
    const { user, onBack } = setup();
    await ready();

    await user.click(button('Archive'));
    expect(await screen.findByText(message)).toBeInTheDocument();
    expect(onBack).not.toHaveBeenCalled();
    expect(noteGet).toHaveBeenCalledOnce();
  });

  it('disables Archive and Trash while processing, with a reason', async () => {
    setup(testNote({ status: 'processing' }));
    await ready();

    expect(button('Archive')).toBeDisabled();
    expect(button('Trash')).toBeDisabled();
    expect(button('Trash')).toHaveAccessibleDescription(/processing/);
  });

  it('asks to confirm Trash for 3 s, then reverts without sending', async () => {
    const { user } = setup();
    await ready();

    await user.click(button('Trash'));
    expect(button('Confirm trash')).toBeInTheDocument();
    await tick(CONFIRM_MS);
    expect(button('Trash')).toBeInTheDocument();
    expect(api.noteTrash).not.toHaveBeenCalled();
  });

  it('trashes on the second click and goes back', async () => {
    const { user, onBack } = setup();
    await ready();

    await user.click(button('Trash'));
    await user.click(button('Confirm trash'));
    expect(api.noteTrash).toHaveBeenCalledExactlyOnceWith({ id: 7 });
    await waitFor(() => expect(onBack).toHaveBeenCalledOnce());
  });

  it('shows a trash error and stays', async () => {
    vi.mocked(api.noteTrash).mockResolvedValue({ ok: false, code: 'trash_failed', message: 'Trash is full' });
    const { user, onBack } = setup();
    await ready();

    await user.click(button('Trash'));
    await user.click(button('Confirm trash'));
    expect(await screen.findByText('Trash is full')).toBeInTheDocument();
    expect(onBack).not.toHaveBeenCalled();
  });
});

describe('NoteView polling', () => {
  it('polls a processing note every 3 s and stops once it is done', async () => {
    noteGet
      .mockResolvedValueOnce(got(testNote({ status: 'processing' })))
      .mockResolvedValueOnce(got(testNote({ status: 'processing' })))
      .mockResolvedValue(got(testNote()));
    setup(null);
    await ready();

    await tick(POLL_MS);
    expect(noteGet).toHaveBeenCalledTimes(2);
    await tick(POLL_MS);
    expect(noteGet).toHaveBeenCalledTimes(3);
    await waitFor(() => expect(button('Archive')).toBeEnabled());
    await tick(POLL_MS * 3);
    expect(noteGet).toHaveBeenCalledTimes(3);
  });

  it('does not poll a done note', async () => {
    setup();
    await ready();
    await tick(POLL_MS * 3);
    expect(noteGet).toHaveBeenCalledOnce();
  });

  it('follows a rerun for 10 s even while the note is done', async () => {
    const { user } = setup();
    await ready();

    await user.click(button('Rerun summary'));
    await screen.findByText('Summary rerun started');
    const after = noteGet.mock.calls.length;
    await tick(RERUN_FOLLOW_MS);
    const polls = noteGet.mock.calls.length - after;
    expect(polls).toBe(Math.floor(RERUN_FOLLOW_MS / POLL_MS));
    await tick(POLL_MS * 3);
    expect(noteGet.mock.calls.length - after).toBe(polls);
  });

  it('restarts the 10 s window on a second rerun', async () => {
    const { user } = setup();
    await ready();

    await user.click(button('Rerun summary'));
    await screen.findByText('Summary rerun started');
    await tick(POLL_MS * 2);
    await user.click(button('Rerun summary'));
    await waitFor(() => expect(noteRerun).toHaveBeenCalledTimes(2));
    const after = noteGet.mock.calls.length;
    await tick(RERUN_FOLLOW_MS - 1000);
    expect(noteGet.mock.calls.length - after).toBeGreaterThanOrEqual(2);
  });

  it('does not poll while the window is hidden', async () => {
    vi.spyOn(document, 'visibilityState', 'get').mockReturnValue('hidden');
    setup(testNote({ status: 'processing' }));
    await ready();

    await tick(POLL_MS * 3);
    expect(noteGet).toHaveBeenCalledOnce();
  });

  it('skips a tick while a focused field has unsaved text', async () => {
    const { user } = setup(testNote({ status: 'processing' }));
    await ready();

    await user.type(screen.getByRole('textbox', { name: 'My notes' }), 'x');
    noteUpdate.mockReturnValue(new Promise(() => {}));
    noteGet.mockClear();
    await tick(POLL_MS);
    expect(noteGet).not.toHaveBeenCalled();
  });

  it('stops polling on unmount', async () => {
    const { unmount } = setup(testNote({ status: 'processing' }));
    await ready();
    unmount();

    await tick(POLL_MS * 3);
    expect(noteGet).toHaveBeenCalledOnce();
  });
});

describe('NoteView layout (FR-11)', () => {
  const follows = (a: Node, b: Node) => (a.compareDocumentPosition(b) & Node.DOCUMENT_POSITION_FOLLOWING) !== 0;

  it('orders the tab bar, then the panels, then the side column, with no pinning', async () => {
    setup();
    await ready();

    const tablist = screen.getByRole('tablist', { name: 'Note' });
    const panels = screen.getAllByRole('tabpanel', { hidden: true });
    const speakers = screen.getByRole('region', { name: 'Speakers' });
    const myNotes = screen.getByRole('textbox', { name: 'My notes' });
    expect(panels).toHaveLength(2);
    for (const panel of panels) {
      expect(follows(tablist, panel)).toBe(true);
      expect(follows(panel, myNotes)).toBe(true);
    }
    expect(follows(myNotes, speakers)).toBe(true);
    expect(document.querySelector('[data-sticky]')).toBeNull();
  });

  it('keeps Transcript selected across polls and the reload that ends processing', async () => {
    noteGet
      .mockResolvedValueOnce(got(testNote({ status: 'processing' })))
      .mockResolvedValueOnce(got(testNote({ status: 'processing' })))
      .mockResolvedValue(got(testNote()));
    const { user } = setup(null);
    await ready();

    await user.click(screen.getByRole('tab', { name: 'Transcript' }));
    await tick(POLL_MS);
    await tick(POLL_MS);
    expect(noteGet).toHaveBeenCalledTimes(3);
    await waitFor(() => expect(button('Archive')).toBeEnabled());
    expect(screen.getByRole('tab', { name: 'Transcript' })).toHaveAttribute('aria-selected', 'true');
    expect(screen.getByRole('tabpanel', { name: 'Transcript' })).toBeVisible();
  });

  it('keeps Transcript selected after Rerun reloads the note', async () => {
    const { user } = setup();
    await ready();

    await user.click(screen.getByRole('tab', { name: 'Transcript' }));
    await user.click(button('Rerun summary'));
    await screen.findByText('Summary rerun started');
    const before = noteGet.mock.calls.length;
    await tick(POLL_MS);
    expect(noteGet.mock.calls.length).toBeGreaterThan(before);
    expect(screen.getByRole('tab', { name: 'Transcript' })).toHaveAttribute('aria-selected', 'true');
  });

  it('reaches the Summary Edit button with Tab from the tab row', async () => {
    const { user } = setup();
    await ready();

    screen.getByRole('tab', { name: 'Summary' }).focus();
    await user.tab();
    expect(button('Edit')).toHaveFocus();
    await user.keyboard('{Enter}');
    expect(screen.getByRole('textbox', { name: 'Summary (markdown)' })).toBeInTheDocument();
  });

  it('lists all 20 speakers in the side column', async () => {
    const speakers = Array.from({ length: 20 }, (_, i) => ({
      label: `Speaker ${i}`,
      name: `Person ${i}`,
      evidence: '',
      name_edited: true,
    }));
    setup(testNote({ speakers }));
    await ready();

    expect(within(screen.getByRole('region', { name: 'Speakers' })).getAllByRole('listitem')).toHaveLength(20);
  });

  it('says "No transcript." for a note without one', async () => {
    const { user } = setup(testNote({ transcript: null }));
    await ready();

    await user.click(screen.getByRole('tab', { name: 'Transcript' }));
    expect(screen.getByRole('tabpanel', { name: 'Transcript' })).toHaveTextContent('No transcript.');
  });

  it('puts the player in the bar with the tab row, before the panels', () => {
    render(<Tabs summary={<p>summary</p>} transcript={null} aside={<p>aside</p>} player={<p>player</p>} />);

    const tablist = screen.getByRole('tablist');
    const player = screen.getByText('player');
    expect(player.parentElement?.parentElement).toBe(tablist.parentElement);
    expect(follows(player, screen.getByRole('tabpanel', { name: 'Summary' }))).toBe(true);
    expect(follows(screen.getByText('summary'), screen.getByText('aside'))).toBe(true);
  });
});
