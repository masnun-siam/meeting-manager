# Rust crate: `meeting`

Covers: crates/meeting/Cargo.toml, crates/meeting/src/ (lib.rs, main.rs, calendar.rs, calendar_fetch.rs, pipeline.rs, plan.rs, jobs.rs, meta.rs, lock.rs, extract.rs, folders.rs, import.rs, deepgram.rs, transcript.rs, summarise.rs, summary_md.rs, context.rs, context_block.rs, ical.rs, vault.rs, notify.rs, logging.rs, test_support.rs, config.rs, secret.rs, spawn.rs, store.rs, install.rs, doctor.rs, native.rs), crates/meeting/tests/

One crate (`meeting`, edition 2024) with a library (`src/lib.rs`, linked by the desktop app) and the `meeting` binary (`src/main.rs`). Dependencies: `clap`, `serde`, `serde_json`, `toml`, `anyhow`, `tracing`, `tracing-subscriber`, `ureq` (HTTP), `keyring` (Keychain), `rusqlite` (bundled SQLite with FTS5, for the desktop app's store). External programs: `ffmpeg`, `claude`, `osascript`. See [architecture.md](architecture.md) for how the crate fits with the extension.

## CLI subcommands

Defined in `crates/meeting/src/main.rs` (`Cli`, `Command`, `run`).

| Command | Does | Details |
|---|---|---|
| `meeting process <recording> [--retranscribe] [--language bn\|en] [--job-id <id>]` | Builds a `Pipeline`, runs it in the foreground, sends a macOS notification at the end. `--job-id` is hidden: the native host passes it. | [features/processing.md](features/processing.md) |
| `meeting install` | Registers the native host, writes config. | [features/setup.md](features/setup.md) |
| `meeting config set-key` | Stores the Deepgram key in the Keychain (hidden prompt). | [features/setup.md](features/setup.md) |
| `meeting config set-calendar` | Only with `FEATURE_MEETING_CONTEXT=1`: checks, fetches and parses the secret iCal address (hidden prompt), then stores it in the Keychain; empty answer removes it. | [features/setup.md](features/setup.md) |
| `meeting import <file> --language bn\|en [--date "YYYY-MM-DD HH:MM"]` | Copies the file into a new meeting folder (`import::import`), prints `imported → <path>`, then runs the same foreground pipeline and notification as `process`. | [features/processing.md](features/processing.md) |
| `meeting doctor` | Prints one line per check; exits non-zero if any failed. | [features/setup.md](features/setup.md) |
| `meeting native-host [origin]` | Entry point Chrome launches (as `meeting <origin>`, rewritten by `chrome_args`). | [features/native-messaging.md](features/native-messaging.md) |

- `Language` (`bn`, `en`) is defined in `lib.rs` and imported by `deepgram`, `meta`, `native`, `plan`, `transcript`, `vault`.
- `run` dispatches with a chain of `if let` blocks, each returning. The trailing `bail!("... not implemented yet")` is unreachable for current commands.

## Module map

| Module | Responsibility | Key symbols |
|---|---|---|
| `lib.rs` | Library root: every module below as `pub mod`, `Language` | `Language` |
| `main.rs` | CLI parsing (gate-dependent via `cli`), wiring, exit code; imports the library as `meeting::` | `Cli`, `Command`, `cli`, `run`, `fetch_calendar`, `context_enabled`, `desktop_enabled` |
| `pipeline.rs` | Orchestrates one `process` run; saves job and meta on every stage change. Before summarising, with `context` on, loads `context.json`, looks the meeting up in the calendar and builds the context block (`context_extra`, `look_up`); calendar errors never fail the job. With `desktop` on, writes the notes store (`processing` at the start, the outcome at the end), writes the vault note from the merged row and takes `calendar_event` as the calendar match; store errors never fail the job | `Pipeline` (fields `key`, `context`, `desktop`, `store_path`, `calendar`, `calendar_policy`), `Pipeline::run`, `Ending`, `State`, `Track` |
| `plan.rs` | Pure function: which stages to skip, from file mtimes | `plan`, `Plan`, `Files` |
| `jobs.rs` | Job file store, `Stage` enum, atomic write, UTC time and ids | `Job`, `JobStore`, `Stage`, `write_atomic`, `now_utc`, `new_job_id` |
| `meta.rs` | `meta.json`: language, speakers, run history; `recorded_at` from folder name | `Meta`, `Run`, `Speaker`, `recorded_at_for` |
| `lock.rs` | Per-recording PID lock file | `Lock::acquire`, `Lock::path_for`, `is_held` |
| `extract.rs` | ffmpeg: video to mono 16 kHz Opus | `extract` |
| `folders.rs` | Archive and Trash for one recording folder, by recording path: the moves, the `meta.json` and vault note rewrite, job removal and, when given a store, the row update. Returns a `Refusal` (code and message) instead of failing. Called by `native`; the desktop app will call it too | `archive`, `trash`, `Refusal` |
| `import.rs` | `meeting import`: validate, name and copy a source into a new meeting folder | `import` |
| `deepgram.rs` | Deepgram HTTP client with retries | `Deepgram`, `Deepgram::transcribe` |
| `calendar_fetch.rs` | Download the secret iCalendar address under `Policy` (https only, ≤3 https redirects, 20 s, 50 MiB); errors are kinds only, never the address. Used by `set-calendar` and the `calendar` doctor check through `main::fetch_calendar`, and by `pipeline` with `Pipeline.calendar_policy` | `fetch`, `Policy`, `Policy::REAL`, `FetchError` |
| `transcript.rs` | Parse Deepgram JSON, render `transcript.md`; `utterance_lines` gives the store's copy (no header) | `parse`, `render`, `has_speech`, `utterance_lines`, `hh_mm_ss`, `Transcript`, `Utterance` |
| `summarise.rs` | Run headless `claude` (optionally after a context block), validate structured output | `summarise`, `Summary`, `SCHEMA`, `SYSTEM_PROMPT` |
| `summary_md.rs` | Render `Summary` to Markdown; `body_without_title` is the store's `body_md` | `render`, `speaker_names`, `body_without_title` |
| `vault.rs` | Write the Obsidian note: from a run (`write_note`) or from a store row after an app edit (`rewrite_from_row`, which copies the note's `meeting_title`, `attendees` and `calendar` lines); `compose_body` joins title, body and "My notes" | `write_note`, `Note`, `compose_body`, `rewrite_from_row`, `find_existing`, `repoint_note`, `note_people` |
| `notify.rs` | macOS notification via `osascript` | `notify`, `message`, `Outcome` |
| `logging.rs` | File logger init | `init` |
| `config.rs` | `config.toml` load/save, `home()`, `check_recording` (the foldered-recording path rule), `RECORDING_EXTENSIONS` | `Config`, `home`, `check_recording`, `RECORDING_EXTENSIONS` |
| `secret.rs` | Keychain access for the Deepgram key and the calendar address; `meeting config set-calendar` logic | `deepgram_key`, `set_key`, `calendar_address`, `set_calendar`, `read_hidden` |
| `spawn.rs` | Start `meeting process` detached (own process group, null stdio) from an absolute path, and the rule for a usable tool path (absolute, executable regular file). Used by `native` and the `meeting_path` doctor check; the desktop app will call both | `process_detached`, `check_tool` |
| `store.rs` | The desktop app's notes database (`notes.sqlite`: SQLite, WAL, FTS5 search): open and migrate by `PRAGMA user_version`, one row per recording, the rerun merge that keeps edits, the edit rules, search, attendees, the "recording deleted" mark and the "used before" names. Called by `pipeline` and by `folders` (through `native`) in a desktop build; see [Notes store](#notes-store) | `Store` (incl. `set_recording_deleted`, `deleted_paths`, `without_attendees`, `fill_attendees`, `used_before`), `Row`, `ListRow`, `RunStart`, `RunResult`, `Edit`, `EditError`, `OpenError`, `ATTENDEES_FILLED`, `error_kind`, `shown_name`, `default_path` |
| `install.rs` | `meeting install` | `install` |
| `doctor.rs` | `meeting doctor` checks (incl. the gated `calendar` and `meeting_path` checks), manifest path | `checks`, `Env`, `manifest_path` |
| `context.rs` | `context.json`: the extension's or the desktop app's meeting context (`platform: "desktop"`, optional `calendar_event`), checked against the PRD limits tables (reject or cut); a rejected `calendar_event` drops only the event; `load` reads and checks the file, logging only the rule | `Context`, `CalendarEvent`, `EventAttendee`, `Platform`, `check`, `load` |
| `context_block.rs` | Pure builder of the fenced, escaped, 64 KiB-capped meeting-context block for Claude; called by `pipeline` | `build`, `Input`, `SEPARATOR` |
| `ical.rs` | In-house iCalendar reader for the calendar match: unfolding, unescaping, raw times, `VTIMEZONE` offsets, HTML description to text (`html_to_text`, also used for desktop event notes); malformed input is an `Err`, never a panic. `secret` and `doctor` call `parse`; `calendar` reads the events | `parse`, `Calendar`, `Event`, `When`, `Zone`, `Event::plain_description`, `html_to_text` |
| `native.rs` | Chrome native messaging host; Archive and Trash look the job up and call `folders`, with the store when `Host.desktop` is on | `Host` (incl. `store_path`), `serve` |
| `test_support.rs` | Test fakes (`#[cfg(test)]` only) | `mock`, `mock_raw`, `stall`, `stub`, `envelope`, `temp`, `VALID` |

Processing modules are covered in [features/processing.md](features/processing.md). `native.rs`: [features/native-messaging.md](features/native-messaging.md). `install.rs`, `doctor.rs`, `config.rs`, `secret.rs`: [features/setup.md](features/setup.md).

## Layering

Arrows are imports in non-test code: `crate::` inside the library, `meeting::` from `main.rs`. Every module is a `pub mod` of the library; `main.rs` is the binary on top of it.

```mermaid
flowchart TD
    main --> pipeline & native & install & doctor & notify & logging & secret & deepgram & jobs & config & calendar_fetch
    pipeline --> plan & extract & deepgram & transcript & summarise & summary_md & vault & meta & jobs & lock & config
    pipeline --> context & calendar_fetch & ical & calendar & context_block & store
    vault --> store & calendar & context
    context --> calendar
    native --> jobs & lock & doctor & config & context & calendar_fetch & folders & spawn & store
    folders --> store & vault & meta & jobs & lock & config
    secret --> calendar_fetch & ical
    context --> summarise
    summarise --> context_block
    context_block --> context & calendar & transcript
    install --> doctor & jobs & config
    summary_md --> summarise
    summarise --> meta
    meta --> jobs
    store --> jobs
    jobs --> config
    logging --> config
    vault --> calendar & context
    doctor --> config & calendar_fetch & ical & spawn
```

- `main.rs` is the only place that builds dependencies (`Config::load()`, `JobStore::default_dir()`, `secret::deepgram_key`, `secret::calendar_address`, `Policy::REAL`, `context_enabled()`, `desktop_enabled()`) and passes them in.
- `calendar_fetch` imports no `crate::` module.
- Stage modules (`extract`, `deepgram`, `transcript`, `summarise`, `summary_md`, `vault`) do not import each other, except `summary_md` using types from `summarise`. `pipeline.rs` is the only caller of all of them.
- `ical` imports nothing from the crate; `calendar` imports only `ical`; `pipeline` calls `pick`, and `context_block` and `vault` read its types.
- `summarise` imports only `context_block::SEPARATOR`; `context_block` reads types from `context` and `calendar` and the clock format from `transcript`.
- `store` imports only `jobs::now_utc` and `crate::Language`.
- `spawn` imports only `crate::Language`. `folders` holds every Archive and Trash rule; `native` only maps a job id to its recording and a `Refusal` to a reply.
- `jobs::write_atomic` is the shared atomic-write helper (also used by `meta`, `pipeline`).

## Conventions

- Errors: `anyhow::Result` everywhere, no custom error types, except `calendar_fetch::FetchError` and `ical::Error` (fixed kinds with no address or input text, so neither reaches the log) and `store::OpenError` / `store::EditError` (states a caller must tell apart). Context via `.with_context(|| format!("cannot ... {}", path.display()))`. Messages that the user can act on name the fix (`run \`meeting doctor\``, `run \`meeting config set-key\``). `main` logs the error with `{e:#}` and returns it.
- Dependency injection by struct fields and fn pointers, not traits: `Pipeline.key: fn() -> Result<String>`, `Pipeline.calendar: fn() -> Result<Option<String>>`, `Pipeline.calendar_policy`, `Deepgram { base_url, retry_delays }`, `native::Host { open, key, ... }`, `doctor::Env`. Tests swap the fields.
- Subprocesses (`ffmpeg`, `claude`, `osascript`, `ps`) use absolute paths from config or constants, because Chrome starts the host with a bare `PATH`.
- Writes that readers may see use write-to-`.tmp`-then-rename (`write_atomic`, `extract` uses `.part`, `vault::write_note` uses a dotted `.tmp`). `config.rs` `save_to` writes directly.
- Feature flags: `FEATURE_MEETING_CONTEXT` (`context_enabled`) and `FEATURE_DESKTOP_APP` (`desktop_enabled`) are read once at build time in `main.rs`, both through `flag_gate` (on only for `1`), and passed to the library as plain `bool` fields (`context`, `desktop`) or arguments (`install::install`'s `desktop`). The library reads no flag itself. With `desktop` off, `install` writes no `meeting_path`, `doctor` has no `meeting_path` check and the native host opens no store.
- Logging: `tracing` macros, file only, via `logging::init()`. See Gotchas.
- Time: UTC strings built by hand (`jobs::format_utc`), no time crate.
- Not removing a file that is absent: `remove` helpers treat `NotFound` as success (`pipeline.rs`, `lock.rs`).
- Tests are in-file `#[cfg(test)] mod tests` in every module; names are full sentences in snake_case describing behaviour.

## Notes store

`store.rs` owns `~/Library/Application Support/meeting-manager/notes.sqlite` (`store::default_path(home)`). It is only to be opened when `FEATURE_DESKTOP_APP` is on. The native host opens it for Archive and Trash (`Host.store_path`, only when `Host.desktop`) and passes it to `folders`; the pipeline and the app will use it too.

- **Open:** `Store::open` creates the directory and the file with mode 0600 before SQLite sees it (so `-wal` and `-shm` get the same mode), sets a 5 s busy timeout and WAL, then runs the migration steps above the stored `user_version` in one `BEGIN IMMEDIATE` transaction that re-reads the version under the lock. Schema version: 2 (step 2 adds `attendees` and `recording_deleted_at`). SQLite refuses the switch to WAL at once while another connection holds a lock, so `open` repeats the set-up on a busy error for up to 5 s: several processes may open a new file at the same time.
- **Typed open errors:** `OpenError::Newer { found }` (the file's version is above this build's; the file is not touched) and `OpenError::Corrupt` (not a SQLite database), both downcastable from the `anyhow::Error`. Every other failure (directory, permissions, busy) is a plain error, so a caller never moves a healthy database aside.
- **Tables:** `meetings` (unique `recording_path`; `speakers`, `event` and `attendees` are JSON text (`attendees` a list of names, `NULL` until something fills it, `[]` when looked and none found); `recording_deleted_at` is a UTC `YYYY-MM-DDTHH:MM:SSZ` or `NULL`; `series_id` is a generated column over `event`), `settings` (`key`, `value`), and `meetings_fts` (FTS5 over title, body, "My notes", speaker names and transcript, tokenizer `unicode61 remove_diacritics 0 categories 'L* N* Co Mn Mc'` so Bengali words stay whole). Triggers keep the index in step and fire only for the indexed columns.
- **Runs:** `start_run` upserts the row by path as `processing` and touches no note field. `merge_run` reads and writes under one write lock: an edited title, body or speaker name is kept, unedited ones follow the result, a `None` in the result never erases a stored value, including `attendees` (`start_run` writes `RunStart.attendees` the same way), "My notes" never changes, and each edited speaker name is put back into the new unedited title and body. No run, edit, `clear_edits` or `moved` touches `recording_deleted_at`.
- **Edits:** `edit` allows "My notes" in every status (up to 1,048,576 bytes) and title, body and speaker names only on a `done` note. A speaker rename replaces the previous name (`Name (inferred)`, `Name` or the label) as a whole word in title and body, without marking them edited. A whole word is bounded by characters that are not `char::is_alphanumeric`. `clear_edits` drops the edited marks.
- **Recording deleted and attendees:** `set_recording_deleted(id, deleted)` sets the mark to UTC now or clears it, without changing `updated_at` or the search index; `deleted_paths` lists the recording paths of marked rows; `ListRow.recording_deleted` is true when the mark is set. `Row.recording_deleted_at` is `#[serde(skip)]`, never sent to the UI. `without_attendees` returns `(id, recording_path)` of the rows whose `attendees` is `NULL`; `fill_attendees(id, names)` writes only while it is still `NULL`, so a run or rescan that wrote first wins. The one-time fill records its end in the setting `ATTENDEES_FILLED` (`"attendees_filled"`). An unknown id is a no-op for both writes.
- **Used before:** `used_before` returns the speaker names with `name_edited` true across all rows, newest `updated_at` first (ties: newest id, then speaker order), distinct ignoring case with the newest spelling kept (folded in Rust: SQLite `lower()` is ASCII only), blank names skipped, at most 50 (`USED_BEFORE_MAX`).
- **Search:** `list(Some(query))` quotes every typed word (operators and column filters are plain text), requires all of them, treats the last as a prefix, and orders by `bm25` then newest. `list` and `earlier` return `ListRow` (no body, transcript, speakers or notes).
- **Logging:** the store logs nothing. Callers log `store {op} failed: {kind}` with a fixed operation name (`open`, `moved`, `removed`) and `store::error_kind(&e)`, a fixed word (`busy`, `read_only`, `corrupt`, `disk_full`, `cannot_open`, `constraint`, `sqlite`, `newer_version`, `io`, `other`), never the error text.
- A `Store` holds one connection; each caller opens its own.

## Testing

| What | Where |
|---|---|
| Unit tests | `#[cfg(test)] mod tests` at the bottom of each `src/*.rs` |
| Shared fakes | `crates/meeting/src/test_support.rs` (declared `#[cfg(test)] mod test_support` in `lib.rs`) |
| Binary-level tests | `crates/meeting/tests/logging.rs` (spawns `CARGO_BIN_EXE_meeting` with a temp `HOME`, checks the log file and empty stdout); `crates/meeting/tests/native_host.rs` (native host over stdio) |
| Fixtures | `crates/meeting/tests/fixtures/short.webm`; `crates/meeting/tests/fixtures/deepgram/{en_two_speakers,bn_two_speakers,one_speaker,empty}.json`; `crates/meeting/tests/fixtures/calendar/*.ics` (CRLF, as feeds send them) |

- `test_support::mock(responses)` is a TCP server on `127.0.0.1:0` that answers one scripted `(status, body)` per connection and sends each request head down a channel. Pair it with `Deepgram { base_url, retry_delays: vec![] }`.
- `test_support::stub(dir, tail)` writes a fake `claude` shell script that records args and stdin to `args.txt` and `stdin.txt`, then runs `tail`. `envelope(VALID)` produces the JSON `claude` returns.
- `test_support::temp(name)` makes `<tmp>/meeting-test-<pid>-<name>`. Some modules define their own local `temp` and `ffmpeg()` instead of using the shared one (inconsistent).
- Pipeline and extract tests need a real `ffmpeg` on the machine; they find it with `/usr/bin/which ffmpeg`.
- Fixtures are included with `include_str!("../tests/fixtures/...")` or `env!("CARGO_MANIFEST_DIR")`.
- Dev commands: see [development.md](development.md).

## Gotchas

- Never log the Deepgram key or transcript text, and never write to stdout in `native-host` mode (stdout is Chrome's message channel). `logging.rs` says so; if file logging cannot open, the tool continues with a stderr warning.
- `Job` optional fields serialise as `null` on purpose; the extension expects the keys ([extension.md](extension.md)).
- `Stage::name()` and the serde lowercase names must stay in step; `meeting process` prints `name()` per stage and the extension reads the serialised form.
- `plan.rs` treats an absent input as fresh, because inputs are deleted after success. Changing which files are deleted changes resume behaviour.
- `lock.rs` checks PID liveness with `/bin/ps`; a reused PID can make a stale lock look held (noted in a `ponytail:` comment).
- `main.rs` handles `--help`/parse errors with `try_parse` so they are logged.
