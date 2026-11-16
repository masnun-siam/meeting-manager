# Architecture

Covers: Cargo.toml, FLAGS.md

A Chrome extension records a meeting tab plus the mic; a local Rust CLI (`meeting`) transcribes it with Deepgram, summarises it with headless Claude Code and writes a note into an Obsidian vault. macOS only. No server: everything runs on the user's machine.

## Components

| Component | Path | Doc |
|---|---|---|
| Chrome MV3 extension (React 19, Vite, CRXJS) | `extension/` | [extension.md](extension.md) |
| Rust CLI and native-messaging host `meeting` | `crates/meeting/` | [tool.md](tool.md) |
| Desktop app `Meeting` (Tauri 2, React 19), a scaffold behind `FEATURE_DESKTOP_APP`: it links the `meeting` library and does nothing yet | `desktop/` | [desktop.md](desktop.md) |

```mermaid
flowchart LR
  subgraph Chrome
    P[Popup] --> SW[Service worker]
    SW <--> O[Offscreen recorder]
  end
  O -- chunks --> OPFS[(OPFS temp file)]
  SW -- chrome.downloads --> DL[(~/Downloads/Meetings/#lt;name#gt;/recording.webm)]
  SW -- native messaging --> H[meeting native-host]
  H -- spawns detached --> PR[meeting process]
  H <--> J[(jobs/*.json)]
  PR --> J
  PR --> FF[ffmpeg]
  PR --> DG[Deepgram API]
  PR --> CL[claude CLI]
  PR --> V[(Obsidian vault note)]
  PR --> N[osascript notification]
  DL --> PR
```

## How they talk

- **Extension internals:** `chrome.runtime` messages between popup, service worker and offscreen document; status in `chrome.storage.session`. See [extension.md](extension.md#message-passing).
- **Extension → tool:** Chrome native messaging (`com.meeting_manager.host`). Chrome launches `meeting chrome-extension://<id>/` per request, which `main.rs` runs as `native-host`; the host only accepts the pinned extension origin. See [recorder/native-messaging.md](modules/recorder/native-messaging.md).
- **Tool → tool:** the host never processes a recording itself; it writes a `queued` job and spawns `meeting process --job-id <id>` in its own process group, then exits.
- **Tool → extension:** no push. The popup Jobs tab polls `list_jobs`; the tool reads and writes job JSON files that both sides see through the host.

## Data stores

| Store | Owner | Doc |
|---|---|---|
| `chrome.storage.session` / `.local`, OPFS `recordings/` | Extension | [extension.md](extension.md#state-storage) |
| Recording and side files in the recording's folder (`meta.json`, `transcript.md`, `summary.md`, …) | Tool | [recorder/meeting-recorder.md](modules/recorder/meeting-recorder.md#files) |
| Job files under `~/Library/Application Support/meeting-manager/jobs/` | Tool | [recorder/meeting-recorder.md](modules/recorder/meeting-recorder.md#job-lifecycle) |
| `~/.config/meeting-manager/config.toml`, Keychain items (Deepgram key; calendar address, flag on), Chrome host manifest | Tool | [recorder/setup.md](modules/recorder/setup.md) |
| Vault note | Tool | [recorder/meeting-recorder.md](modules/recorder/meeting-recorder.md#files) |
| Log `~/Library/Logs/meeting-manager/meeting.log` | Tool | [tool.md](tool.md#gotchas) |

## Main flow

```mermaid
sequenceDiagram
  actor U as User
  participant E as Extension
  participant H as native-host
  participant P as meeting process
  U->>E: Start, then Stop & save
  E->>E: record, download .webm
  E->>H: process {path, language}
  H->>P: spawn with job id
  H-->>E: {ok, job_id}
  P->>P: extract → transcribe → summarise → write note
  opt flag on, context.json with a meeting ID, calendar address stored
    P->>P: fetch the calendar (every run), match the event, add it to the block
  end
  P-->>U: macOS notification
  E->>H: list_jobs (Jobs tab, every 3 s)
```

Details per stage: [recorder/browser-recording.md](modules/recorder/browser-recording.md) → [recorder/native-messaging.md](modules/recorder/native-messaging.md) → [recorder/meeting-recorder.md](modules/recorder/meeting-recorder.md). First-run setup: [recorder/setup.md](modules/recorder/setup.md).

## Cross-cutting rules

- **Pinned extension ID.** The `key` in `extension/manifest.config.ts` fixes the ID that `EXTENSION_ID` in `crates/meeting/src/install.rs` trusts. Changing one breaks the host handshake.
- **Shared wire types.** `extension/src/shared/native.ts` mirrors the Rust request and job types in `native.rs` and `jobs.rs` by hand; change both sides together.
- **No secrets in files or logs.** The Deepgram key and the calendar address live only in the Keychain; transcript text, the address and calendar or meeting values are never logged.
