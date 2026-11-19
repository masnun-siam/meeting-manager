# Continue a recording — business documentation

**Module:** recording · **Updated:** 2026-11-20 · **Repos:** meeting-manager

The source of truth for how this feature works for the business, as it is live today. A PRD is the temporary source of truth for one change, from request to delivery, and locked once approved; after delivery, this doc says what is true. Meeting notes, chats and specs are the material it was built from, never the truth on their own.

## What it does

After a recording is stopped by mistake (Stop pressed, tab closed, browser or app quit), the user can continue the same meeting from the Chrome extension or the desktop app. The new part is added to the same meeting, and the whole meeting is transcribed and summarised again into the same note. Planned, not live yet: built behind the `FEATURE_CONTINUE_RECORDING` flag.

## Rules

- **R-1 What can be continued.** Each surface (the extension, the desktop app) can continue only its own last recording, while idle, for 2 hours after it stopped (or was recovered), while its meeting folder and recording are still where they were saved, and while its total recorded time is under 3 hours. Any new recording replaces it as the last one. A continued meeting can be continued again under the same rules.
  - Acceptance: 1 minute after a stop Continue is offered; at 2 h 1 min, after archiving the meeting, or after a new recording it is not.
  - Delivery: not delivered
  - Sources: [continue-recording PRD](../../prds/continue-recording.md) FR-4, D4, D11
- **R-2 Where to continue.** The extension popup shows a Continue card (folder, recorded time, how long ago, why it stopped) and records the active tab, as Start does. The desktop app offers Continue in its window, its menu bar and the `meeting-manager://record/continue` link; the Alfred workflow has a "Continue last recording" item. The language is the earlier part's and cannot be changed.
  - Acceptance: after closing a recorded Meet tab and reopening it, Continue in the popup records it with the timer picking up from the earlier total.
  - Delivery: not delivered
  - Sources: [continue-recording PRD](../../prds/continue-recording.md) FR-5, FR-6, FR-7, FR-13, D5, D8, D9; per [desktop/quick-record.md](../desktop/quick-record.md) R-1 for the link
- **R-3 Time and limit.** While a continued part records, the timer shows the meeting's total and the 3-hour cap applies to the total.
  - Acceptance: continuing a 2:59:00 meeting stops by the cap after 60 seconds.
  - Delivery: not delivered
  - Sources: [continue-recording PRD](../../prds/continue-recording.md) FR-8, D6
- **R-4 One meeting, processed again.** A continued part is saved into the same meeting folder; processing stops any run still going for that meeting, joins the parts into the one recording, and transcribes and summarises the whole meeting again into the same note, keeping the user's edits. The meeting keeps one job row. Processing starts once the total is at least 10 seconds.
  - Acceptance: a finished meeting continued for 2 minutes ends as one note covering both parts with consistent speakers, and its edits unchanged.
  - Delivery: not delivered
  - Sources: [continue-recording PRD](../../prds/continue-recording.md) FR-9, FR-10, D3, D7, D14
- **R-5 Nothing is lost.** A failed join keeps the recording and every part, and the job can be retried. A crash while continuing saves the leftover into the same meeting. A part whose meeting folder was moved while it recorded is saved as a new meeting with an error notice.
  - Acceptance: with a corrupt part the job fails at "joining" and the original still plays; Retry with a good part finishes.
  - Delivery: not delivered
  - Sources: [continue-recording PRD](../../prds/continue-recording.md) FR-11, FR-12, D17

## Decisions

- 2026-11-20 Continuing re-processes the whole meeting, accepting the extra transcription cost for earlier parts — speaker numbers stay consistent. By: team, "One meeting, re-processed whole (Recommended)". [continue-recording PRD](../../prds/continue-recording.md) D3

## Open questions

None.

## Critical checks

None.

## Timeline

- 2026-11-20 request (team) R-1 R-2 R-3 R-4 R-5 continue a recording stopped by mistake instead of starting a new note, in the app and the extension [continue-recording PRD](../../prds/continue-recording.md)
- 2026-11-20 planned (team) R-1 R-2 R-3 R-4 R-5 as specified, PRD [continue-recording](../../prds/continue-recording.md)

## Related

- [docs/prds/continue-recording.md](../../prds/continue-recording.md), [docs/features/recording.md](../../features/recording.md), [docs/features/desktop-recorder.md](../../features/desktop-recorder.md), [docs/features/processing.md](../../features/processing.md).
