# Desktop app

Covers: desktop/

The Meeting desktop app: a Tauri 2 app whose Rust side (`desktop/src-tauri`, package `meeting-desktop`, lib `meeting_desktop`) links the `meeting` library and whose UI (`desktop/src`) is React 19 with strict TS and CSS modules. Every command is implemented (the calendar commands: [features/desktop-calendar.md](features/desktop-calendar.md); the recorder commands: [features/desktop-recorder.md](features/desktop-recorder.md); the notes commands: [Notes commands](#notes-commands-notes) below). Behaviour: [prds/desktop-app.md](prds/desktop-app.md).

## Flag

`FEATURE_DESKTOP_APP` is read at build time (`option_env!` in `lib.rs`, on only for the value `1`; see `FLAGS.md`).

| Built | Window | Registered |
|---|---|---|
| Off (default) | label `off`, page `off.html` ("Built without FEATURE_DESKTOP_APP"), no capability | Nothing: no state, no command, no plugin, no hook |
| On | label `main`, page `index.html` | The single-instance and updater plugins, the `App` state, the 26 commands, the launch hooks |

## Rust side (`desktop/src-tauri/src`)

| File | Holds |
|---|---|
| `main.rs` | Calls `meeting_desktop::run()`. The binary is `meeting-desktop`; the CLI owns the name `meeting` |
| `lib.rs` | `run()`, the flag, `configure` (setup and command registration), `launch` (state, tool swap, window, hooks), the `App` state `{env, recorder, calendar, notes, admin, updater}`. `launch_in` takes the `Env` and `Updater` constructors, so its tests start the app in a temp home with an updater whose checks fail (`updater::tests::unplugged`) and an invoked command never touches the real store, vault, recordings or update server. The issues that fill a module do not edit it |
| `env.rs` | `Env { home, store_path, jobs }`; `config()` reloads `config.toml` on every call and returns the error as a string; `store()` opens the notes store and keeps `anyhow::Error`, so `store::OpenError` can be downcast. `Env::at(home)` is the constructor tests use |
| `reply.rs` | `ok(fields)`, `err(code, message)`: the reply shape of every command |
| `recorder/` | `Recorder` ([features/desktop-recorder.md](features/desktop-recorder.md)): WAV writer, folder, conversion, hand-off, notices; `recorder_status`, `recorder_start`, `recorder_pause`, `recorder_resume`, `recorder_stop`, `recorder_retry`; hooks `on_launch` (crash recovery), `on_sleep` (will-sleep observer), `on_exit_requested` (`Recorder::quit`; `false` keeps the app running) |
| `calendar/` | `Calendar` (EventKit, [features/desktop-calendar.md](features/desktop-calendar.md)); `calendar_status`, `calendar_events`, `calendar_event`, `calendars`, `set_hidden_calendars`; hook `on_launch`; `Calendar::event_for_context` for `context.json` |
| `notes/mod.rs` | `Notes { open, spawn }` (fn pointers; real: `native::open_with_system`, `spawn::process_detached`); `notes_list`, `note_get`, `note_update`, `note_rerun`, `note_open`, each a thin wrapper over a `pub fn` taking `&Env` |
| `notes/markdown.rs` | `render_html` (summary markdown to safe HTML) and `shown_transcript` (speaker labels to current names) |
| `admin/mod.rs` | `Admin { progress, trash, doctor, doctor_timeout }`; `rescan`, `rescan_status`, `health`, `note_archive`, `note_trash`, `settings_info`; hook `on_launch` (first backfill) |
| `admin/backfill.rs` | Folder scan (`candidates`), `read_folder` (folder files to a `RunResult`), `run` (merge, then remove gone rows, then `backfill_done`), `open_or_rebuild` (move a broken store aside) |
| `updater/mod.rs` | `Updater { version, check, restart, every, poll }` (FR-25, FR-26; below); `update_status`, `update_check`, `update_install`, `set_auto_update`; hook `on_launch` (automatic check and 6-hourly timer); `swap_tool`, the tool swap (FR-27), which fills `tool_error` through `Updater::set_tool_error` |
| `updater/tool.rs` | `swap(bundled, target)`: replaces `meeting_path` with the bundled `meeting` when the bytes differ (FR-27; below) |
| `tray.rs` | Hook `setup`: the menu-bar item ([features/desktop-recorder.md](features/desktop-recorder.md#menu-bar-item-trayrs)); needs the main thread, so the mock-runtime tests run without it |
| `capture/` | Audio capture: the boundary in `mod.rs` and the Core Audio implementation (below) |

- **Startup order** (`launch`, flag on): manage `App`, `updater::swap_tool` (before anything can spawn a job), open the window, then `tray::setup`, `recorder::on_launch`, `admin::on_launch`, `calendar::on_launch`, `recorder::on_sleep`, `updater::on_launch`. Hooks take `&AppHandle<R>` for any runtime so tests run them on Tauri's mock runtime.
- **Single instance:** `tauri-plugin-single-instance` is added in `run()`, before everything else, and only there: on macOS a second instance in one process exits it, which would end the test process. A second launch focuses the `main` window.
- **Updater:** `tauri-plugin-updater` is added in `run()` right after single instance, flag on only. Its endpoint (the `latest.json` of this repo's latest GitHub Release) and public key are `plugins.updater` in `tauri.conf.json`; release builds come from `release.yml` ([development.md](development.md#releasing)).
- **Quit:** with the flag on, `run()` calls `recorder::on_exit_requested` on `RunEvent::ExitRequested` and prevents the exit when it returns `false`. Built off, it is not called, nor for the updater's relaunch (`code` = `tauri::RESTART_EXIT_CODE`): the updater holds the recorder in `saving` until the process ends, so waiting for that save would hang the main thread, and Tauri ignores `prevent_exit` on a restart anyway. Any other exit asks `updater::on_exit_requested` first, which keeps the app running while an update installs.
- **Closing the window** (flag on, D15) hides it instead: the app and its menu-bar item keep running until Quit. The menu's Open Meeting, a second launch or a click on the Dock icon (`RunEvent::Reopen`) shows it again (`focus_main`).
- **Logging:** `meeting::logging::init()`, the same log file and rules as the tool. Stubs log nothing.
- **App icon:** `icons/icon.svg` is the source. The bundle PNGs (`32x32`, `128x128`, `128x128@2x`, `icon`) and `icon.icns` are rendered from it with `rsvg-convert` and `iconutil`; the `tray-*.png` menu-bar icons are separate.

## Command contract

The PRD's API table (Technical design › API) is the contract. Every command is `#[tauri::command(rename_all = "snake_case")]`, returns `serde_json::Value`, and answers `{ok: true, ...}` or `{ok: false, code, message}`.

- `desktop/src/api.ts` has one typed function per command, the request and reply types, and each command's error codes as a string-literal union. Argument keys are snake_case, exactly the Rust argument names. A reply without a boolean `ok` throws `Invalid reply from the Meeting app`; an error reply is returned, not thrown.
- `desktop/src/api.mock.ts`: `mockApi(overrides)` for component tests (`vi.mock('./api', async () => (await import('./api.mock')).mockApi())`).
- A test in `lib.rs` checks that `api.ts` names the same 26 commands. A new command changes the contract: PRD first.

## Capture boundary (`capture/mod.rs`)

`Capture { check, request, start }` is a struct of fn pointers so the recorder's tests swap in a fake.

| Item | Meaning |
|---|---|
| `SAMPLE_RATE` | 48 000 Hz |
| `check` | `Err(Microphone)` when the microphone is denied or restricted; never prompts. System Audio has no public status API, so `check` cannot see it (D36) |
| `request` | Asks for the microphone if not yet asked, then creates and destroys a throwaway tap (macOS asks for System Audio on first use); a failed tap or aggregate device is `Err(SystemAudio)`; any other probe failure (no input device) is `Ok`, and `start` reports it. Blocks until the user answers, so call it off the main thread |
| `start(samples, events)` | Starts capture and returns a `Stream`; `samples` gets mono 48 kHz i16, already mixed and clipped |
| `Event` | `DeviceChanged`, or `Failed(String)` when capture ended |
| `Denied` | `Microphone` or `SystemAudio`; an `Error` whose message names the System Settings pane, so it survives `anyhow` and can be downcast |
| `Stream::stop(self)` | Runs the stop closure once |

`samples` runs on Core Audio's real-time thread and `events` on the rebuild thread: no blocking, heavy allocation or logging. `start` returns an error that downcasts to `Denied` when the microphone is denied or the tap or aggregate device cannot be created.

### Implementation (`capture/*.rs`)

| File | Holds | Tested by |
|---|---|---|
| `mix.rs` | `Downmix` (average channels and buffers to mono), `mix` (sum mic and tap, pad, NaN to 0, clip to [-1, 1]), `to_i16` (1.0 → 32767, -1.0 → -32767), `Resampler` (linear, state kept across calls) | unit tests |
| `rebuild.rs` | `Policy::PRD` (settle 200 ms, retry 250 ms, give up 5 s, health check 2 s), the pure `Rebuild` timer, and `Worker`, whose `build`, `halt`, `health`, `events` and clock are injected | unit tests |
| `devices.rs` | Property reads and writes, default input, device UID, own process object, stream layout, nominal rate, default-device listeners; local `AudioBuffer`, `AudioBufferList`, `StreamFormat` | manual |
| `graph.rs` | `Graph::build`: mono global tap excluding the app, private aggregate device (default input as main sub-device, tap with drift compensation), 48 kHz nominal rate, IOProc; `probe()` for `request` | one `#[ignore]` hardware test |
| `permission.rs` | `check`, `request`, `microphone_revoked` (AVFoundation) | manual |

- **Graph.** Every Core Audio object lives in `Parts`, whose `Drop` stops the IOProc and destroys the IOProc id, aggregate device and tap in that order, only those created, so a failed build leaks nothing. The aggregate delivers the microphone's buffers first and the tap's last; the build checks the aggregate's layout equals the microphone's followed by the tap's and fails otherwise. If the device keeps a rate other than 48 kHz, the IOProc resamples.
- **IOProc.** Mixes into scratch buffers reserved at build time (16 384 frames, more on the resampled side when the device runs below 48 kHz; a larger cycle is dropped rather than allocating), then calls `samples` through `try_lock` on the mutex it shares with later graphs: a held lock drops the buffer instead of blocking. A buffer count it was not built for sets a flag the health check reports.
- **Rebuild.** Listeners on the default input and output device send `Changed` to the worker thread. The worker waits until 200 ms pass without a change, drops the old graph, builds a new one with the same `samples`, and sends `DeviceChanged`; a failure is retried every 250 ms until 5 s after the first change, then `Failed("Audio capture could not follow the device change: <stage> failed (OSStatus n)")`. Every 2 s, when no rebuild is pending, it checks the microphone permission (`Failed("Microphone permission was revoked")`) and the layout flag, which each rebuild clears. While device changes keep a rebuild pending, the check waits, so a revocation then is reported up to 5 s later than the 3 s target. After `Failed` the graph is gone and changes are ignored until `stop`.
- **Stop.** `Stream::stop` tells the worker to stop, joins it (it drops the graph), then removes the listeners; each stream leaks its 8-byte listener `Sender`, since Core Audio does not document that removal waits for a running callback. Dropping the `Stream` also ends capture, without waiting. `stop` must not be called from `events`, which runs on the worker it joins.
- **Not detected** (D36): a System Audio denial that yields a silent tap instead of a failure, and a System Audio revocation while recording until the next rebuild fails.
- **IOProc API.** `AudioDeviceCreateIOProcIDWithBlock` needs the `dispatch2` feature, so the C function-pointer form is declared locally in `graph.rs`, with the CoreAudioTypes structs in `devices.rs` (their bindings sit behind the `objc2-core-audio-types` feature). The lockfile lists `objc2-core-audio-types` as an optional dependency of `objc2-core-audio`; it is not compiled.
- **Hardware check:** `cargo test -p meeting-desktop capture -- --ignored` builds the real graph for 2 s and asserts about 48 000 samples per second. The host terminal needs Microphone and System Audio Recording permission; with a MacBook lid closed the built-in microphone delivers silence.

## Notes commands (`notes/`)

Each command opens the store through `Env::store()`; an open or query failure is `store_unavailable` with the open error as the message. Nothing logged or returned in a `message` holds a title, body, transcript, "My notes" or a name: logs carry the note id and an error kind.

| Command | Behaviour |
|---|---|
| `notes_list` | `Store::list(query)` order kept. `processing` rows get `stage` from the newest job file of the recording (one job-directory scan, only when a row is processing); `failed` rows get `failed_stage` and `error`. `folder_name` is the meeting folder; `recording_missing` is `!recording_path.is_file()` |
| `note_get` | The store row as serialised, plus `body_html`, `recording_missing`, and `transcript` with each `[hh:mm:ss] <label>: ` replaced by `store::shown_name` (exact label then `: `, so `Speaker 1` never matches `Speaker 10`) |
| `note_update` | Applies `title`, `body_md`, `my_notes`, `speaker` in that order through `Store::edit`, stopping at the first refusal (`UnknownSpeaker` is `not_editable`). A `done` row's vault note is then always rewritten (`vault::rewrite_from_row`, path saved), so the UI's Retry resends only `id`; a failure is `vault: "failed"` with the root cause as `vault_error`, a file system error as `cannot write the vault note: <io error>` (the note's file name holds the title, so it is left out). Other statuses take only "My notes", store only (D39) |
| `note_rerun` | Refuses in order: `unknown_note`, `recording_missing`, `already_processing` (lock held), `tool_missing` (`meeting_path` unset, failing `spawn::check_tool`, or a failed spawn). Then `clear_edits` when `replace_edits`, then spawns `meeting process <recording> [--language]` detached |
| `note_open` | `folder`: the recording's parent. `obsidian`: the stored `vault_note_path` if it is a file, else `vault::find_existing` in the vault folder (the found path is saved). No target is `missing_file`; a failed `open` is `open_failed` |

`render_html` runs `pulldown-cmark` (feature `html`, `Options::empty()`) with raw HTML turned into text (D25) and drops the tag of any link or image whose destination is not `http://`, `https://` or `mailto:` (case-insensitive) or an email autolink, keeping its text.

## Admin commands (`admin/`)

No command takes a path from the UI. `health`, `note_archive` and `note_trash` are `async` and run on `spawn_blocking`, so a slow doctor or move never freezes the window. Logs carry counts and error kinds only, never a title, transcript or name.

| Command | Behaviour |
|---|---|
| `rescan` | `not_configured` when `config.toml` can't be read; otherwise `{started: true}` at once, also when a run is already going (no second run). The run has its own thread and store connection: it opens the store, and a corrupt or newer one (`store::OpenError` only) is renamed (`-wal`, `-shm`, then the database, so a failed move leaves it broken for the next rescan) to `notes.sqlite.broken-<YYYYMMDDTHHMMSSZ>[-wal\|-shm]` and recreated (D33). A moved-aside file is never replaced or deleted; any other open error ends the run with nothing moved |
| `rescan_status` | `{running, done, total}` from atomics; all `0` before the first run |
| `health` | `<meeting_path> doctor` (argument list, null stdin and stderr, killed after 10 s, also when it closed stdout but keeps running), its `✓ <name>` / `✗ <name>: <fix>` lines parsed (D38). `meeting_path` unset or failing `spawn::check_tool` gives one failing `meeting_path` check without spawning; a timeout, spawn failure or output with no check line gives one failing `doctor` check |
| `settings_info` | `recordings_dir`, `vault_dir`, `vault_folder` from `config.toml` and `store: {ok}` or `{ok: false, message}` (the open error). Never moves the store |
| `note_archive` / `note_trash` | Row id to `recording_path`, then `folders::archive` / `folders::trash` with the store; a `Refusal` passes through as `{code, message}` (D41). `unknown_note`, `store_unavailable`, and `not_configured` when the config can't be read |

**Backfill** (`on_launch` when the store opens and has no `backfill_done`, and every `rescan`): every `recording.<ext>` (first in `RECORDING_EXTENSIONS` order) in a folder directly inside the recordings directory or `Archive/`, checked by `config::check_recording`, so flat files, `Archive/` itself and hidden folders are skipped. A folder whose lock is held is left to the pipeline. Each row goes through `Store::merge_run`, so edits are kept:

- `done` when `summary.md` exists: title from `meta.json`, else the summary's `# ` line; `body_md` without that line; speakers from `meta.json`.
- `no_speech` when `transcript.md` has the line `No speech detected` and no utterance line.
- otherwise `failed`: the newest `meta.json` run error, else the newest job's, split at `"<stage>: "`; with neither, a fixed "rerun the summary" message.
- Every status: `language`, `duration_s` (rounded) and `recorded_at` from `meta.json` (`recorded_at_for` without it), `transcript` = `transcript::utterance_lines`, `source` and `event` from `context.json` by `pipeline::source_of` / `pipeline::event_link` (D40).

A file that can't be read counts as missing. After the folders, rows whose recording is no longer a file are removed, unless the recordings directory itself can't be read (an unmounted volume would make every recording look gone). `backfill_done` is set only when no row failed to store.

## Updates (`updater/`)

Rust side only (D47): the plugin is reached through `UpdaterExt` in `Updater::new`, never from the webview. One `Mutex` holds the phase (`idle`, `checking`, `downloading`, `ready`, `installing`), `progress`, the last answered check's `available` release, `error` and `tool_error`. Work runs on named threads (`update-check`, `update-install`, `update-timer`); the commands return at once. Logs carry the failed stage only.

| Command | Behaviour |
|---|---|
| `update_status` | `{version, auto_update, state, progress, available: {version, notes}\|null, error, tool_error}`. `auto_update` reads the store setting `auto_update`: only `"true"` is on; absent, other values or an unopenable store are off (logged by kind) |
| `update_check` | `{started: true}`. Starts a check only from `idle`, so two never run at once and a check during a download changes nothing. A failure sets `error` to `Couldn't check for updates: <reason>`; a connect or timeout error is `offline`. `is_newer` (the plugin's `version_comparator`) offers only a higher `major.minor.patch` that is not a pre-release (D48) |
| `update_install` | `no_update` with no `available`; `recording_active` ("Finish the recording to install") when the recorder is not `idle`, with nothing downloaded. Otherwise `{started: true}` and the install below; also `{started: true}` when one is already running |
| `set_auto_update` | `{enabled}` → stores `"true"`/`"false"`, answers `{auto_update}`; `store_unavailable` when the store can't be written. Checks nothing at once (FR-26: next launch or tick) |

**Install** (manual or automatic): download with `progress` 0–100, then `ready` while it polls `Recorder::begin_saving()` every second. The `SavingGuard` is held from install to relaunch, so Start is refused (`already_recording`) in between. Then `Update::install` and `AppHandle::restart()`. A quit asked for while `installing` is held back (`updater::on_exit_requested`, asked before the recorder's, whose wait for `saving` would never end) and done once the install ends, instead of the relaunch. A failed download (the signature is verified there) or install drops the guard and returns to `idle` with `error` = `Couldn't install the update: <reason>`; the running app is unchanged and the next launch or check tries again.

**Automatic** (`auto_update` on): `on_launch` checks once and the `update-timer` thread every 6 hours, each time reading the setting; a newer release is downloaded in the background, even while recording, and installed through the same gate, so a recording running when the download finishes delays the relaunch until it is saved.

**Tool swap** (FR-27, `swap_tool`, every launch before the window): the bundled tool is `resource_dir()/meeting`, read only in a bundled app; a development run skips it, since its resource directory is Cargo's output directory and its `meeting` a local build (D50). The target is `meeting_path` from `config.toml`. Nothing is written when there is no bundled tool, no `meeting_path`, no regular file there, or the bytes are equal (or it is the same file). Otherwise `updater/tool.rs` writes `<dir>/.meeting.update-<pid>` (mode 0755, `sync_all`) and renames it over the target, so a running job keeps the old file and the next spawn uses the new one; any failure removes the temp file and keeps the old tool. A failure sets `tool_error` to `The meeting tool wasn't updated: <io error>. Make <dir> writable or run `meeting install` from a writable location`; the next launch tries again and clears it. Logs carry the outcome (`replaced`, `same`, `no target`, `no bundled tool`, `failed`) and the io error kind only.

## UI (`desktop/src`)

| File | Holds |
|---|---|
| `App.tsx` | Sidebar (app mark, then Upcoming, All notes, Settings as buttons with `aria-current`; All notes while a note is open goes back to the list), `RecordingBar` above every view, the open note id. The sidebar comes first in tab order |
| `theme.css`, `icons.tsx` | Design tokens (SF Pro for the UI, New York for meeting titles, light and dark from macOS, reduced motion respected), base element styles, buttons by `data-variant` (`primary`, `record`, `danger`, `plain`), `.sr-only`. Inline SVG icons, always `aria-hidden` |
| `recording/RecordingBar.tsx` | Above every view: Language select, Start recording (ad hoc), the state mark and `role="timer"` "Recorded time" from `elapsed_s`, Pause/Resume, Stop, the saving line, error replies and the status `notice`, with Retry for an `error` notice that has a `folder` (FR-1 to FR-7). Recording shows a pulsing REC mark, a large timer and a decorative level meter; Start stays in the DOM while recording, disabled and visually hidden |
| `recording/useRecorderStatus.ts` | Polls `recorder_status` every second; the latest request wins; `setState` applies a command's returned state at once and drops polls already in flight. Also used by the event view |
| `recording/notices.ts`, `recording/attempt.ts` | The FR-2 permission texts; the notice text is the Rust side's `message`, never rebuilt from `kind`. `attempt` turns an error reply or a rejected call into the text shown |
| `calendar/Upcoming.tsx`, `calendar/useUpcoming.ts` | Today + 7 days by day ("Today" first), loading, empty, denied (FR-8) and error states; refetches on window focus and on `calendar-changed` (debounced 300 ms, no timer); a failed refetch keeps the last list. A day's `date` is read as a local date (`calendar/format.ts`). Rows show start and end time, title and calendar; today's past events are dimmed and today's current or next one gets a Now / Up next badge |
| `calendar/EventView.tsx` | One event (FR-10): details, attendees by name with a text response, notes as plain text, Start recording linked to the event (disabled while recording or more than 7 days away, FR-11), up to 20 "Earlier notes" that open in All notes. Focus goes to its heading, and back to the row on Back |
| `notes/list/NotesList.tsx` | All notes (FR-15, FR-16): a "Search meetings" box sending `notes_list {query}` 200 ms after the last key (blank is `null`), rows in the order the command returns as buttons that open the note, the PRD's empty and "No meetings match." texts, a `store_unavailable` alert with Rescan folders, other failures in a `role="status"` line above the last good list |
| `notes/list/useNotes.ts` | `notes_list` at once on a query change, every 3 s while `document.visibilityState` is `visible` and on becoming visible; the latest request wins and a reply asked before the current query is dropped |
| `notes/list/useRescanProgress.ts` | `rescan_status` on mount and every 1 s while `running`, shown as "Loading meetings… N of M" (FR-14); the list is refetched once when a run stops |
| `notes/list/format.ts` | Row labels: `recorded_at` read as local time, `H:MM:SS` / `M:SS`, Bengali / English, Browser / Import / Desktop, status word (the job stage while `processing`) |
| `notes/view/NoteView.tsx` | One note (FR-17 to FR-21), keyed by id: title (edited in place for `done`; the h2 stays as the focus target), one-line facts (date, duration, language, source; linked event on its own line; a calendar name with `@` is hidden), status, save line, vault banner with Retry, actions, then the editors (`Tabs`), with a side column of "My notes" first and the compact speakers list. Title, summary and speaker names are editable only for `done`; "My notes" always |
| `notes/view/useNote.ts` | `note_get`, latest request wins over older ones and over replies already applied. Polls every 3 s while `document.visibilityState` is `visible` and the note is `processing` or a rerun from this view is under 10 s old; skips a tick while a focused field holds unsaved text |
| `notes/view/useAutosave.ts` | Per field (`title`, `body_md`, `my_notes`, `speaker:<label>`) a draft and a 1 s timer; blur saves at once. One `note_update` in flight; a queued field sends its latest draft. A reply replaces the stored note, but a field typed into since keeps its draft. A rename first flushes the other fields; an empty name is never sent. "My notes" over 1 MB (UTF-8 bytes) is refused locally. Retry sends `{id}` only. Back, Archive, Trash, Rerun and unmount flush first; when a save was refused or failed, the first Back stays and says the change is not saved, the second leaves |
| `notes/view/Summary.tsx` | The only HTML sink: `body_html` in read mode (rendered by `render_html`, D25); a markdown textarea in edit mode. A test fails if `dangerouslySetInnerHTML` appears in any other file of the folder. A click on a link in it is cancelled: the window would otherwise load the remote page, and nothing opens the system browser yet |
| `notes/view/TranscriptTab.tsx`, `SpeakersPanel.tsx`, `MyNotes.tsx` | The note body (FR-11): a bar with the Summary/Transcript tabs (ARIA tabs, arrow keys, Home, End) and a player slot, sticking to the top of the page (with a band hiding text passing under it); then the selected panel beside a 300 px side column, which moves under the content below 900 px. One page scroll (D6): no part has its own scroll area. The selected tab (Summary by default) lives in `Tabs`, so polls and reloads keep it. The Summary's Edit/Done button is the first row of its panel; the transcript is shown as `note_get` sends it (labels already replaced by shown names), as text. Speakers (count in the heading): label and rename field, then shown name (`format.ts` `shownName` = `store::shown_name`) and evidence on one truncated line, full evidence on hover. Each `[hh:mm:ss] Name: text` line is a row with the speaker's name in a colour by order of first appearance; other lines are shown as they are |
| `notes/view/Actions.tsx`, `RerunDialog.tsx` | Open in Obsidian, Open folder and Rerun (both disabled with "Recording missing"), Archive and Trash (disabled while processing; Trash confirms for 3 s). Rerun with any edit mark opens a modal dialog: Keep my edits / Replace with new summary / Cancel (Escape), focus back on Rerun |
| `settings/Settings.tsx`, `settings/useSettingsInfo.ts` | Settings (FR-20): Calendars, Updates, Meetings (Rescan folders), Folders and Health sections, each loading and failing on its own. `settings_info` on mount and again when a rescan finishes; the latest request wins |
| `settings/CalendarsSection.tsx` | `calendar_status`, then `calendars` only when `granted`; the FR-8 denied text, a not-asked line, "No calendars found". A checked box is a shown calendar; a toggle shows at once and sends the full hidden id list through one promise chain (calls in order, the latest reply wins); a failed latest call (`store_unavailable` or a rejection, D42) returns to the last confirmed set with its message in a `role="status"` line. When access was never asked, Allow calendar access calls `calendar_events` (which shows the macOS prompt) and then reloads |
| `settings/UpdatesSection.tsx` | Updates (FR-25, FR-27): "Version X.Y.Z", Install updates automatically (shows at once, the latest save wins, a failed save, `store_unavailable` or a rejection, returns to the last confirmed value with its message in a `role="status"` line), Check for updates (disabled while `checking`, `downloading`, `ready` or `installing`). One `role="status"` line: "Checking…", "Downloading… N%" from `progress`, or "Meeting is up to date" only after a check pressed here ends with no release and no error. A release found shows "Version A.B.C is available", its notes as plain text and Install and relaunch, disabled while the recorder is not `idle` ("Finish the recording to install", linked with `aria-describedby`) or an update runs. `error`, `tool_error` and error replies (`recording_active`, `no_update`) are shown verbatim in `role="alert"`; a reply's alert clears when the recording ends |
| `settings/useUpdateStatus.ts` | `update_status` on mount and every 1 s while `checking`, `downloading`, `ready` or `installing`; the latest request wins; `reload` resolves to its own read's status |
| `settings/RescanSection.tsx` | Rescan folders, disabled while `rescan_status.running` (reuses `notes/list/useRescanProgress.ts`), "Loading meetings… N of M" then "Rescan finished" in a `role="status"` line, errors in a `role="alert"`; with a broken store, "Notes database can't be opened: <message>" above the button (D33) |
| `settings/FoldersSection.tsx` | Recordings directory and `vault_dir`/`vault_folder` as read-only text, "Change with `meeting install`"; a `not_configured` message instead |
| `settings/HealthSection.tsx` | `health` on mount and on Check again (disabled in flight): a list labelled "Health checks", each check's name, "OK"/"Failing" and a failing one's fix |

Conventions are the extension's: colocated `*.test.ts(x)`, CSS modules, Biome. No inline `style` attributes: the content security policy forbids them.

## Security

- **CSP** (`tauri.conf.json`): `default-src 'self'`, IPC only in `connect-src`, no remote origin, no `unsafe-inline`, no `unsafe-eval`. `devCsp` adds only the dev server's websocket.
- **Capability** (`capabilities/default.json`): window `main`, no permission. The app's own commands need none; no plugin is exposed to the webview, so no shell, file-system or HTTP access from the UI. The updater plugin is Rust side only (D47): no capability lists it.
- `withGlobalTauri` is off, no asset protocol, no window in the config (windows are created in code).
- `Info.plist` holds the Microphone, Audio Capture and Calendar usage strings. `Entitlements.plist` (`bundle.macOS.entitlements`) grants `device.audio-input` and `personal-information.calendars`: the bundle is signed with the hardened runtime, which otherwise refuses both without a prompt.
- Tests in `lib.rs` read these files and fail if they loosen.

## Dependencies

`desktop/src-tauri/Cargo.toml` lists every crate the PRD approves (D22, D25, D32, and `tauri-plugin-updater` by D47). The `objc2-*` framework crates start with no framework feature on; an issue that needs a class turns its cargo feature on and adds no crate. `@tauri-apps/api` and `@tauri-apps/cli` stay on the `tauri` crate's minor version (2.11): the Tauri CLI refuses to build when they differ.

## Gates

```sh
# desktop/
npm run build && npm run typecheck && npx biome ci . && npm test
# repo root, after the build above
cargo fmt --all --check && cargo clippy --all-targets -- -D warnings && cargo test
FEATURE_DESKTOP_APP=1 cargo test -p meeting-desktop
```

The Rust tests use Tauri's mock runtime (`tauri` feature `test`), which runs `setup` only inside its event loop, so they call `launch` directly.
