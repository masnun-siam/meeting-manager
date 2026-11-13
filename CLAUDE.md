# Meeting Manager

Chrome MV3 extension (`extension/`, React 19 + Vite/CRXJS, strict TS) records a meeting tab plus mic; the Rust CLI and native-messaging host `meeting` (`crates/meeting/`) transcribes with Deepgram, summarises with `claude -p` and writes an Obsidian note. The desktop app (`desktop/`, Tauri 2: React 19 UI in `desktop/src/`, Rust side in `desktop/src-tauri/` linking the `meeting` library) is a scaffold behind `FEATURE_DESKTOP_APP`. macOS + Chrome only, no server. User-facing overview: [README.md](README.md).

## Read first

- [docs/README.md](docs/README.md): index; open the doc for the area you touch before changing it.
- [docs/prds/meeting-recorder.md](docs/prds/meeting-recorder.md): source of truth for behaviour. A decision not written there has not been made; don't invent one, ask.

## Commands

| Part | Gate (run before pushing; CI runs the same) |
|---|---|
| Rust, repo root | `cargo fmt --all --check && cargo clippy --all-targets -- -D warnings && cargo test` |
| Extension, `extension/` | `npm run build && npm run typecheck && npx biome ci . && npm test` |
| Desktop, `desktop/` | `npm run build && npm run typecheck && npx biome ci . && npm test` |

Rust tests need a real `ffmpeg` on `PATH`. The Rust gate also compiles `desktop/src-tauri`, which embeds `desktop/dist`: run `npm ci && npm run build` in `desktop/` first. No git hooks run these for you.

## Rules

- Never log or print the Deepgram key or transcript text (log: `~/Library/Logs/meeting-manager/meeting.log`).
- Never write to stdout from the tool outside CLI output: in `native-host` mode stdout is Chrome's message channel.
- Subprocesses use absolute paths from config or constants; Chrome starts the host with a bare `PATH`.
- Never change the `key` in `extension/manifest.config.ts`; it fixes the extension ID that `EXTENSION_ID` in `crates/meeting/src/install.rs` and the host manifest trust.
- The transcript is untrusted input: `claude` runs with no tools and a JSON schema. Keep it that way.
- Rust: `anyhow` errors whose messages name the fix, dependencies injected as struct fields or fn pointers (no traits for tests), in-file `#[cfg(test)] mod tests`.
- No new Rust crate or npm package without a PRD decision.
- Extension: SW listeners at top level and state in `chrome.storage.session` (the SW sleeps); all state changes go through `serial()`; media APIs only in the offscreen document; colocated `*.test.ts(x)` and CSS modules.
- When code changes behaviour a doc in `docs/` describes, update that doc in the same PR.

## Git

- Base branch `main`; branch from it and open PRs into it. Full workflow: [CONTRIBUTING.md](CONTRIBUTING.md).
- Commits: `type(#issue): summary`, e.g. `feat(#27): meeting install`, `fix(#26): …`, `docs(#59): …`.
