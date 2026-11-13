# Desktop calendar (EventKit)

Covers: desktop/src-tauri/src/calendar/

The desktop app's calendar module: macOS Calendar through EventKit, the upcoming list, one event with its earlier notes, the calendars and the hidden-calendar setting, change notifications, and the lookup the recorder uses for `context.json`. Behaviour: [../prds/desktop-app.md](../prds/desktop-app.md) FR-8 to FR-11, FR-20. Module map: [../desktop.md](../desktop.md).

## Files

| File | Holds |
|---|---|
| `mod.rs` | `Calendar` (store path, `EventSource`, change watch), the five commands, `on_launch`, `Calendar::event_for_context` |
| `model.rs` | Plain data (`RawEvent`, `RawPerson`, `RawCalendar`, `Access`), `EventSource` (fn pointers), the rules: `clean`, `visible`, `occurrence_id` / `occurrence_parts`, `description` |
| `window.rs` | The 8-day window, `iso_local` (`YYYY-MM-DDTHH:MM:SS±HH:MM`), `local_date`; std only, the offset at an instant comes from a fn pointer |
| `eventkit.rs` | `real()`, the only Objective-C: maps EventKit to plain data. One worker thread owns the `EKEventStore`; calls send it a job over a channel |

Tests swap in a fake `EventSource` (`Calendar::with(&env, source)`). In this crate's test builds `Calendar::new` reads access as denied, so no test shows the system prompt or reaches EventKit. `eventkit.rs` is covered by its enum-mapping tests and a manual check only.

## Rules

| Rule | Where |
|---|---|
| Window: local today 00:00 to the local midnight ending the 7th following day; the offset is read at each end, so a daylight-saving change inside the window is respected | `window::window` |
| Listed: events that start inside the window, on their start day only; today's past events stay | `events_reply` |
| Dropped from the list: all-day, declined by the current user (`isCurrentUser`), calendars in `hidden_calendars`. An event with no attendees is kept | `model::visible` |
| Order: days by local start date, today first; events by start, then title; empty days are not listed | `events_reply` |
| `occurrence_id` = `<eventIdentifier>\|<start unix seconds>`; `series_id` = `calendarItemExternalIdentifier`. An event whose id is over 255 characters is skipped (the tool would reject it) | `model::clean` |
| Attendees: rooms and resources dropped; names empty or containing `@` dropped; an organizer named with `@` becomes `null`. Only `EKParticipant.name` is read, never its URL | `model::clean`, `eventkit::raw_person` |
| Responses: accepted, declined, tentative; anything else is `needs_action` | `eventkit::response_of` |
| Notes: trimmed, cut to 20,000 characters, `null` when empty; `has_notes` is the same test | `model::description` |
| Logs: counts and error kinds only, never a title, calendar name, attendee name or notes | everywhere |

## Commands

| Command | Reply | Errors |
|---|---|---|
| `calendar_status` | `{access: granted\|denied\|not_asked}`; never prompts | none |
| `calendar_events` | `{days: [{date, events: [{occurrence_id, series_id, title, start, end, calendar, has_notes}]}]}`. When `not_asked` it prompts once and waits | `calendar_denied`, `calendar_failed` |
| `calendar_event {occurrence_id}` | `{event: <context.json calendar_event>, earlier: [{id, title, recorded_at}]}`; `earlier` from `Store::earlier(series_id, None, 20)`, title falling back to the meeting folder's name; a store that cannot be opened gives `earlier: []` | `calendar_denied`, `unknown_event`, `calendar_failed` |
| `calendars` | `{calendars: [{id, title, account, hidden}]}` sorted by account then title; `[]` when access is not granted | none |
| `set_hidden_calendars {ids}` | Stores `ids` as a JSON list in the store setting `hidden_calendars` (unknown ids kept), replies like `calendars` | `store_unavailable` |

Every command runs on a blocking thread: EventKit calls block and must not run on the main thread.

## Lookup for the recorder

`Calendar::event_for_context(occurrence_id) -> Result<Option<meeting::context::CalendarEvent>, CalendarError>` returns what goes into `context.json` unchanged (`calendar` is the calendar's title, D31). It asks EventKit for the events starting in that second and matches the identifier and start (`eventWithIdentifier:` returns only a series' first occurrence). It skips the list's filters and window: the user picked the event. `Ok(None)` for an unknown, malformed or deleted event; `Err(Denied)` without access.

## Change notifications

`on_launch` stores a callback that emits the Tauri event `calendar-changed` (no payload) to every window, and registers for `EKEventStoreChangedNotification` once access is granted: at launch, or right after the first `calendar_events` prompt is granted. It registers once. The UI listens for it and also refetches on window focus.
