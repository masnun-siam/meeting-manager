# Meeting Manager

Records a meeting in Chrome, transcribes it with Deepgram, summarises it with headless `claude` and writes a note into an Obsidian vault. macOS only, no server: everything runs on the user's machine. Single-repo product: this repo is its own Documentation Repo (`.blubird/repos.json`).

## Architecture

| Part | Path | Doc |
|---|---|---|
| Chrome MV3 extension (React 19, Vite, CRXJS) | `extension/` | `docs/extension.md` |
| Rust CLI and native-messaging host `meeting` | `crates/meeting/` | `docs/tool.md` |
| Desktop app (Tauri 2 + React 19), links the `meeting` library | `desktop/src-tauri/`, `desktop/src/` | `docs/desktop.md` |

Flow: extension records → `chrome.downloads` → native message → host writes a job and spawns `meeting process` → extract (ffmpeg) → transcribe → summarise → vault note. Map: `docs/architecture.md`.

## Principles

- Never change `key` in `extension/manifest.config.ts`: it fixes the ID in `EXTENSION_ID` (`crates/meeting/src/install.rs`).
- `extension/src/shared/native.ts` mirrors the Rust types in `native.rs` and `jobs.rs` by hand: change both together.
- Secrets (Deepgram key, calendar address) live only in the Keychain (`secret.rs`); never log transcript text or calendar values.
- New features ship dark behind build-time `FEATURE_*` flags (`FLAGS.md`, `main.rs`); merging is not releasing.
- Tests never touch the real home: use `test_support` temp dirs; stub executables via `write_script`.
- Gates run in Docker as non-root, except `desktop/src-tauri` (host only, macOS APIs).

## Commands

| Job | Command |
|---|---|
| Once per clone (host tools, hook image, git hooks) | `make setup` |
| Run: extension build (Docker) + desktop app (host); `AGENT=<name>` gets its own port | `make up` |
| Every gate, as CI | `make check` |
| Lint / format / tests only | `make lint` / `make fmt` / `make test` |
| Tooling contract tests | `make test-tooling` |
| Health check of the installed tool | `make doctor` |

Toolchains: Rust `rust-toolchain.toml` (1.99.1; do not bump past it, see `docs/development.md`), Node `.nvmrc` (24). Never skip hooks.

## References

- Workflow: the `blubird` plugin (`.claude/settings.json`); PRs into `main` (no `staging`, owner's decision on #186); only the owner merges.
- PRDs: `docs/prds/<module>/<feature>.md`; technical docs: `docs/` (index `docs/README.md`); setup and gates: `docs/development.md`.
- Lessons: `tasks/lessons.md`. Code intelligence: GitNexus MCP (`.mcp.json`).
