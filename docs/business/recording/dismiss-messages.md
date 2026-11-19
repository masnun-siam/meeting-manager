# Closable recording messages — business documentation

**Module:** recording · **Updated:** 2026-11-20 · **Repos:** meeting-manager

The source of truth for how this feature works for the business, as it is live today. A PRD is the temporary source of truth for one change, from request to delivery, and locked once approved; after delivery, this doc says what is true. Meeting notes, chats and specs are the material it was built from, never the truth on their own.

## What it does

Every message shown under the recording controls, in the desktop app and in the Chrome extension, can be closed with a × button. Planned, not live yet.

## Rules

- **R-1 Close any message.** Every message line under the recording controls (desktop: start error, outcome notice, command and connection errors; extension: notice, stop reason, microphone warning, send error) has a keyboard-reachable × named "Dismiss message". A closed outcome notice stays closed (also in the desktop menu bar and floating pill); an error that happens again shows again.
  - Acceptance: after a recording, "Saved, sent for processing" closes with × and does not come back on reopening the popup or window.
  - Delivery: not delivered
  - Sources: [continue-recording PRD](../../prds/continue-recording.md) FR-1, FR-2, FR-3, D2; per [desktop/quick-record.md](../desktop/quick-record.md) R-3 for the pill

## Decisions

- 2026-11-20 Every message gets a ×, in both the app and the extension. By: team, "Every message, app + extension (Recommended)". [continue-recording PRD](../../prds/continue-recording.md) D2

## Open questions

None.

## Critical checks

None.

## Timeline

- 2026-11-20 request (team) R-1 alerts under the recording controls cannot be dismissed [continue-recording PRD](../../prds/continue-recording.md)
- 2026-11-20 planned (team) R-1 as specified, PRD [continue-recording](../../prds/continue-recording.md)

## Related

- [docs/prds/continue-recording.md](../../prds/continue-recording.md), [docs/features/recording.md](../../features/recording.md), [docs/features/desktop-recorder.md](../../features/desktop-recorder.md).
