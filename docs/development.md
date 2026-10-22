# Development

Covers: Cargo.toml, Cargo.lock, crates/meeting/Cargo.toml, extension/package.json, extension/package-lock.json, extension/vite.config.ts, extension/vitest.config.ts, extension/tsconfig.json, extension/biome.json, extension/.gitignore, .gitignore, .github/workflows/

## Prerequisites

| Tool | Why |
|---|---|
| macOS | Keychain (`keyring` apple-native), Chrome native-host path under `~/Library`, logs under `~/Library/Logs` |
| Rust ≥ 1.87, edition 2024 | `crates/meeting` |
| `ffmpeg` on `PATH` | `meeting install` requires it; `extract.rs` and `pipeline.rs` tests run the real binary on `tests/fixtures/short.webm` |
| `claude` (Claude Code) on `PATH` | `meeting install` requires it; summaries shell out to it |
| Node ≥ 24 | `extension/` (`engines` in `package.json`) |
| Chrome ≥ 116 | `minimum_chrome_version` in `extension/manifest.config.ts` |

## Commands

Rust, from the repo root (single-crate workspace):

| Command | Does |
|---|---|
| `cargo build` | Builds `target/debug/meeting` |
| `FEATURE_MEETING_CONTEXT=1 cargo build` | Turns the meeting-context feature on (build-time; unset is off, Cargo rebuilds when it changes); see `FLAGS.md` |
| `cargo test` | Unit tests (in-module `#[cfg(test)]`) plus `crates/meeting/tests/*.rs` integration tests |
| `cargo fmt --all --check` | Format gate |
| `cargo clippy --all-targets -- -D warnings` | Lint gate; any warning fails |

Extension, from `extension/`:

| Command | Does |
|---|---|
| `npm ci` | Install from lockfile |
| `npm run dev` | Vite dev server with CRXJS hot reload |
| `npm run build` | Builds the unpacked extension into `extension/dist/` |
| `npm run typecheck` | `tsc --noEmit` |
| `npm run lint` | `biome check .` (CI runs `biome ci .`) |
| `npm test` | `vitest run` (jsdom, setup in `src/test/setup.ts`) |

## Running it end to end

1. `cargo build`, then run `target/debug/meeting install`. It writes the Chrome native-host manifest pointing at the binary that ran it (`std::env::current_exe`), so re-run it if you move or rebuild to another path. See [features/setup.md](features/setup.md).
2. `meeting config set-key`, then `meeting doctor`.
3. `npm run build` in `extension/`, then load `extension/dist/` as an unpacked extension. The `key` in `manifest.config.ts` pins the extension ID the native host trusts; never change it.
4. Logs: `~/Library/Logs/meeting-manager/meeting.log` (the tool never logs to stdout; stdout is the native-messaging channel).

## CI gates

Two path-filtered workflows run on pull requests and pushes to `main`.

| Workflow | Runs on | Triggers on | Steps |
|---|---|---|---|
| `.github/workflows/tool.yml` | `macos-latest` | `Cargo.*`, `crates/**` | install ffmpeg → `cargo fmt --all --check` → `cargo clippy --all-targets -- -D warnings` → `cargo test` |
| `.github/workflows/extension.yml` | `ubuntu-latest`, Node 24 | `extension/**` | `npm ci` → `npm run build` → `npx tsc --noEmit` → `npx biome ci .` → `npx vitest run` |

- No git hooks are configured; run the matching gate commands locally before pushing.
- Formatting rules: [extension.md](extension.md#conventions) (biome), `cargo fmt` defaults (Rust).
