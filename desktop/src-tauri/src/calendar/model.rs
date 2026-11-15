//! Plain data from EventKit and the rules that turn it into what leaves the module. No type here
//! has an email or URL field, so no address can leave by accident.

use std::collections::HashSet;
use std::sync::Arc;

use meeting::calendar::Response;
use meeting::context::{CalendarEvent, EventAttendee};

use super::window::{OffsetAt, iso_local};

/// The PRD's cut for an event's notes text (FR-11).
pub const MAX_DESCRIPTION: usize = 20_000;
/// The tool rejects a `calendar_event` whose identifiers are longer.
const MAX_ID: usize = 255;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Access {
    NotAsked,
    Granted,
    Denied,
}

impl Access {
    pub fn as_str(self) -> &'static str {
        match self {
            Access::NotAsked => "not_asked",
            Access::Granted => "granted",
            Access::Denied => "denied",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PersonKind {
    Person,
    Room,
    Resource,
    Group,
    Unknown,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RawPerson {
    pub name: Option<String>,
    pub kind: PersonKind,
    pub response: Response,
    pub is_current_user: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RawEvent {
    /// EventKit's `eventIdentifier`, the same for every occurrence of a series.
    pub identifier: String,
    /// `calendarItemExternalIdentifier`.
    pub series_id: String,
    pub title: String,
    /// Unix seconds.
    pub start: i64,
    pub end: i64,
    pub all_day: bool,
    pub calendar_id: String,
    pub calendar_title: String,
    pub notes: Option<String>,
    pub organizer: Option<RawPerson>,
    pub attendees: Vec<RawPerson>,
    /// The response of the attendee that is the current user, if any.
    pub my_response: Option<Response>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RawCalendar {
    pub id: String,
    pub title: String,
    /// The title of the calendar's source (the account).
    pub account: String,
}

/// Called when Calendar reports a change.
pub type Notify = Arc<dyn Fn() + Send + Sync>;

/// Everything the module asks of macOS, as `fn` pointers so tests swap in fakes.
#[derive(Clone, Copy)]
pub struct EventSource {
    /// The current access, never prompting.
    pub access: fn() -> Access,
    /// Prompts for full access and waits for the answer.
    pub request: fn() -> Access,
    pub calendars: fn() -> Vec<RawCalendar>,
    /// Occurrences overlapping `[from, to)`; `Err` is a fixed message, safe to log.
    pub events: fn(i64, i64) -> Result<Vec<RawEvent>, &'static str>,
    pub now: fn() -> i64,
    pub offset_min: OffsetAt,
    /// Calls the function on every Calendar change; `false` when it could not register.
    pub watch: fn(Notify) -> bool,
}

/// `<eventIdentifier>|<start unix seconds>`: opaque to everyone else.
pub fn occurrence_id(identifier: &str, start: i64) -> String {
    format!("{identifier}|{start}")
}

/// The parts of an occurrence id; `None` when it is malformed.
pub fn occurrence_parts(id: &str) -> Option<(&str, i64)> {
    let (identifier, start) = id.rsplit_once('|')?;
    if identifier.is_empty() {
        return None;
    }
    Some((identifier, start.parse().ok()?))
}

/// Whether the upcoming list shows `raw`.
pub fn visible(raw: &RawEvent, hidden: &HashSet<String>) -> bool {
    !raw.all_day
        && raw.my_response != Some(Response::Declined)
        && !hidden.contains(&raw.calendar_id)
}

/// A name that can leave the module: trimmed, not empty and not an email address.
fn safe_name(name: Option<&str>) -> Option<String> {
    let name = name?.trim();
    (!name.is_empty() && !name.contains('@')).then(|| name.to_string())
}

/// The notes text, trimmed and cut to `MAX_DESCRIPTION` characters; `None` when empty.
pub fn description(notes: Option<&str>) -> Option<String> {
    let notes = meeting::ical::html_to_text(notes?);
    let notes = notes.trim();
    (!notes.is_empty()).then(|| notes.chars().take(MAX_DESCRIPTION).collect())
}

/// What leaves the module for `raw`; `None` when its identifiers are longer than the tool takes.
pub fn clean(raw: &RawEvent, offset_min: OffsetAt) -> Option<CalendarEvent> {
    let occurrence_id = occurrence_id(&raw.identifier, raw.start);
    let too_long = |id: &str| id.chars().count() > MAX_ID;
    if too_long(&occurrence_id) || raw.series_id.is_empty() || too_long(&raw.series_id) {
        return None;
    }
    let attendees = raw
        .attendees
        .iter()
        .filter(|p| !matches!(p.kind, PersonKind::Room | PersonKind::Resource))
        .filter_map(|p| {
            Some(EventAttendee {
                name: safe_name(p.name.as_deref())?,
                response: p.response,
            })
        })
        .collect();
    Some(CalendarEvent {
        occurrence_id,
        series_id: raw.series_id.clone(),
        title: raw.title.clone(),
        start: iso_local(raw.start, offset_min(raw.start)),
        end: iso_local(raw.end, offset_min(raw.end)),
        organizer: raw
            .organizer
            .as_ref()
            .and_then(|o| safe_name(o.name.as_deref())),
        calendar: (!raw.calendar_title.is_empty()).then(|| raw.calendar_title.clone()),
        attendees,
        description: description(raw.notes.as_deref()),
    })
}

#[cfg(test)]
pub mod tests {
    use super::*;

    pub fn person(name: &str, kind: PersonKind, response: Response) -> RawPerson {
        RawPerson {
            name: Some(name.into()),
            kind,
            response,
            is_current_user: false,
        }
    }

    /// A timed event on calendar `cal` at `start` for an hour.
    pub fn raw(identifier: &str, start: i64, cal: &str) -> RawEvent {
        RawEvent {
            identifier: identifier.into(),
            series_id: format!("series-{identifier}"),
            title: format!("Event {identifier}"),
            start,
            end: start + 3600,
            all_day: false,
            calendar_id: cal.into(),
            calendar_title: format!("Calendar {cal}"),
            notes: None,
            organizer: None,
            attendees: vec![],
            my_response: None,
        }
    }

    fn dhaka(_: i64) -> i32 {
        360
    }

    #[test]
    fn occurrence_ids_round_trip_and_malformed_ones_are_none() {
        let id = occurrence_id("ABC:1|x", 1_794_528_000);
        assert_eq!(id, "ABC:1|x|1794528000");
        assert_eq!(occurrence_parts(&id), Some(("ABC:1|x", 1_794_528_000)));
        assert_eq!(occurrence_parts("A|-5"), Some(("A", -5)));
        for bad in ["", "A", "|5", "A|", "A|x", "A|5.0", "A| 5"] {
            assert_eq!(occurrence_parts(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn visible_drops_all_day_declined_and_hidden() {
        let hidden = HashSet::from(["h".to_string()]);
        let own = raw("a", 0, "c");
        assert!(visible(&own, &hidden));
        let mut all_day = own.clone();
        all_day.all_day = true;
        assert!(!visible(&all_day, &hidden));
        let mut declined = own.clone();
        declined.my_response = Some(Response::Declined);
        assert!(!visible(&declined, &hidden));
        for kept in [
            Response::Accepted,
            Response::Tentative,
            Response::NeedsAction,
        ] {
            let mut e = own.clone();
            e.my_response = Some(kept);
            assert!(visible(&e, &hidden), "{kept:?}");
        }
        assert!(!visible(&raw("a", 0, "h"), &hidden));
    }

    #[test]
    fn clean_keeps_names_only_and_drops_rooms_resources_and_addresses() {
        let mut e = raw("a", 1_794_528_000, "c");
        e.organizer = Some(person(
            "org@example.com",
            PersonKind::Person,
            Response::Accepted,
        ));
        e.attendees = vec![
            person("Karim", PersonKind::Person, Response::Accepted),
            person("rahim@example.com", PersonKind::Person, Response::Accepted),
            person(
                "Rahim <rahim@example.com>",
                PersonKind::Person,
                Response::Accepted,
            ),
            person("  ", PersonKind::Person, Response::Accepted),
            person("Room 4", PersonKind::Room, Response::Accepted),
            person("Projector", PersonKind::Resource, Response::Accepted),
            person("Team", PersonKind::Group, Response::Tentative),
            person(" Unknown kind ", PersonKind::Unknown, Response::NeedsAction),
        ];
        e.attendees.push(RawPerson {
            name: None,
            ..person("", PersonKind::Person, Response::Declined)
        });
        let c = clean(&e, dhaka).unwrap();
        let names: Vec<_> = c.attendees.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, ["Karim", "Team", "Unknown kind"]);
        assert_eq!(c.attendees[1].response, Response::Tentative);
        assert_eq!(c.organizer, None);
        assert!(!serde_json::to_string(&c).unwrap().contains('@'));
        e.organizer = Some(person(" Org ", PersonKind::Person, Response::Accepted));
        assert_eq!(clean(&e, dhaka).unwrap().organizer.as_deref(), Some("Org"));
    }

    #[test]
    fn clean_fills_ids_times_and_calendar() {
        let mut e = raw("a", 1_794_528_000, "c");
        e.calendar_title = String::new();
        let c = clean(&e, dhaka).unwrap();
        assert_eq!(c.occurrence_id, "a|1794528000");
        assert_eq!(c.series_id, "series-a");
        assert_eq!(c.title, "Event a");
        assert_eq!(c.start, "2026-11-13T06:00:00+06:00");
        assert_eq!(c.end, "2026-11-13T07:00:00+06:00");
        assert_eq!(c.calendar, None);
        assert_eq!(c.description, None);
        e.calendar_title = "Work".into();
        assert_eq!(clean(&e, dhaka).unwrap().calendar.as_deref(), Some("Work"));
    }

    #[test]
    fn identifiers_the_tool_would_reject_skip_the_event() {
        let mut e = raw(&"i".repeat(244), 1_794_528_000, "c");
        assert_eq!(clean(&e, dhaka).unwrap().occurrence_id.len(), 255);
        e.identifier.push('i');
        assert_eq!(clean(&e, dhaka), None);
        let mut e = raw("a", 0, "c");
        e.series_id = "s".repeat(255);
        assert!(clean(&e, dhaka).is_some());
        e.series_id.push('s');
        assert_eq!(clean(&e, dhaka), None);
        e.series_id = String::new();
        assert_eq!(clean(&e, dhaka), None);
    }

    #[test]
    fn notes_are_trimmed_and_cut_by_characters() {
        assert_eq!(description(None), None);
        assert_eq!(description(Some(" \n\t ")), None);
        assert_eq!(description(Some("  hi \n")).as_deref(), Some("hi"));
        let long = "d".repeat(20_001);
        assert_eq!(description(Some(&long)).unwrap().len(), 20_000);
        let wide = "é".repeat(20_001);
        let cut = description(Some(&wide)).unwrap();
        assert_eq!(cut.chars().count(), 20_000);
        assert_eq!(cut.len(), 40_000);
    }

    #[test]
    fn html_notes_become_plain_text() {
        assert_eq!(
            description(Some(
                "<ol><li>Week &amp; plan</li></ol><br>Join: <a href=\"https://x\">x</a>"
            ))
            .as_deref(),
            Some("- Week & plan\n\nJoin: x")
        );
        assert_eq!(description(Some("<br><p></p>")), None);
    }
}
