# Desktop app

Covers: desktop/

The Meeting desktop app: a Tauri 2 app whose Rust side (`desktop/src-tauri`, package `meeting-desktop`, lib `meeting_desktop`) links the `meeting` library and whose UI (`desktop/src`) is React 19 with strict TS and CSS modules. It is a scaffold: the window, the navigation, the command contract and the module names exist; every command answers `not_implemented`. Behaviour: [prds/desktop-app.md](prds/desktop-app.md).

## Flag

`FEATURE_DESKTOP_APP` is read at build time (`option_env!` in `lib.rs`, on only for the value `1`; see `FLAGS.md`).

| Built | Window | Registered |
|---|---|---|
| Off (default) | label `off`, page `off.html` ("Built without FEATURE_DESKTOP_APP"), no capability | Nothing: no state, no command, no plugin, no hook |
| On | label `main`, page `index.html` | The single-instance plugin, the `App` state, the 22 commands, the launch hooks |

## Rust side (`desktop/src-tauri/src`)

| File | Holds |
|---|---|
| `main.rs` | Calls `meeting_desktop::run()`. The binary is `meeting-desktop`; the CLI owns the name `meeting` |
| `lib.rs` | `run()`, the flag, `configure` (setup and command registration), `launch` (state, window, hooks), the `App` state `{env, recorder, calendar, notes, admin}`. The issues that fill a module do not edit it |
| `env.rs` | `Env { home, store_path, jobs }`; `config()` reloads `config.toml` on every call and returns the error as a string; `store()` opens the notes store and keeps `anyhow::Error`, so `store::OpenError` can be downcast. `Env::at(home)` is the constructor tests use |
| `reply.rs` | `ok(fields)`, `err(code, message)`, `not_implemented(command)`: the reply shape of every command |
| `recorder/mod.rs` | `Recorder`; `recorder_status`, `recorder_start`, `recorder_pause`, `recorder_resume`, `recorder_stop`, `recorder_retry`; hooks `on_launch`, `on_sleep`, `on_exit_requested` (`false` keeps the app running) |
| `calendar/mod.rs` | `Calendar`; `calendar_status`, `calendar_events`, `calendar_event`, `calendars`, `set_hidden_calendars`; hook `on_launch` |
| `notes/mod.rs` | `Notes`; `notes_list`, `note_get`, `note_update`, `note_rerun`, `note_open` |
| `admin/mod.rs` | `Admin`; `rescan`, `rescan_status`, `health`, `note_archive`, `note_trash`, `settings_info`; hook `on_launch` |
| `tray.rs` | Hook `setup` (the menu-bar item) |
| `capture/` | Audio capture: the boundary in `mod.rs` and the Core Audio implementation (below) |

- **Startup order** (`launch`, flag on): manage `App`, open the window, then `tray::setup`, `recorder::on_launch`, `admin::on_launch`, `calendar::on_launch`, `recorder::on_sleep`. Hooks take `&AppHandle<R>` for any runtime so tests run them on Tauri's mock runtime.
- **Single instance:** `tauri-plugin-single-instance` is added in `run()`, before everything else, and only there: on macOS a second instance in one process exits it, which would end the test process. A second launch focuses the `main` window.
- **Quit:** with the flag on, `run()` calls `recorder::on_exit_requested` on `RunEvent::ExitRequested` and prevents the exit when it returns `false`. Built off, it is not called.
- **Logging:** `meeting::logging::init()`, the same log file and rules as the tool. Stubs log nothing.

## Command contract

The PRD's API table (Technical design › API) is the contract. Every command is `#[tauri::command(rename_all = "snake_case")]`, returns `serde_json::Value`, and answers `{ok: true, ...}` or `{ok: false, code, message}`.

- `desktop/src/api.ts` has one typed function per command, the request and reply types, and each command's error codes as a string-literal union. Argument keys are snake_case, exactly the Rust argument names. A reply without a boolean `ok` throws `Invalid reply from the Meeting app`; an error reply is returned, not thrown.
- `desktop/src/api.mock.ts`: `mockApi(overrides)` for component tests (`vi.mock('./api', async () => (await import('./api.mock')).mockApi())`).
- A test in `lib.rs` checks that `api.ts` names the same 22 commands. A new command changes the contract: PRD first.

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

## UI (`desktop/src`)

| File | Holds |
|---|---|
| `App.tsx` | Sidebar (Upcoming, All notes, Settings) as buttons with `aria-current`, `RecordingBar` above every view, the open note id |
| `recording/RecordingBar.tsx`, `calendar/Upcoming.tsx`, `notes/list/NotesList.tsx`, `notes/view/NoteView.tsx`, `settings/Settings.tsx` | One placeholder per view; each UI issue owns its folder |

Conventions are the extension's: colocated `*.test.ts(x)`, CSS modules, Biome. No inline `style` attributes: the content security policy forbids them.

## Security

- **CSP** (`tauri.conf.json`): `default-src 'self'`, IPC only in `connect-src`, no remote origin, no `unsafe-inline`, no `unsafe-eval`. `devCsp` adds only the dev server's websocket.
- **Capability** (`capabilities/default.json`): window `main`, no permission. The app's own commands need none; no plugin is exposed to the webview, so no shell, file-system or HTTP access from the UI.
- `withGlobalTauri` is off, no asset protocol, no window in the config (windows are created in code).
- `Info.plist` holds the Microphone, Audio Capture and Calendar usage strings.
- Tests in `lib.rs` read these files and fail if they loosen.

## Dependencies

`desktop/src-tauri/Cargo.toml` lists every crate the PRD approves (D22, D25, D32). The `objc2-*` framework crates start with no framework feature on; an issue that needs a class turns its cargo feature on and adds no crate. `@tauri-apps/api` and `@tauri-apps/cli` stay on the `tauri` crate's minor version (2.11): the Tauri CLI refuses to build when they differ.

## Gates

```sh
# desktop/
npm run build && npm run typecheck && npx biome ci . && npm test
# repo root, after the build above
cargo fmt --all --check && cargo clippy --all-targets -- -D warnings && cargo test
FEATURE_DESKTOP_APP=1 cargo test -p meeting-desktop
```

The Rust tests use Tauri's mock runtime (`tauri` feature `test`), which runs `setup` only inside its event loop, so they call `launch` directly.
