# Docs

Covers: docs/

Synced to: eb81699797149693913c5996b3741ea8736a0016 (2026-10-19)

Written for agents about to change the code. Start with architecture, then the doc for the area you touch.

| Doc | Read when |
|---|---|
| [architecture.md](architecture.md) | First: components, how they talk, data stores, main flow |
| [development.md](development.md) | Setting up, running checks, loading the extension, CI gates |
| [tool.md](tool.md) | Changing the Rust crate: module map, layering, conventions, tests |
| [desktop.md](desktop.md) | Changing the desktop app: module map, command contract, capture boundary, flag, gates |
| [extension.md](extension.md) | Changing the Chrome extension: entry points, messages, storage, conventions |
| [features/recording.md](features/recording.md) | Recording, pause/stop, mic permission, saving, leftover recovery |
| [features/native-messaging.md](features/native-messaging.md) | Extension ↔ tool protocol, hand-off, Jobs tab |
| [features/processing.md](features/processing.md) | `meeting process`: ffmpeg, Deepgram, Claude summary, vault note, job states |
| [features/setup.md](features/setup.md) | `meeting install`, config file, Deepgram key, `meeting doctor` |
| [guides/](guides/) | User guides: installation, quickstart (contributing is `CONTRIBUTING.md` at the root) |
| [prds/](prds/) | Product requirements (owned by `/create-prd`) |
