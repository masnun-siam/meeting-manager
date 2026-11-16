# Recording folders, Archive and Trash

Covers: crates/meeting/src/folders.rs

PRD: [recorder/recording-folders](../../prds/recorder/recording-folders.md). The requests arrive through the native host: [native-messaging.md](native-messaging.md).

## Archive

`archive` acts on the job's recording folder, never on a path from the extension. The host only looks the job up (`lookup_job`, step 1's first half) and maps the result to a reply; every other rule is `folders::archive(config, jobs, store, recording)`, which the desktop app will call with a recording path of its own.

1. Unknown or malformed `job_id` → `unknown_job`. Recording file gone → delete its jobs, reply `{ok: true}` (if the jobs cannot be deleted: `archive_failed`, or `trash_failed` for `trash`).
2. `check_recording` → `invalid_path`; folder already under `Archive/` → `invalid_path` ("already archived"); held lock → `already_processing`.
3. Rename the folder to `<recordings_dir>/Archive/<name>[-2, -3, ...]` (first free name).
4. Set `meta.json`'s `recording` (skipped if absent) to the moved file under its real name (`recording.m4a` stays `recording.m4a`), then `vault::repoint_note` rewrites the note's `recording:` and `transcript:` lines (skipped if no note), then `JobStore::remove_for` deletes every job for the recording.

A failure after step 3 replies `archive_failed`; the folder stays in `Archive/` and the jobs stay.

### Notes store (Archive)

Only a host built with `FEATURE_DESKTOP_APP=1` (`Host.desktop`) opens the notes database at `Host.store_path` (`store::default_path`), once per `archive` or `trash` request, and passes it to `folders`. With the flag off no store file or directory is created.

- Archive: right after the rename in step 3, the row's recording path becomes the archived path (`Store::moved`), so it is right even when a later step fails.
- Trash: the row is removed (`Store::removed`) once the folder is in the Trash. The missing-file shortcut of either verb removes the row too.
- A refusal before the move (`invalid_path`, `already_processing`, a failed trash command) touches no row, and no row is ever created here.
- A store that cannot be opened or written never changes the reply: it is logged as `store open failed: <kind>`, `store moved failed: <kind>` or `store removed failed: <kind>` (`store::error_kind`, a fixed word) and the move goes on.

## Trash

`trash` runs the same steps 1–2 as `archive` (job lookup, missing-file shortcut, `check_recording`, held lock), except an archived recording is allowed. `folders::trash` then runs `/usr/bin/trash -s <folder>` (absolute path; `-s` makes a failed move exit non-zero) through the injected `Host::trash`, and `JobStore::remove_for` deletes every job for the recording. The vault note is untouched. `/usr/bin/trash` ships with macOS 15+; on older macOS the reply is `trash_failed` naming the missing command.
