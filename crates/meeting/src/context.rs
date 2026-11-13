//! `context.json`: what the extension read from the meeting tab, checked against the limits
//! table of `docs/prds/meeting-context.md`. A reject rule drops the whole context, a cut rule
//! trims the field and lists it in `truncated`. Rule names are the only thing ever logged.
//! The desktop app adds `calendar_event` (`docs/prds/desktop-app.md`): its reject rules drop
//! only the event.

use std::fs;
use std::io::ErrorKind;
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::calendar::Response;
use crate::summarise::is_hh_mm_ss;

const MAX_BYTES: usize = 256 * 1024;
const MAX_TEXT: usize = 300;
const MAX_NAME: usize = 100;
const MAX_PARTICIPANTS: usize = 200;
const MAX_SPEAKING: usize = 5_000;
const MAX_SPEAKERS_PER_INTERVAL: usize = 10;
const MAX_EVENT_ID: usize = 255;
const MAX_ATTENDEES: usize = 200;
const MAX_DESCRIPTION: usize = 20_000;
const EVENT: &str = "calendar_event";
const KNOWN_TRUNCATED: [&str; 5] = [
    "tab_title",
    "meeting_title",
    "participants",
    "speaking",
    EVENT,
];

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Platform {
    Meet,
    Zoom,
    Teams,
    Other,
    Desktop,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Participant {
    pub name: String,
    #[serde(rename = "self")]
    pub is_self: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Interval {
    pub from: String,
    pub to: String,
    pub names: Vec<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Context {
    pub version: u32,
    pub platform: Platform,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub meeting_id: Option<String>,
    #[serde(default)]
    pub tab_title: Option<String>,
    #[serde(default)]
    pub meeting_title: Option<String>,
    pub started_at_ms: i64,
    pub utc_offset_min: i32,
    #[serde(default)]
    pub time_zone: Option<String>,
    #[serde(default)]
    pub participants: Vec<Participant>,
    #[serde(default)]
    pub speaking: Vec<Interval>,
    #[serde(default)]
    pub truncated: Vec<String>,
    /// The calendar event the desktop app's user picked for this recording.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub calendar_event: Option<CalendarEvent>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct CalendarEvent {
    pub occurrence_id: String,
    pub series_id: String,
    pub title: String,
    /// ISO 8601 with an offset, as is `end`.
    pub start: String,
    pub end: String,
    pub organizer: Option<String>,
    /// The calendar's name: kept in the notes store, never sent to Claude or the vault (D31).
    pub calendar: Option<String>,
    pub attendees: Vec<EventAttendee>,
    pub description: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct EventAttendee {
    pub name: String,
    pub response: Response,
}

/// The checked `context.json` at `path`; `None` when it is missing or fails a rule (logged by
/// rule name only).
pub fn load(path: &Path) -> Option<Context> {
    let checked = match fs::read(path) {
        Err(e) if e.kind() == ErrorKind::NotFound => return None,
        Err(_) => Err("unreadable"),
        Ok(bytes) => serde_json::from_slice::<Value>(&bytes)
            .map_err(|_| "not JSON")
            .and_then(|raw| check(&raw)),
    };
    checked
        .inspect_err(|rule| tracing::warn!("context.json ignored: {rule}"))
        .ok()
}

/// Validates `raw`; `Err` is a fixed rule name, safe to log.
pub fn check(raw: &Value) -> Result<Context, &'static str> {
    if serde_json::to_vec(raw).map_or(true, |b| b.len() > MAX_BYTES) {
        return Err("too large");
    }
    if raw.get("started_at_ms").is_none() && raw.is_object() {
        return Err("started_at_ms missing");
    }
    if let Some(Value::String(p)) = raw.get("platform")
        && !["meet", "zoom", "teams", "other", "desktop"].contains(&p.as_str())
    {
        return Err("platform");
    }
    // Checked apart from the rest, so a bad event cannot reject the whole context.
    let mut rest = raw.clone();
    let event = rest
        .as_object_mut()
        .and_then(|o| o.remove(EVENT))
        .filter(|e| !e.is_null());
    let mut c: Context = serde_json::from_value(rest).map_err(|_| "wrong type or unknown field")?;
    if c.version != 1 {
        return Err("version");
    }
    if c.url.as_deref().is_some_and(|u| u.chars().count() > 2_000) {
        return Err("url too long");
    }
    if let Some(id) = &c.meeting_id
        && !meeting_id_ok(c.platform, id)
    {
        return Err("meeting_id malformed");
    }
    if !(-720..=840).contains(&c.utc_offset_min) {
        return Err("utc_offset_min range");
    }
    if c.time_zone.as_deref().is_some_and(|z| !time_zone_ok(z)) {
        return Err("time_zone malformed");
    }
    c.truncated
        .retain(|f| KNOWN_TRUNCATED.contains(&f.as_str()));
    c.truncated.dedup();
    let mut cut = Vec::new();
    for (field, name) in [
        (&mut c.tab_title, "tab_title"),
        (&mut c.meeting_title, "meeting_title"),
    ] {
        if let Some(t) = field
            && t.chars().count() > MAX_TEXT
        {
            *t = t.chars().take(MAX_TEXT).collect();
            cut.push(name);
        }
    }
    if trim_participants(&mut c.participants) {
        cut.push("participants");
    }
    if trim_speaking(&mut c.speaking) {
        cut.push("speaking");
    }
    match event.map(check_event) {
        Some(Ok((event, was_cut))) => {
            c.calendar_event = Some(event);
            if was_cut {
                cut.push(EVENT);
            }
        }
        Some(Err(rule)) => tracing::warn!("calendar_event dropped: {rule}"),
        None => {}
    }
    if c.calendar_event.is_none() {
        c.truncated.retain(|f| f != EVENT);
    }
    for f in cut {
        if !c.truncated.iter().any(|t| t == f) {
            c.truncated.push(f.into());
        }
    }
    Ok(c)
}

/// The checked event and whether a cut rule trimmed it; `Err` is a fixed rule name.
fn check_event(raw: Value) -> Result<(CalendarEvent, bool), &'static str> {
    let mut e: CalendarEvent = serde_json::from_value(raw).map_err(|_| "calendar_event type")?;
    let id_ok = |id: &str| !id.is_empty() && id.chars().count() <= MAX_EVENT_ID;
    if !id_ok(&e.occurrence_id) {
        return Err("calendar_event occurrence_id");
    }
    if !id_ok(&e.series_id) {
        return Err("calendar_event series_id");
    }
    if !is_iso_with_offset(&e.start) {
        return Err("calendar_event start");
    }
    if !is_iso_with_offset(&e.end) {
        return Err("calendar_event end");
    }
    let mut cut = cut_to(&mut e.title, MAX_TEXT);
    for name in [&mut e.organizer, &mut e.calendar].into_iter().flatten() {
        cut |= cut_to(name, MAX_NAME);
    }
    if let Some(description) = &mut e.description {
        cut |= cut_to(description, MAX_DESCRIPTION);
    }
    let before = e.attendees.len();
    // An email address is never stored; the app drops email-only names too.
    e.attendees.retain(|a| !a.name.contains('@'));
    e.attendees.truncate(MAX_ATTENDEES);
    cut |= e.attendees.len() != before;
    for a in &mut e.attendees {
        cut |= cut_to(&mut a.name, MAX_NAME);
    }
    Ok((e, cut))
}

/// `YYYY-MM-DDTHH:MM[:SS[.fff]]` followed by `Z` or `±HH:MM`. Shape only.
fn is_iso_with_offset(s: &str) -> bool {
    let b = s.as_bytes();
    // `d` in `pattern` stands for any digit.
    let shaped = |at: usize, pattern: &str| {
        b.get(at..at + pattern.len()).is_some_and(|part| {
            part.iter().zip(pattern.bytes()).all(|(c, p)| match p {
                b'd' => c.is_ascii_digit(),
                _ => *c == p,
            })
        })
    };
    if !shaped(0, "dddd-dd-ddTdd:dd") {
        return false;
    }
    let mut at = 16;
    if shaped(at, ":dd") {
        at += 3;
        if b.get(at) == Some(&b'.') {
            let digits = b[at + 1..]
                .iter()
                .take_while(|c| c.is_ascii_digit())
                .count();
            if digits == 0 {
                return false;
            }
            at += 1 + digits;
        }
    }
    match b.len() - at {
        1 => b[at] == b'Z',
        6 => shaped(at, "+dd:dd") || shaped(at, "-dd:dd"),
        _ => false,
    }
}

fn meeting_id_ok(platform: Platform, id: &str) -> bool {
    match platform {
        Platform::Meet => {
            let parts: Vec<&str> = id.split('-').collect();
            parts.len() == 3
                && [3, 4, 3]
                    .iter()
                    .zip(&parts)
                    .all(|(n, p)| p.len() == *n && p.bytes().all(|b| b.is_ascii_lowercase()))
        }
        Platform::Zoom => !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()),
        _ => false,
    }
}

/// An IANA-shaped zone name `check` accepts; the desktop app tests its own before writing.
pub fn time_zone_ok(z: &str) -> bool {
    !z.is_empty()
        && z.chars().count() <= 64
        && z.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '_' | '+' | '-'))
}

/// Cuts `s` to `max` characters; true when it changed.
fn cut_to(s: &mut String, max: usize) -> bool {
    if s.chars().count() <= max {
        return false;
    }
    *s = s.chars().take(max).collect();
    true
}

fn trim_participants(list: &mut Vec<Participant>) -> bool {
    let before = list.len();
    list.retain(|p| !p.name.is_empty());
    let mut cut = list.len() != before;
    if list.len() > MAX_PARTICIPANTS {
        list.truncate(MAX_PARTICIPANTS);
        cut = true;
    }
    let mut seen_self = false;
    for p in list.iter_mut() {
        cut |= cut_to(&mut p.name, MAX_NAME);
        if p.is_self && std::mem::replace(&mut seen_self, true) {
            p.is_self = false;
        }
    }
    cut
}

fn trim_speaking(list: &mut Vec<Interval>) -> bool {
    let mut cut = false;
    list.retain_mut(|i| {
        let ok = is_hh_mm_ss(&i.from) && is_hh_mm_ss(&i.to) && i.from <= i.to;
        let before = i.names.len();
        i.names.retain(|n| !n.is_empty());
        cut |= i.names.len() != before;
        for n in i.names.iter_mut() {
            cut |= cut_to(n, MAX_NAME);
        }
        if i.names.len() > MAX_SPEAKERS_PER_INTERVAL {
            i.names.truncate(MAX_SPEAKERS_PER_INTERVAL);
            cut = true;
        }
        cut |= !ok || i.names.is_empty();
        ok && !i.names.is_empty()
    });
    if list.len() > MAX_SPEAKING {
        list.truncate(MAX_SPEAKING);
        cut = true;
    }
    cut
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn base() -> Value {
        json!({"version": 1, "platform": "meet", "url": "https://meet.google.com/abc-defg-hij",
            "meeting_id": "abc-defg-hij", "tab_title": "T", "meeting_title": "M",
            "started_at_ms": 1, "utc_offset_min": 360, "time_zone": "Asia/Dhaka",
            "participants": [{"name": "A", "self": true}],
            "speaking": [{"from": "00:00:01", "to": "00:00:02", "names": ["A"]}], "truncated": []})
    }

    fn with(key: &str, value: Value) -> Value {
        let mut v = base();
        v[key] = value;
        v
    }

    fn rejected(raw: Value) -> &'static str {
        check(&raw).unwrap_err()
    }

    #[test]
    fn a_valid_context_passes_unchanged() {
        let c = check(&base()).unwrap();
        assert_eq!(serde_json::to_value(&c).unwrap(), base());
    }

    #[test]
    fn reject_rows() {
        assert_eq!(rejected(with("version", json!(2))), "version");
        assert_eq!(rejected(with("platform", json!("slack"))), "platform");
        assert_eq!(
            rejected(with("url", json!("x".repeat(2_001)))),
            "url too long"
        );
        assert_eq!(
            rejected(with("meeting_id", json!("ABC"))),
            "meeting_id malformed"
        );
        assert_eq!(
            rejected(with("platform", json!("teams"))),
            "meeting_id malformed"
        );
        let mut no_start = base();
        no_start.as_object_mut().unwrap().remove("started_at_ms");
        assert_eq!(rejected(no_start), "started_at_ms missing");
        assert_eq!(
            rejected(with("utc_offset_min", json!(841))),
            "utc_offset_min range"
        );
        assert_eq!(
            rejected(with("utc_offset_min", json!(-721))),
            "utc_offset_min range"
        );
        assert_eq!(
            rejected(with("time_zone", json!("a b"))),
            "time_zone malformed"
        );
        assert_eq!(
            rejected(with("time_zone", json!("a".repeat(65)))),
            "time_zone malformed"
        );
        assert_eq!(
            rejected(with("extra", json!(1))),
            "wrong type or unknown field"
        );
        assert_eq!(
            rejected(with("tab_title", json!(5))),
            "wrong type or unknown field"
        );
        assert_eq!(rejected(json!(5)), "wrong type or unknown field");
        assert_eq!(
            rejected(with("tab_title", json!("x".repeat(MAX_BYTES)))),
            "too large"
        );
    }

    #[test]
    fn meeting_ids_follow_the_platform() {
        let zoom = |id: &str| {
            check(&{
                let mut v = with("platform", json!("zoom"));
                v["meeting_id"] = json!(id);
                v
            })
        };
        assert!(zoom("123456789").is_ok());
        assert!(zoom("12a").is_err());
        assert!(zoom("").is_err());
        let mut other = with("platform", json!("other"));
        other["meeting_id"] = Value::Null;
        assert!(check(&other).is_ok());
    }

    #[test]
    fn long_titles_are_cut_and_listed() {
        let mut raw = with("tab_title", json!("é".repeat(301)));
        raw["meeting_title"] = json!("m".repeat(301));
        let c = check(&raw).unwrap();
        assert_eq!(c.tab_title.unwrap().chars().count(), 300);
        assert_eq!(c.meeting_title.unwrap().chars().count(), 300);
        assert_eq!(c.truncated, ["tab_title", "meeting_title"]);
    }

    #[test]
    fn participants_are_cut() {
        let many: Vec<Value> = (0..201)
            .map(|i| json!({"name": format!("n{i}"), "self": false}))
            .collect();
        let c = check(&with("participants", json!(many))).unwrap();
        assert_eq!(c.participants.len(), 200);
        assert_eq!(c.truncated, ["participants"]);

        let c = check(&with(
            "participants",
            json!([{"name": "n".repeat(101), "self": false}]),
        ))
        .unwrap();
        assert_eq!(c.participants[0].name.chars().count(), 100);
        assert_eq!(c.truncated, ["participants"]);

        let c = check(&with(
            "participants",
            json!([{"name": "", "self": false}, {"name": "B", "self": false}]),
        ))
        .unwrap();
        assert_eq!(c.participants.len(), 1);
        assert_eq!(c.truncated, ["participants"]);
    }

    #[test]
    fn only_the_first_self_stays() {
        let c = check(&with(
            "participants",
            json!([
            {"name": "A", "self": true}, {"name": "B", "self": true}]),
        ))
        .unwrap();
        assert!(c.participants[0].is_self && !c.participants[1].is_self);
        assert!(c.truncated.is_empty());
    }

    #[test]
    fn bad_speaking_entries_are_dropped_and_listed() {
        let good = json!({"from": "00:00:01", "to": "00:00:02", "names": ["A"]});
        for bad in [
            json!({"from": "1:00", "to": "00:00:02", "names": ["A"]}),
            json!({"from": "00:00:03", "to": "00:00:02", "names": ["A"]}),
            json!({"from": "00:00:01", "to": "00:00:02", "names": []}),
            json!({"from": "00:00:01", "to": "00:00:02", "names": [""]}),
        ] {
            let c = check(&with("speaking", json!([good, bad]))).unwrap();
            assert_eq!(c.speaking.len(), 1);
            assert_eq!(c.truncated, ["speaking"]);
        }
    }

    #[test]
    fn speaking_names_and_count_are_cut() {
        let names: Vec<String> = (0..11).map(|i| format!("n{i}")).collect();
        let c = check(&with(
            "speaking",
            json!([{"from": "00:00:01", "to": "00:00:02", "names": names}]),
        ))
        .unwrap();
        assert_eq!(c.speaking[0].names.len(), 10);
        assert_eq!(c.truncated, ["speaking"]);

        let c = check(&with(
            "speaking",
            json!([{"from": "00:00:01", "to": "00:00:02", "names": ["n".repeat(101)]}]),
        ))
        .unwrap();
        assert_eq!(c.speaking[0].names[0].chars().count(), 100);

        let many = vec![json!({"from": "00:00:01", "to": "00:00:02", "names": ["A"]}); 5_001];
        let c = check(&with("speaking", json!(many))).unwrap();
        assert_eq!(c.speaking.len(), 5_000);
        assert_eq!(c.truncated, ["speaking"]);
    }

    fn desktop() -> Value {
        json!({"version": 1, "platform": "desktop", "url": null, "meeting_id": null,
            "tab_title": null, "meeting_title": null, "started_at_ms": 1, "utc_offset_min": 360,
            "time_zone": "Asia/Dhaka", "participants": [], "speaking": [], "truncated": []})
    }

    fn event() -> Value {
        json!({"occurrence_id": "occ-1", "series_id": "ser-1", "title": "Weekly sync",
            "start": "2026-11-13T10:00:00+06:00", "end": "2026-11-13T11:00:00+06:00",
            "organizer": "Karim", "calendar": "Work",
            "attendees": [{"name": "Rahim", "response": "accepted"},
                {"name": "Nadia", "response": "needs_action"}],
            "description": "Agenda"})
    }

    /// A desktop context whose event has `key` set to `value`.
    fn with_event(key: &str, value: Value) -> Value {
        let mut e = event();
        e[key] = value;
        let mut v = desktop();
        v[EVENT] = e;
        v
    }

    fn checked_event(key: &str, value: Value) -> (CalendarEvent, Vec<String>) {
        let c = check(&with_event(key, value)).unwrap();
        (c.calendar_event.unwrap(), c.truncated)
    }

    #[test]
    fn a_desktop_context_passes_and_takes_no_meeting_id() {
        let c = check(&desktop()).unwrap();
        assert_eq!(c.platform, Platform::Desktop);
        assert_eq!(serde_json::to_value(&c).unwrap(), desktop());
        let mut with_id = desktop();
        with_id["meeting_id"] = json!("abc-defg-hij");
        assert_eq!(rejected(with_id), "meeting_id malformed");
    }

    #[test]
    fn a_valid_event_passes_unchanged_and_a_missing_one_adds_no_key() {
        let raw = with_event("title", json!("Weekly sync"));
        let c = check(&raw).unwrap();
        assert_eq!(serde_json::to_value(&c).unwrap(), raw);
        assert!(c.truncated.is_empty());

        let mut null = desktop();
        null[EVENT] = Value::Null;
        for without in [desktop(), null, base()] {
            let c = check(&without).unwrap();
            assert_eq!(c.calendar_event, None);
            assert!(serde_json::to_value(&c).unwrap().get(EVENT).is_none());
        }
    }

    #[test]
    fn a_rejected_event_drops_only_the_event() {
        let mut missing = event();
        missing.as_object_mut().unwrap().remove("title");
        let mut unknown = event();
        unknown["email"] = json!("x");
        let mut bad_response = event();
        bad_response["attendees"][0]["response"] = json!("delegated");
        let mut extra_in_attendee = event();
        extra_in_attendee["attendees"][0]["email"] = json!("x");
        let bad_events = [
            missing,
            unknown,
            bad_response,
            extra_in_attendee,
            json!("not an object"),
            json!([]),
        ];
        let rows = [
            ("title", json!(5)),
            ("attendees", json!("Rahim")),
            ("occurrence_id", json!("")),
            ("occurrence_id", json!("x".repeat(256))),
            ("series_id", json!("")),
            ("series_id", json!("x".repeat(256))),
            ("start", json!("2026-11-13T10:00:00")),
            ("start", json!("2026-11-13")),
            ("end", json!("tomorrow")),
            ("end", json!("2026-11-13T10:00:00+0600")),
            ("end", json!("2026-11-13T10:00:00.Z")),
        ];
        let rejected_events = bad_events.into_iter().map(|e| {
            let mut v = desktop();
            v[EVENT] = e;
            v
        });
        for raw in rejected_events.chain(rows.into_iter().map(|(k, v)| with_event(k, v))) {
            let mut listed = raw.clone();
            listed["truncated"] = json!([EVENT]);
            for raw in [raw, listed] {
                let c = check(&raw).unwrap_or_else(|rule| panic!("{rule}: {raw}"));
                assert_eq!(c.calendar_event, None, "{raw}");
                assert_eq!(c.platform, Platform::Desktop);
                assert!(c.truncated.is_empty(), "{raw}");
            }
        }
        // The event rides on any context, and its loss leaves the rest alone.
        let mut meet = base();
        meet[EVENT] = json!({"title": 5});
        let c = check(&meet).unwrap();
        assert_eq!(c.calendar_event, None);
        assert_eq!(serde_json::to_value(&c).unwrap(), base());
    }

    #[test]
    fn event_identifiers_and_times_at_the_limits_pass() {
        for (key, value) in [
            ("occurrence_id", json!("x".repeat(255))),
            ("series_id", json!("ক".repeat(255))),
            ("start", json!("2026-11-13T10:00Z")),
            ("start", json!("2026-11-13T10:00:00Z")),
            ("end", json!("2026-11-13T10:00:00.123-05:30")),
            ("title", json!("")),
            ("organizer", Value::Null),
            ("calendar", Value::Null),
            ("description", Value::Null),
            ("attendees", json!([])),
        ] {
            let (_, truncated) = checked_event(key, value.clone());
            assert!(truncated.is_empty(), "{key}: {value}");
        }
    }

    #[test]
    fn long_event_values_are_cut_and_listed_once() {
        let listed = [EVENT.to_string()];
        let (e, truncated) = checked_event("title", json!("é".repeat(301)));
        assert_eq!(e.title.chars().count(), 300);
        assert_eq!(truncated, listed);
        let (e, truncated) = checked_event("organizer", json!("o".repeat(101)));
        assert_eq!(e.organizer.unwrap().chars().count(), 100);
        assert_eq!(truncated, listed);
        let (e, truncated) = checked_event("calendar", json!("c".repeat(101)));
        assert_eq!(e.calendar.unwrap().chars().count(), 100);
        assert_eq!(truncated, listed);
        let (e, truncated) = checked_event("description", json!("d".repeat(20_001)));
        assert_eq!(e.description.unwrap().chars().count(), 20_000);
        assert_eq!(truncated, listed);
        let (e, truncated) = checked_event(
            "attendees",
            json!([{"name": "n".repeat(101), "response": "accepted"}]),
        );
        assert_eq!(e.attendees[0].name.chars().count(), 100);
        assert_eq!(truncated, listed);
        let many: Vec<Value> = (0..201)
            .map(|i| json!({"name": format!("n{i}"), "response": "declined"}))
            .collect();
        let (e, truncated) = checked_event("attendees", json!(many));
        assert_eq!(e.attendees.len(), 200);
        assert_eq!(truncated, listed);

        // Several cuts, and a list that already names the event, still list it once.
        let mut raw = with_event("title", json!("t".repeat(301)));
        raw[EVENT]["description"] = json!("d".repeat(20_001));
        raw["truncated"] = json!([EVENT]);
        assert_eq!(check(&raw).unwrap().truncated, listed);
    }

    #[test]
    fn an_attendee_named_by_an_email_address_is_dropped() {
        let (e, truncated) = checked_event(
            "attendees",
            json!([{"name": "a@b.com", "response": "accepted"},
                {"name": "Rahim", "response": "tentative"}]),
        );
        assert_eq!(e.attendees.len(), 1);
        assert_eq!(e.attendees[0].name, "Rahim");
        assert_eq!(e.attendees[0].response, Response::Tentative);
        assert_eq!(truncated, [EVENT]);
    }

    #[test]
    fn unknown_truncated_names_are_dropped() {
        let c = check(&with("truncated", json!(["speaking", "bogus"]))).unwrap();
        assert_eq!(c.truncated, ["speaking"]);
    }

    #[test]
    fn load_returns_only_a_checked_context() {
        let dir = crate::test_support::temp("context-load");
        let path = dir.join("context.json");
        assert_eq!(load(&path), None);
        fs::write(&path, "{").unwrap();
        assert_eq!(load(&path), None);
        fs::write(&path, with("version", json!(2)).to_string()).unwrap();
        assert_eq!(load(&path), None);
        fs::write(&path, base().to_string()).unwrap();
        assert_eq!(load(&path), check(&base()).ok());
    }
}
