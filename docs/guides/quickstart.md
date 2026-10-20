# Quickstart

From a fresh clone to your first meeting note. Assumes macOS, Chrome, Rust, Node 24, `brew install ffmpeg`, a logged-in Claude Code and a Deepgram key; see [installation](installation.md) for details and troubleshooting.

```sh
cargo install --path crates/meeting
meeting install          # vault path + folder
meeting config set-key   # Deepgram key
meeting doctor           # all ✓
cd extension && npm ci && npm run build
```

Load `extension/dist/` in `chrome://extensions` (Developer mode → **Load unpacked**).

## First recording

1. Open the meeting tab (Meet, Zoom web, Teams web) and click the extension.
2. Pick **Bengali** or **English** and press **Start**. The first time, a permission page opens: allow the mic, go back and press **Start** again.
3. Talk for at least 10 seconds (shorter recordings are saved but not processed), then press **Stop & save**.
4. Open the **Jobs** tab and watch it move through extracting → transcribing → summarising → done. A macOS notification says when it is ready. **Archive** on a finished meeting moves it to `~/Downloads/Meetings/Archive/`. **Trash** (click twice) moves it to the macOS Trash and keeps the vault note.
5. Click **Open summary**, or find the note in your vault folder (`<vault>/Meetings/` by default).

The recording, transcript, summary and `meta.json` sit in the meeting's folder under `~/Downloads/Meetings/`.

## Next

- Picked the wrong language? `meeting process <folder>/recording.webm --language en`.
- Want a fresh summary? `meeting process <folder>/recording.webm` reuses the transcript, so Deepgram is not billed again.
- Recorded a call outside Chrome (phone voice memo, desktop app)? `meeting import ~/Desktop/call.m4a --language en` copies it into its own meeting folder and processes it.
- Something failed? `meeting doctor`, then `~/Library/Logs/meeting-manager/meeting.log`.
