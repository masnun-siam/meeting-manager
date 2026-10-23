# Setup and configuration

Covers: crates/meeting/src/install.rs, crates/meeting/src/config.rs, crates/meeting/src/secret.rs, crates/meeting/src/doctor.rs, crates/meeting/src/main.rs

First-run order: `meeting install`, `meeting config set-key`, `meeting doctor`. Commands are wired in `run` in `main.rs`. See [../tool.md](../tool.md), [native-messaging.md](native-messaging.md).

## `meeting install`

`install::install` takes input, output, home, PATH and exe as arguments (no globals).

1. Finds `ffmpeg` and `claude` on `PATH` (first executable file; symlinks kept). Missing: error, nothing written.
2. Prompts `Vault path` (default: existing config value; required; must be a directory; leading `~/` expands) and `Vault folder` (default `Meetings` or existing).
3. Writes the native host manifest atomically (`jobs::write_atomic`).
4. Writes the config file. An existing `recordings_dir` is kept; otherwise `~/Downloads/Meetings`.
5. Warns if `claude` is not logged in; prints where recordings are read from.
6. `main.rs` then prints the next step (`config set-key` if no key is stored, else `doctor`; `install_next`). With `FEATURE_MEETING_CONTEXT=1` one more line names `meeting config set-calendar` as optional.

Re-running is safe: existing values become the defaults.

| Artifact | Path |
|---|---|
| Config | `~/.config/meeting-manager/config.toml` (`Config::path`) |
| Host manifest | `~/Library/Application Support/Google/Chrome/NativeMessagingHosts/com.meeting_manager.host.json` (`doctor::manifest_path`) |
| Jobs dir | `~/Library/Application Support/meeting-manager/jobs` (`JobStore::default_dir`, created on first job) |

Manifest content comes from `doctor::expected_manifest`: `name`, `description`, `path` (the running `meeting` binary), `type: "stdio"`, `allowed_origins: ["chrome-extension://<EXTENSION_ID>/"]`. Moving the binary needs a re-install.

`EXTENSION_ID` (`install.rs`) is fixed by the `key` in `extension/manifest.config.ts`; keep them equal (`extension/manifest.test.ts` pins it).

## Config file

TOML, no secrets (`config::Config`). Paths are absolute because Chrome starts the host with a bare `PATH`. Edit by hand to change `recordings_dir`.

| Key | Meaning | Default |
|---|---|---|
| `recordings_dir` | Folder whose `<name>/recording.<ext>` files are processed | `~/Downloads/Meetings` |
| `vault_dir` | Obsidian vault root | prompted |
| `vault_folder` | Subfolder in the vault for notes | `Meetings` (also serde default) |
| `ffmpeg_path` | Absolute path to `ffmpeg` | found on `PATH` |
| `claude_path` | Absolute path to `claude` | found on `PATH` |
| `extension_id` | Origin the host accepts | `EXTENSION_ID` |

`Config::load` errors: missing file gives "meeting is not configured; run `meeting install`"; bad TOML gives "invalid config <path>".

## Deepgram key (`meeting config set-key`)

- Reads one line from stdin via `secret::read_hidden`; echo is off (`/bin/stty`) when stdin is a terminal.
- Trims it; blank is rejected ("no key entered"). Replaces any existing key.
- Stored in the macOS Keychain through the `keyring` crate: service `meeting-manager`, account `deepgram`.
- `secret::deepgram_key` reads it; a missing item errors with "run `meeting config set-key`". The value is never logged or printed.

## Calendar address (`meeting config set-calendar`)

Only in a build with `FEATURE_MEETING_CONTEXT=1`. With the gate off the subcommand is hidden from `meeting config --help` and `run` refuses it with "unrecognized subcommand 'set-calendar'" (clap cannot remove a derived subcommand; `cli` in `main.rs` hides it).

- `secret::set_calendar` holds the logic; `main.rs` passes the hidden-prompt answer, the Keychain entry, `fetch_calendar` (`calendar_fetch::fetch` with `Policy::REAL`) and stdout.
- Trimmed answer. Empty: removes a stored address and prints "Removed.", or "Nothing stored.".
- Refused before any fetch: not starting with `https://` (any case) gives "not https"; a path (after the host, before any `?` or `#`) not ending in `.ics` (any case) gives "not an .ics address". Both add "expected an https:// address whose path ends in .ics".
- Then fetched and parsed (`ical::parse`): a failed fetch gives "could not fetch: <kind>" (`FetchError` text), a parse error "not an iCalendar file". Only after both succeed is it stored (scheme lower-cased) and "Saved. Found N events." printed.
- Stored in the Keychain: service `meeting-manager`, account `calendar-ics`; `secret::calendar_address` reads it (`None` when absent). Never in `config.toml`, the log or any output.

## `meeting doctor`

Runs `doctor::checks`; prints `✓ name` or `✗ name: fix`; exits non-zero with "N checks failed". The same list is the native `health` reply.

| Check | Passes when | Fix text |
|---|---|---|
| `config` | Config loads | Run `meeting install` |
| `deepgram_key` | Keychain has the key | Run `meeting config set-key` |
| `vault` | `vault_dir` is a directory | Create it or run `meeting install` again |
| `ffmpeg` | `ffmpeg_path` is a file | Install ffmpeg, then `meeting install` |
| `claude` | `claude_path` is a file | Install Claude Code, then `meeting install` |
| `claude_login` | `claude auth status --json` exits 0 with `loggedIn: true` | Run `claude auth login` |
| `native_host` | Manifest JSON equals `expected_manifest(current exe, config.extension_id)` | Run `meeting install` |
| `calendar` | Only with `FEATURE_MEETING_CONTEXT=1`. No address stored, or the stored one fetches (20 s, `Policy::REAL`) and parses | Names the cause (`offline`, `rejected (http <status>)`, `not an iCalendar feed`, `failed (<kind>)`, Keychain unreadable) and `meeting config set-calendar`; never the address |

Without a config, the five checks after `deepgram_key` all fail with the install fix; `calendar` does not need the config and still runs. Order is as listed. An unreachable calendar can make `doctor` and `health` take up to 20 s.
