# Development

Covers: Cargo.toml, Cargo.lock, crates/meeting/Cargo.toml, extension/package.json, extension/package-lock.json, extension/vite.config.ts, extension/vitest.config.ts, extension/tsconfig.json, extension/biome.json, extension/.gitignore, desktop/package.json, desktop/package-lock.json, desktop/vite.config.ts, desktop/vitest.config.ts, desktop/tsconfig.json, desktop/biome.json, desktop/.gitignore, desktop/src-tauri/Cargo.toml, desktop/src-tauri/tauri.conf.json, .gitignore, .github/workflows/, Makefile, docker/hooks.Dockerfile, scripts/hooks/, scripts/repo-guards/, rust-toolchain.toml, .nvmrc, .pre-commit-config.yaml, .github/dependabot.yml, .mcp.json, .claude/settings.json, tasks/lessons.md, .blubird/repos.json

## Prerequisites

| Tool | Why |
|---|---|
| macOS | Keychain (`keyring` apple-native), Chrome native-host path under `~/Library`, logs under `~/Library/Logs` |
| Rust 1.99.1 (`rust-toolchain.toml`, rustup installs it), edition 2024 | `crates/meeting`, `desktop/src-tauri`; the same pin as `release.yml` |
| Docker | Hooks and `make check` run the `meeting` crate, the extension and the desktop UI in `docker/hooks.Dockerfile` |
| `pre-commit` | Installs the git hooks (`make setup`) |
| `ffmpeg` on `PATH` | `meeting install` requires it; `extract.rs` and `pipeline.rs` tests run the real binary on `tests/fixtures/short.webm` |
| `claude` (Claude Code) on `PATH` | `meeting install` requires it; summaries shell out to it |
| Node 24 (`.nvmrc`) | `extension/` and `desktop/` (`engines` in `package.json`); host Node only for `make desktop` (`tauri dev`) |
| macOS ≥ 14.4, Apple silicon | Running the desktop app (`minimumSystemVersion` in `desktop/src-tauri/tauri.conf.json`) |
| Chrome ≥ 116 | `minimum_chrome_version` in `extension/manifest.config.ts` |

## One command per job

From the repo root (`make help` lists them). The `meeting` crate, the extension and the desktop UI run in the hook image; the Tauri crate and the running app need macOS and run on the host (`scripts/hooks/host-rust.sh`).

| Command | Does |
|---|---|
| `make setup` | Once per clone: host `cargo-llvm-cov` (`scripts/hooks/install-host-tools.sh`), the hook image, the git hooks |
| `make up` | Builds the extension in Docker (`extension/dist/`), then runs the desktop app with `tauri dev` on the host. `AGENT=<name>` moves the Vite port off 1420 (`1421 + cksum % 500`); `make up all` and `with=<repo>` start the same (one repo) |
| `make doctor` | `meeting doctor` from source: health check of config, Keychain, ffmpeg, `claude`, Chrome host |
| `make check` | Every gate on every file, as CI: secrets, fmt, clippy, types, biome, full suites with coverage (`scripts/hooks/run.sh all`) |
| `make lint` / `make fmt` / `make test` | Lint as CI; format and fix; full suites without coverage |
| `make test-tooling` | Contract tests of the hooks, Makefile and CI pins (`scripts/hooks/tests/run.sh`, `scripts/repo-guards/tests/run.sh`) |
| `make clean-docker` | Removes this project's hook containers, volumes and images |

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

1. `cargo build`, then run `target/debug/meeting install`. It writes the Chrome native-host manifest pointing at the binary that ran it (`std::env::current_exe`), so re-run it if you move or rebuild to another path. See [recorder/setup.md](modules/recorder/setup.md).
2. `meeting config set-key`, then `meeting doctor`.
3. `npm run build` in `extension/`, then load `extension/dist/` as an unpacked extension. The `key` in `manifest.config.ts` pins the extension ID the native host trusts; never change it.
4. Logs: `~/Library/Logs/meeting-manager/meeting.log` (the tool never logs to stdout; stdout is the native-messaging channel).

## CI gates

Three path-filtered workflows run on pull requests and pushes to `main`, `guards.yml` on every pull request; `release.yml` runs only on a version tag (see [Releasing](#releasing)).

| Workflow | Runs on | Triggers on | Steps |
|---|---|---|---|
| `.github/workflows/tool.yml` | `macos-latest`, Node 24 | `Cargo.*`, `rust-toolchain.toml`, `crates/**`, `desktop/src-tauri/**`, `scripts/repo-guards/**` | install ffmpeg → `npm ci` and `npm run build` in `desktop/` → `cargo fmt --all --check` → `cargo clippy --all-targets -- -D warnings` → `cargo llvm-cov --workspace` → coverage gates |
| `.github/workflows/extension.yml` | `ubuntu-latest`, Node 24 | `extension/**`, `scripts/repo-guards/**` | `npm ci` → `npm run build` → `npx tsc --noEmit` → `npx biome ci .` → `npx vitest run --coverage` → coverage gates |
| `.github/workflows/desktop.yml` | `ubuntu-latest`, Node 24 | `desktop/**`, `scripts/repo-guards/**` | `npm ci` → `npm run build` → `npm run typecheck` → `npx biome ci .` → `npx vitest run --coverage` → coverage gates |
| `.github/workflows/release.yml` | `macos-latest`, Node 24 | a pushed tag `v*.*.*` | version check (tag = `tauri.conf.json` = `meeting` = `meeting-desktop`, else fail before building) → `npm ci` in `desktop/` → `cargo build --release -p meeting` → `npx tauri build --config src-tauri/tauri.release.conf.json` (all with `FEATURE_DESKTOP_APP=1`) → write `latest.json` → `gh release create` with the three assets |

| `.github/workflows/guards.yml` | `ubuntu-latest` | every pull request | gitleaks 8.30.1 over the PR's commits; `make test-tooling`; the PR body references its issue (`Closes`/`Fixes`/`Refs #n`; Dependabot exempt) |

`tool.yml` takes Rust from `rust-toolchain.toml` and runs `cargo llvm-cov --workspace` instead of `cargo test`; `extension.yml` and `desktop.yml` run `vitest run --coverage`. On pull requests each then requires ≥80% of changed lines covered (`scripts/repo-guards/changed-lines-coverage.py`, base `origin/main`), after `coverage-log-guard.sh` proves the report is complete.

## Git hooks

`make setup` installs them (`.pre-commit-config.yaml`). They check only changed files, in the hook image (`scripts/hooks/docker.sh`: image tagged by the Dockerfile's hash, `node_modules` volumes keyed by lockfile, non-root user, containers named `meeting-manager-hook-<pid>-<start>`). Docker down fails the hook; there is no host fallback.

| Stage | Checks (`scripts/hooks/gates.sh`) |
|---|---|
| pre-commit | no commits on `main`; only the root `CLAUDE.md`; no generated agent files; secrets in staged lines; no new lint, type or test suppressions (`#[allow`, `@ts-ignore`, `biome-ignore`, `it.skip`, …); no file over 500 KB; no conflict markers; `rustfmt`; `biome check` |
| pre-push | no push to `main`; files changed since `origin/main`: secrets in the branch's commits, the commit checks again, clippy, `tsc`, the related tests with coverage and ≥80% of changed lines covered; then the Tauri crate on the host only when `desktop/src-tauri/` changed (`make check` and CI always build it) |

- Workers: `max(1, nproc - 2)` where the work runs; `HOOK_JOBS` overrides.
- Two `meeting` tests need macOS and are `#[cfg(target_os = "macos")]`: they run in macOS CI only.
- Test stubs that are exec'd are written with `test_support::write_script`, never `fs::write`: on Linux a parallel test's fork can hold the write descriptor and the exec fails with "Text file busy".
- Pre-push skips the Tauri crate unless `desktop/src-tauri/` changed (owner decision, #186): a `crates/meeting` change that breaks it is caught by `make check` and CI. A new worktree's first push that touches the Tauri crate builds it cold (about 8 min).
- Formatting rules: [extension.md](extension.md#conventions) (biome), `cargo fmt` defaults (Rust).

## Releasing

The app and the `meeting` tool share one version `X.Y.Z` (PRD D46), released by pushing a tag (FR-24).

1. Set `X.Y.Z` in `desktop/src-tauri/tauri.conf.json`, `crates/meeting/Cargo.toml` and `desktop/src-tauri/Cargo.toml` (then `cargo check` to update `Cargo.lock`), and merge that into `main`. `the_versions_match` in `desktop/src-tauri/src/lib.rs` fails while they differ.
2. Tag `main` with an annotated tag; its message becomes the release notes: `git tag -a vX.Y.Z -m "<notes>"`, then `git push origin vX.Y.Z`.
3. `release.yml` publishes the GitHub Release `vX.Y.Z` with `Meeting.app.tar.gz` (the app, with `meeting` in `Contents/Resources/`), `Meeting.app.tar.gz.sig` and `latest.json`, the file the app's updater reads. A tag that differs from any of the three versions fails before building and publishes nothing.

- **Secrets:** `TAURI_SIGNING_PRIVATE_KEY` and `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` (Actions secrets, from `npx tauri signer generate`). Only the release build sees them; the public key is `plugins.updater.pubkey` in `tauri.conf.json`. Replacing the key pair means every installed app must be reinstalled by hand.
- **Toolchain:** `release.yml` pins Rust 1.99.1, and `rust-toolchain.toml` pins the same for the host, the hook image and CI. On Rust 1.100 or later, `tauri-build` 2.6 copies the bundled `meeting` into the wrong directory and the build fails with "Is a directory". Move both pins (and `docker/hooks.Dockerfile`) together once `tauri-build` is upgraded.
- **Release-only settings** (`createUpdaterArtifacts`, bundling `target/release/meeting`) live in `desktop/src-tauri/tauri.release.conf.json` (D50), so a local `npm run tauri build` needs no key and no prebuilt `meeting`. To try a release build locally, build the tool first (`FEATURE_DESKTOP_APP=1 cargo build --release -p meeting`) and pass a throwaway key from `npx tauri signer generate -w <scratch path>`.

## Agent tooling

- `.claude/settings.json` enables the `blubird` plugin; `.mcp.json` runs GitNexus 1.6.12 over MCP. Build its index with `npx gitnexus@1.6.12 analyze --index-only` (plain `analyze` writes `CLAUDE.md`).
- `.blubird/repos.json`: this repo is the whole product and its own Documentation Repo (PRDs in `docs/prds/`).
- `tasks/lessons.md`: short lessons, read at the start of every skill.
- `.github/dependabot.yml`: weekly version updates into `main` for cargo, both npm packages, Actions and the hook image.
