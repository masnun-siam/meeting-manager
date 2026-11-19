# Extension popup look — business documentation

**Module:** extension · **Updated:** 2026-11-20 · **Repos:** meeting-manager

The source of truth for how this feature works for the business, as it is live today. A PRD is the temporary source of truth for one change, from request to delivery, and locked once approved; after delivery, this doc says what is true. Meeting notes, chats and specs are the material it was built from, never the truth on their own.

## What it does

The Chrome extension's popup (Record and Jobs tabs) and its microphone permission page get their own visual identity, in light and dark, without changing what the extension does. Planned, not live yet.

## Rules

- **R-1 Own identity, light and dark.** The popup and permission page use the "meeting." identity (amber accent, red record colour, Space Grotesk and Inter fonts shipped inside the extension), following macOS light or dark; the popup is 360 px wide.
  - Acceptance: the popup is dark in macOS dark mode and light in light mode, and its fonts render with the network off.
  - Delivery: not delivered
  - Sources: [continue-recording PRD](../../prds/continue-recording.md) FR-15, D12, D13, D15
- **R-2 Record and Jobs tabs.** Record shows a large timer, a round record button, a two-choice language control, the Continue card when available, and closable messages; while recording, a pulsing REC label, Pause/Resume and Stop & save. Jobs shows each job as a card with a coloured stage chip and today's actions.
  - Acceptance: every recording state and job state renders as described and works as before.
  - Delivery: not delivered
  - Sources: [continue-recording PRD](../../prds/continue-recording.md) FR-16, FR-17
- **R-3 Accessible and unchanged.** Motion stops with Reduce motion; contrast meets WCAG 2.1 AA in both palettes; every control is keyboard usable with a name. No extension behaviour, stored data or request changes.
  - Acceptance: with Reduce motion nothing pulses; existing behaviour tests pass with only named text and selector updates.
  - Delivery: not delivered
  - Sources: [continue-recording PRD](../../prds/continue-recording.md) FR-18, FR-19

## Decisions

- 2026-11-20 A new identity for the extension rather than the desktop app's look, in light and dark. By: team, "C: New identity"; "Light and dark". [continue-recording PRD](../../prds/continue-recording.md) D12, D13

## Open questions

None.

## Critical checks

None.

## Timeline

- 2026-11-20 request (team) R-1 R-2 R-3 the extension UI looks bad and should be beautiful and interactive [continue-recording PRD](../../prds/continue-recording.md)
- 2026-11-20 planned (team) R-1 R-2 R-3 direction C from the mockups, PRD [continue-recording](../../prds/continue-recording.md)

## Related

- [docs/prds/continue-recording.md](../../prds/continue-recording.md), [docs/extension.md](../../extension.md), mockups: [Extension popup directions](https://claude.ai/artifact/TMspUajbjocWBpayqhv7qE).
