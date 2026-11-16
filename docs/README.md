# Docs

Covers: docs/README.md

Synced to: 0f60722448f65a7fd91e2d0ed644dfbf440ff1d5 (2026-11-17)

Written for agents about to change the code. Start with architecture, then the doc for the area you touch.

| Doc | Read when |
|---|---|
| [architecture.md](architecture.md) | First: components, how they talk, data stores, main flow |
| [development.md](development.md) | Setting up (`make setup`), `make` targets, git hooks, CI gates, releasing, agent tooling |
| [tool.md](tool.md) | Changing the Rust crate: module map, layering, conventions, tests |
| [desktop.md](desktop.md) | Changing the desktop app: module map, command contract, capture boundary, flag, gates |
| [extension.md](extension.md) | Changing the Chrome extension: entry points, messages, storage, conventions |
| [modules/recorder/browser-recording.md](modules/recorder/browser-recording.md) | Recording in Chrome: pause/stop, mic permission, saving, leftover recovery |
| [modules/recorder/native-messaging.md](modules/recorder/native-messaging.md) | Extension ↔ tool protocol, hand-off, Jobs tab |
| [modules/recorder/meeting-recorder.md](modules/recorder/meeting-recorder.md) | `meeting process`: ffmpeg, Deepgram, Claude summary, vault note, job states |
| [modules/recorder/recording-folders.md](modules/recorder/recording-folders.md) | Archive and Trash of a recording folder |
| [modules/recorder/recording-import.md](modules/recorder/recording-import.md) | `meeting import` of a recording made outside Chrome |
| [modules/recorder/setup.md](modules/recorder/setup.md) | `meeting install`, config file, Deepgram key, `meeting doctor` |
| [modules/context/meeting-context.md](modules/context/meeting-context.md) | Meet roster, calendar match and the context block Claude reads (`FEATURE_MEETING_CONTEXT`) |
| [modules/desktop/calendar.md](modules/desktop/calendar.md) | Desktop calendar module: EventKit, upcoming list, filters, ids, `calendar-changed` |
| [modules/desktop/recorder.md](modules/desktop/recorder.md) | Desktop recording session: start checks, WAV, pause, stops, conversion, hand-off, notices |
| [guides/installation.md](guides/installation.md), [guides/quickstart.md](guides/quickstart.md) | User guides (contributing is `CONTRIBUTING.md` at the root) |
| [prds/](prds/) | Product requirements (owned by `/blubird:create-prd`; module and feature names match `modules/`) |
