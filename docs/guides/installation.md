# Installation

Set up Meeting Manager on macOS with Google Chrome. For the shortest path to a first recording, see the [quickstart](quickstart.md).

## 1. Prerequisites

| Need | Check | Get it |
|---|---|---|
| macOS | — | Keychain and Chrome's native-host location are macOS-specific |
| Google Chrome 116+ | `chrome://version` | google.com/chrome |
| Rust 1.87+ | `rustc --version` | `curl https://sh.rustup.rs -sSf \| sh` |
| Node.js 24+ | `node --version` | nodejs.org or `brew install node` |
| ffmpeg with libopus | `ffmpeg -hide_banner -encoders \| grep libopus` | `brew install ffmpeg` |
| Claude Code, logged in | `claude auth status` | Install Claude Code, then `claude auth login` |
| Deepgram API key | — | console.deepgram.com (Nova-3 pre-recorded is billed per audio minute) |
| Obsidian vault | the folder exists | Any existing folder works |

`ffmpeg` and `claude` must be on your `PATH` when you run `meeting install`; it stores their absolute paths, because Chrome starts the tool with a bare `PATH`.

## 2. Install the CLI

```sh
git clone https://github.com/masnun-siam/meeting-manager.git
cd meeting-manager
cargo install --path crates/meeting
```

This puts `meeting` in `~/.cargo/bin` (make sure that is on your `PATH`). Install it there rather than running it from `target/`: Chrome's native-host manifest points at the binary that ran `meeting install`.

## 3. Configure it

```sh
meeting install
```

It finds `ffmpeg` and `claude`, asks for the vault path and the vault folder (default `Meetings`), registers the Chrome native host and writes `~/.config/meeting-manager/config.toml`. Rerunning it is safe: your current answers become the defaults.

```sh
meeting config set-key
```

Paste the Deepgram key at the hidden prompt. It is stored in the macOS Keychain (service `meeting-manager`, account `deepgram`), never in a file.

Recordings are read from `~/Downloads/Meetings`. If Chrome saves downloads somewhere else, change `recordings_dir` in the config file.

## 4. Install the extension

```sh
cd extension
npm ci
npm run build
```

In Chrome, open `chrome://extensions`, turn on **Developer mode**, click **Load unpacked** and choose `extension/dist/`. The extension ID is fixed at `obbaikcmcdooakogedoiiflflbbhpgdh`, which is what the native host trusts. Pin it to the toolbar.

## 5. Verify

```sh
meeting doctor
```

Every line must show `✓`. A `✗` line names its fix. The popup's Jobs tab should show "No meetings processed yet" rather than a "not installed" message.

## 6. Install the desktop app (optional)

The desktop app records calls with the microphone and system audio, lists upcoming calendar events and lets you browse and edit notes. It needs macOS 14.4 or later on Apple silicon.

Download `Meeting.app.tar.gz` from the [latest release](https://github.com/masnun-siam/meeting-manager/releases/latest) and unpack it into `/Applications`:

```sh
cd /Applications
curl -fL https://github.com/masnun-siam/meeting-manager/releases/latest/download/Meeting.app.tar.gz | tar xz
```

The app carries a matching `meeting` binary in `Meeting.app/Contents/Resources/`; until the app installs it for you, keep the tool from section 2 built with the flag on (`FEATURE_DESKTOP_APP=1 cargo install --path crates/meeting`).

To build it yourself instead: the app is behind the build-time flag `FEATURE_DESKTOP_APP`. Build the tool and the app with the flag on:

```sh
FEATURE_DESKTOP_APP=1 cargo install --path crates/meeting
meeting install
cd desktop
npm ci
FEATURE_DESKTOP_APP=1 npm run tauri build
```

Move `Meeting.app` from `target/release/bundle/macos/` (at the repository root) to `/Applications`. The app is ad-hoc signed, so open it the first time with right-click › **Open**, then grant the permissions it asks for. Because the signature changes with every build, macOS may ask for Microphone, System Audio Recording and Calendar permission again after a rebuild.

Built without the flag, the app only shows "Built without FEATURE_DESKTOP_APP".

## Update

```sh
git pull
cargo install --path crates/meeting
cd extension && npm ci && npm run build
```

Then click the reload icon on the extension in `chrome://extensions`. The binary path stays the same, so `meeting install` is only needed again if you install `meeting` somewhere else.

With the desktop app installed, build the tool with `FEATURE_DESKTOP_APP=1` instead, rebuild the app as in section 6 and replace `/Applications/Meeting.app`.

## Uninstall

```sh
cargo uninstall meeting
rm "$HOME/Library/Application Support/Google/Chrome/NativeMessagingHosts/com.meeting_manager.host.json"
rm -r ~/.config/meeting-manager "$HOME/Library/Application Support/meeting-manager" ~/Library/Logs/meeting-manager
security delete-generic-password -s meeting-manager -a deepgram
```

If you installed the desktop app, quit it and move `/Applications/Meeting.app` to the Trash. Its notes database (`notes.sqlite*`) is in `~/Library/Application Support/meeting-manager`, which the commands above already remove.

Remove the extension from `chrome://extensions`. Recordings in `~/Downloads/Meetings` and notes in your vault are left alone.

Reference for every setting, path and `doctor` check: [../features/setup.md](../features/setup.md).
