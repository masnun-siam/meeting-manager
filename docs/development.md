# Development

Covers: Cargo.toml, Cargo.lock, crates/meeting/Cargo.toml, extension/package.json, extension/package-lock.json, extension/vite.config.ts, extension/vitest.config.ts, extension/tsconfig.json, extension/biome.json, extension/.gitignore, desktop/package.json, desktop/package-lock.json, desktop/vite.config.ts, desktop/vitest.config.ts, desktop/tsconfig.json, desktop/biome.json, desktop/.gitignore, desktop/src-tauri/Cargo.toml, desktop/src-tauri/tauri.conf.json, .gitignore, .github/workflows/

## Prerequisites

| Tool | Why |
|---|---|
| macOS | Keychain (`keyring` apple-native), Chrome native-host path under `~/Library`, logs under `~/Library/Logs` |
| Rust ≥ 1.87, edition 2024 | `crates/meeting`, `desktop/src-tauri` |
| `ffmpeg` on `PATH` | `meeting install` requires it; `extract.rs` and `pipeline.rs` tests run the real binary on `tests/fixtures/short.webm` |
| `claude` (Claude Code) on `PATH` | `meeting install` requires it; summaries shell out to it |
| Node ≥ 24 | `extension/` and `desktop/` (`engines` in `package.json`) |
| macOS ≥ 14.4, Apple silicon | Running the desktop app (`minimumSystemVersion` in `desktop/src-tauri/tauri.conf.json`) |
| Chrome ≥ 116 | `minimum_chrome_version` in `extension/manifest.config.ts` |

## Commands

Rust, from the repo root (a workspace of two crates: `crates/meeting` and `desktop/src-tauri`). The desktop crate embeds `desktop/dist` at compile time, so run `npm ci && npm run build` in `desktop/` first; without it every workspace-wide `cargo` command fails with a missing `frontendDist` error (`cargo build -p meeting` still works):

| Command | Does |
|---|---|
| `cargo build` | Builds `target/debug/meeting` and `target/debug/meeting-desktop` |
| `FEATURE_MEETING_CONTEXT=1 cargo build` | Turns the meeting-context feature on (build-time; unset is off, Cargo rebuilds when it changes); see `FLAGS.md` |
| `FEATURE_DESKTOP_APP=1 cargo build` | Turns the desktop-app feature on (build-time; unset is off); see `FLAGS.md` |
| `cargo test` | Unit tests (in-module `#[cfg(test)]`) plus `crates/meeting/tests/*.rs` integration tests |
| `cargo fmt --all --check` | Format gate |
| `cargo clippy --all-targets -- -D warnings` | Lint gate; any warning fails |

Extension, from `extension/`:

| Command | Does |
|---|---|
| `npm ci` | Install from lockfile |
| `npm run dev` | Vite dev server with CRXJS hot reload |
| `npm run build` | Builds the unpacked extension into `extension/dist/` |
| `FEATURE_MEETING_CONTEXT=1 npm run build` | Same with the meeting-context feature on (manifest adds Meet access and the content script); unset is off; see `FLAGS.md` |
| `npm run typecheck` | `tsc --noEmit` |
| `npm run lint` | `biome check .` (CI runs `biome ci .`) |
| `npm test` | `vitest run` (jsdom, setup in `src/test/setup.ts`) |

Desktop, from `desktop/` (see [desktop.md](desktop.md)):

| Command | Does |
|---|---|
| `npm ci` | Install from lockfile |
| `npm run dev` | Vite dev server on port 1420, UI only (no Rust side, so every command call fails) |
| `npm run tauri dev` | Runs the app against the dev server; built off, it shows "Built without FEATURE_DESKTOP_APP" |
| `FEATURE_DESKTOP_APP=1 npm run tauri dev` | Runs the app with the feature on |
| `npm run build` | Builds the UI into `desktop/dist/` |
| `npm run typecheck` | `tsc --noEmit` |
| `npm run lint` | `biome check .` (CI runs `biome ci .`) |
| `npm test` | `vitest run` (jsdom, setup in `src/test/setup.ts`) |
| `FEATURE_DESKTOP_APP=1 npm run tauri build` | Builds `target/release/bundle/macos/Meeting.app` (ad-hoc signed) |

## Running it end to end

1. `cargo build`, then run `target/debug/meeting install`. It writes the Chrome native-host manifest pointing at the binary that ran it (`std::env::current_exe`), so re-run it if you move or rebuild to another path. See [features/setup.md](features/setup.md).
2. `meeting config set-key`, then `meeting doctor`.
3. `npm run build` in `extension/`, then load `extension/dist/` as an unpacked extension. The `key` in `manifest.config.ts` pins the extension ID the native host trusts; never change it.
4. Logs: `~/Library/Logs/meeting-manager/meeting.log` (the tool never logs to stdout; stdout is the native-messaging channel).

## CI gates

Three path-filtered workflows run on pull requests and pushes to `main`.

| Workflow | Runs on | Triggers on | Steps |
|---|---|---|---|
| `.github/workflows/tool.yml` | `macos-latest`, Node 24 | `Cargo.*`, `crates/**`, `desktop/src-tauri/**` | install ffmpeg → `npm ci` and `npm run build` in `desktop/` → `cargo fmt --all --check` → `cargo clippy --all-targets -- -D warnings` → `cargo test` |
| `.github/workflows/extension.yml` | `ubuntu-latest`, Node 24 | `extension/**` | `npm ci` → `npm run build` → `npx tsc --noEmit` → `npx biome ci .` → `npx vitest run` |
| `.github/workflows/desktop.yml` | `ubuntu-latest`, Node 24 | `desktop/**` | `npm ci` → `npm run build` → `npm run typecheck` → `npx biome ci .` → `npm test` |

- No git hooks are configured; run the matching gate commands locally before pushing.
- Formatting rules: [extension.md](extension.md#conventions) (biome), `cargo fmt` defaults (Rust).
