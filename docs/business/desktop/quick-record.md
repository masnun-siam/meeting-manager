# Quick record and floating indicator — business documentation

**Module:** desktop · **Updated:** 2026-11-19 · **Repos:** meeting-manager

The source of truth for how this feature works for the business, as it is live today. A PRD is the temporary source of truth for one change, from request to delivery, and locked once approved; after delivery, this doc says what is true. Meeting notes, chats and specs are the material it was built from, never the truth on their own.

## What it does

Lets the user start, pause, resume and stop a desktop-app recording from outside the app (a Raycast Quicklink, Shortcuts, a script) through `meeting-manager://` links, and shows a small floating pill on screen for the whole recording with the time, Pause/Resume and Stop. R-1 to R-4 are live in the desktop app; R-5 (an Alfred workflow) ships as a release asset from the next release.

## Rules

- **R-1 Recording links.** The app handles `meeting-manager://record/start`, `/stop`, `/pause`, `/resume` and `/toggle`; `start` and `toggle` accept `language=bn|en`, else the last used language. A link only acts in the states the PRD's link table allows and otherwise does nothing; unknown links do nothing and are logged without their text.
  - Acceptance: `open "meeting-manager://record/start"` starts an ad hoc recording; the same link while recording changes nothing.
  - Delivery: delivered — `meeting-manager@6cd799a` PR #246 (with PR #241)
  - Sources: [quick-record PRD](../../prds/quick-record.md) FR-1, FR-2, FR-3, FR-12
- **R-2 Link launch stays quiet.** A launch of Meeting caused by a link does not show the main window; a refused link start (permission, setup, capture) shows the main window with the error, which can be dismissed and also shows in the menu bar.
  - Acceptance: with Meeting quit, a start link records with no window; with the microphone denied, the window comes up with the permission message.
  - Delivery: delivered — `meeting-manager@6cd799a` PR #246 (with PR #242, PR #243)
  - Sources: [quick-record PRD](../../prds/quick-record.md) FR-4, FR-5
- **R-3 Floating indicator.** While a recording runs, is paused or is saving, a floating pill shows the recorded time with Pause/Resume and one-click Stop, on every Space and over full-screen apps, without taking focus; it can be dragged and remembers its place. After saving it shows the outcome: success for 4 seconds, errors until dismissed. A Settings switch, on by default, turns it off from the next recording.
  - Acceptance: Start from any source shows the pill; Stop shows "Saving…" then "Saved, sent for processing" for 4 seconds; with the switch off no pill appears.
  - Delivery: delivered — `meeting-manager@df83f19` PR #245 (with PR #244, PR #243)
  - Sources: [quick-record PRD](../../prds/quick-record.md) FR-6 to FR-10
- **R-4 Raycast guide.** The user guide explains the links and how to add them as Raycast Quicklinks with a hotkey.
  - Acceptance: following the guide, a Raycast hotkey starts a recording.
  - Delivery: delivered — `meeting-manager@6cd799a` PR #246
  - Sources: [quick-record PRD](../../prds/quick-record.md) FR-11
- **R-5 Alfred workflow.** Each release carries `Meeting.alfredworkflow`; once imported, the Alfred keyword `rec` lists toggle, start, start in English, start in Bangla, pause, resume and stop, each opening its `meeting-manager://record/…` link, and one unassigned hotkey toggles recording.
  - Acceptance: import the release's file, choose "Start recording in English" from `rec` → a recording starts in English with no window.
  - Delivery: delivered — `meeting-manager@7c3be3d` PR #255 (with PR #254)
  - Sources: [alfred-workflow PRD](../../prds/alfred-workflow.md) FR-1 to FR-5

## Decisions

- 2026-11-18 Links need no secret token; the visible pill, menu-bar mark and macOS indicators guard against unnoticed recording — zero setup in Raycast. By: team, "Visible indicator only (Recommended)". [quick-record PRD](../../prds/quick-record.md) D10

## Open questions

None.

## Critical checks

None.

## Timeline

- 2026-11-18 request (team) R-1 R-2 R-3 R-4 start recordings without opening the app and see a floating recording indicator [quick-record PRD](../../prds/quick-record.md)
- 2026-11-18 planned (team) R-1 R-2 R-3 R-4 as specified, PRD [quick-record](../../prds/quick-record.md)
- 2026-11-18 delivered R-3 floating indicator window, pill page and its Settings switch `meeting-manager@df83f19` PR #245, PR #244, PR #243
- 2026-11-18 delivered R-1 R-2 R-4 recording links, quiet link launch with the shared start error, and the Raycast guide `meeting-manager@6cd799a` PR #246, PR #241, PR #242, PR #243
- 2026-11-19 planned (team) R-5 Alfred workflow for the recording links, PRD [alfred-workflow](../../prds/alfred-workflow.md)
- 2026-11-19 delivered R-5 Alfred workflow source, link drift test, release packing and its guide `meeting-manager@7c3be3d` PR #255, PR #254

## Related

- [docs/prds/quick-record.md](../../prds/quick-record.md), [docs/prds/alfred-workflow.md](../../prds/alfred-workflow.md), [docs/prds/desktop-app.md](../../prds/desktop-app.md), [docs/desktop.md](../../desktop.md).
