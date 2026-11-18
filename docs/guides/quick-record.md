# Quick record

Start, pause, resume and stop the desktop app's recorder from anywhere with a link: a Raycast hotkey, an Alfred workflow, a Shortcuts action, or `open` in Terminal. Needs the desktop app installed in `/Applications` and opened once, so macOS knows its `meeting-manager:` links.

## The links

| Link | What it does |
|---|---|
| `meeting-manager://record/start` | Starts recording when nothing is recording; otherwise does nothing |
| `meeting-manager://record/stop` | Stops and saves a running or paused recording |
| `meeting-manager://record/pause` | Pauses a running recording |
| `meeting-manager://record/resume` | Resumes a paused recording |
| `meeting-manager://record/toggle` | Starts when nothing is recording, stops when recording or paused: one hotkey for both |

`start` and `toggle` take `?language=bn` or `?language=en`, for example `meeting-manager://record/start?language=en`. Without it, the recording uses the language you used last. The language a link starts with becomes the last used.

A link that does not apply right now (pause while paused, start while saving) does nothing and shows nothing. A link the app does not know (another path, `language=fr`, extra parameters) is ignored too; the log (`~/Library/Logs/meeting-manager/meeting.log`) records only `link ignored:` and the reason, never the link.

If Meeting is not running, a link opens it without showing its window, and the menu-bar item shows the recording. If a link's start fails (for example the microphone is not allowed), the Meeting window comes to the front with the reason.

Try one in Terminal:

```sh
open "meeting-manager://record/toggle?language=en"
```

## Raycast hotkey

1. In Raycast, run **Create Quicklink**.
2. Name it (for example "Record meeting"), set **Link** to `meeting-manager://record/toggle` (add `?language=en` or `?language=bn` to fix the language), leave **Open With** on the default, and save.
3. Open Raycast **Settings › Extensions › Quicklinks**, select the quicklink and record a **Hotkey**.
4. Repeat for any other link you want on a key, such as `meeting-manager://record/pause`.

Raycast opens the link directly, so no prompt appears. Opening a `meeting-manager:` link from a browser instead shows macOS's "Open Meeting?" prompt the first time; allow it.

## Alfred workflow

Needs Alfred 5 with the Powerpack, and Meeting v0.3.0 or later installed in `/Applications` and opened once; otherwise macOS says no application is set to open the link.

1. Download `Meeting.alfredworkflow` from the [latest release](https://github.com/masnun-siam/meeting-manager/releases/latest), double-click it and press **Import** on Alfred's "Meeting" sheet.
2. Type `rec` in Alfred to see the seven actions below; typing more filters them, for example `rec stop`. Each opens the matching link from [The links](#the-links).

   | Item | Opens |
   |---|---|
   | Toggle recording | `meeting-manager://record/toggle` |
   | Start recording | `meeting-manager://record/start` (last used language) |
   | Start recording in English | `meeting-manager://record/start?language=en` |
   | Start recording in Bangla | `meeting-manager://record/start?language=bn` |
   | Pause recording | `meeting-manager://record/pause` |
   | Resume recording | `meeting-manager://record/resume` |
   | Stop recording | `meeting-manager://record/stop` |

3. To put toggle on a key, open Alfred **Preferences › Workflows › Meeting**, double-click the **Hotkey** object, record a key and press **Save**. The hotkey is unassigned after import. Pressing it starts a recording; pressing it again stops and saves.
4. To change the keyword, double-click the **List Filter** object in the same workflow, edit **Keyword** and press **Save**. If another workflow also uses `rec`, Alfred shows both until you change one.

Importing the file from a newer release updates the same workflow instead of adding a copy. Like any link, an item or hotkey that does not apply right now does nothing, and the workflow shows no confirmation of its own.
