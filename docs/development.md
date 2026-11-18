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
| `FEATURE_DESKTOP_APP=1 npm run tauri build` | Builds `target/release/bundle/macos/Meeting.app` (ad-hoc signed locally, certificate-signed in releases) |

## Running it end to end

1. `cargo build`, then run `target/debug/meeting install`. It writes the Chrome native-host manifest pointing at the binary that ran it (`std::env::current_exe`), so re-run it if you move or rebuild to another path. See [features/setup.md](features/setup.md).
2. `meeting config set-key`, then `meeting doctor`.
3. `npm run build` in `extension/`, then load `extension/dist/` as an unpacked extension. The `key` in `manifest.config.ts` pins the extension ID the native host trusts; never change it.
4. Logs: `~/Library/Logs/meeting-manager/meeting.log` (the tool never logs to stdout; stdout is the native-messaging channel).

## CI gates

Three path-filtered workflows run on pull requests and pushes to `main`; `release.yml` runs only on a version tag (see [Releasing](#releasing)).

| Workflow | Runs on | Triggers on | Steps |
|---|---|---|---|
| `.github/workflows/tool.yml` | `macos-latest`, Node 24 | `Cargo.*`, `crates/**`, `desktop/src-tauri/**` | install ffmpeg → `npm ci` and `npm run build` in `desktop/` → `cargo fmt --all --check` → `cargo clippy --all-targets -- -D warnings` → `cargo test` |
| `.github/workflows/extension.yml` | `ubuntu-latest`, Node 24 | `extension/**` | `npm ci` → `npm run build` → `npx tsc --noEmit` → `npx biome ci .` → `npx vitest run` |
| `.github/workflows/desktop.yml` | `ubuntu-latest`, Node 24 | `desktop/**` | `npm ci` → `npm run build` → `npm run typecheck` → `npx biome ci .` → `npm test` |
| `.github/workflows/release.yml` | `macos-latest`, Node 24 | a pushed tag `v*.*.*` | version check (tag = `tauri.conf.json` = `meeting` = `meeting-desktop`, else fail before building) → `npm ci` in `desktop/` → `cargo build --release -p meeting` → `npx tauri build --config src-tauri/tauri.release.conf.json` (all with `FEATURE_DESKTOP_APP=1`) → write `latest.json` → `gh release create` with the three assets |

- No git hooks are configured; run the matching gate commands locally before pushing.
- Formatting rules: [extension.md](extension.md#conventions) (biome), `cargo fmt` defaults (Rust).

## Releasing

The app and the `meeting` tool share one version `X.Y.Z` (PRD D46), released by pushing a tag (FR-24).

1. Set `X.Y.Z` in `desktop/src-tauri/tauri.conf.json`, `crates/meeting/Cargo.toml` and `desktop/src-tauri/Cargo.toml` (then `cargo check` to update `Cargo.lock`), and merge that into `main`. `the_versions_match` in `desktop/src-tauri/src/lib.rs` fails while they differ.
2. Tag `main` with an annotated tag; its message becomes the release notes: `git tag -a vX.Y.Z -m "<notes>"`, then `git push origin vX.Y.Z`.
3. `release.yml` publishes the GitHub Release `vX.Y.Z` with `Meeting.app.tar.gz` (the app, with `meeting` in `Contents/Resources/`), `Meeting.app.tar.gz.sig` and `latest.json`, the file the app's updater reads. A tag that differs from any of the three versions fails before building and publishes nothing.

- **Secrets:** `TAURI_SIGNING_PRIVATE_KEY` and `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` (Actions secrets, from `npx tauri signer generate`). Only the release build sees them; the public key is `plugins.updater.pubkey` in `tauri.conf.json`. Replacing the key pair means every installed app must be reinstalled by hand.
- **Code signing (#239):** releases are signed with one long-lived self-signed certificate, so the designated requirement (`identifier "com.masnunsiam.meeting-manager" and certificate leaf = H"…"`) is the same in every release and macOS keeps the Microphone, System Audio Recording and Calendars grants across updates. Local builds stay ad-hoc (`signingIdentity: "-"`) and are asked again after each rebuild. Secrets: `APPLE_CERTIFICATE` (the base64 `.p12`), `APPLE_CERTIFICATE_PASSWORD` and `APPLE_SIGNING_IDENTITY` (the certificate's common name); the release fails before building when one is missing, and after building when the app's requirement is not tied to the certificate. Create the certificate once and keep the `.p12` offline, never in the repository:

  ```bash
  openssl req -x509 -newkey rsa:2048 -nodes -keyout key.pem -out cert.pem -days 3650 -subj "/CN=Meeting Release Signing" -addext "extendedKeyUsage=critical,codeSigning" -addext "keyUsage=critical,digitalSignature" -addext "basicConstraints=critical,CA:false"
  ```

  ```bash
  openssl pkcs12 -export -legacy -inkey key.pem -in cert.pem -out cert.p12
  ```

  `-legacy` keeps the `.p12` importable into Keychain Access for a local test. Then `base64 -i cert.p12` is `APPLE_CERTIFICATE`. The certificate needs no trust setting: `codesign` signs with an untrusted identity, but the Tauri CLI's own `APPLE_CERTIFICATE` import refuses one ("failed to resolve signing identity"). So `release.yml` imports it into a keychain of the job's own and gives Tauri only `APPLE_SIGNING_IDENTITY`. **Rotating it (or letting it expire) resets the permissions once for every user**, like the first update from an ad-hoc release.
- **Toolchain:** `release.yml` pins Rust 1.99.1. On Rust 1.100 or later, `tauri-build` 2.6 copies the bundled `meeting` into the wrong directory and the build fails with "Is a directory". Unpin it once `tauri-build` is upgraded.
- **Release-only settings** (`createUpdaterArtifacts`, bundling `target/release/meeting`) live in `desktop/src-tauri/tauri.release.conf.json` (D50), so a local `npm run tauri build` needs no key and no prebuilt `meeting`. To try a release build locally, build the tool first (`FEATURE_DESKTOP_APP=1 cargo build --release -p meeting`) and pass a throwaway key from `npx tauri signer generate -w <scratch path>`.
