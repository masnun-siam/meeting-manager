import type { Note } from '../../api';

/** A `done` note for tests; pass the fields a test cares about. */
export function testNote(extra: Partial<Note> = {}): Note {
  return {
    id: 7,
    recording_path: '/recordings/meeting-2026-11-12_1000/recording.m4a',
    source: 'desktop',
    recorded_at: '2026-11-12T10:00',
    duration_s: 1830,
    language: 'en',
    status: 'done',
    failed_stage: null,
    error: null,
    title: 'Weekly sync',
    title_edited: false,
    body_md: '## Action items\n- Rahim (inferred) sends the deck',
    body_edited: false,
    speakers: [
      { label: 'Speaker 0', name: 'Ayesha', evidence: 'Introduced herself', name_edited: false },
      { label: 'Speaker 1', name: 'Rahim', evidence: 'Called Rahim by Ayesha', name_edited: false },
    ],
    my_notes: '',
    transcript: '[00:00:01] Ayesha (inferred): Hello.\n[00:00:04] Rahim (inferred): Hi.',
    event: null,
    vault_note_path: '/vault/Meetings/Weekly sync.md',
    created_at: '2026-11-12T04:30:00Z',
    updated_at: '2026-11-12T04:30:00Z',
    body_html: '<h2>Action items</h2>\n<ul>\n<li>Rahim (inferred) sends the deck</li>\n</ul>\n',
    recording_missing: false,
    ...extra,
  };
}

/** A promise a test settles when it wants, to hold a reply in flight. */
export function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((r) => {
    resolve = r;
  });
  return { promise, resolve };
}
