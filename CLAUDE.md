# Meeting Manager

Temporary stub during agentic onboarding (#186); the full constitution replaces it.

- Rust workspace (`Cargo.toml`): `crates/meeting` (CLI and Chrome native-messaging host) and `desktop/src-tauri` (Tauri app).
- `extension/`: Chrome MV3 extension (`extension/manifest.config.ts`); `desktop/`: the desktop app's React UI.
- Never change `key` in `extension/manifest.config.ts`: it fixes the ID in `EXTENSION_ID` (`crates/meeting/src/install.rs`).
- PRDs: `docs/prds/`. Workflow: the `blubird` plugin's engineering workflow; PRs into `main`.
