//! Picks the calendar event a recording belongs to (`docs/prds/context/meeting-context.md`, FR-8):
//! candidates by meeting ID, then the instance within 12 hours, else the latest series. Pure: no
//! I/O, never panics. Returns no email address.

use crate::ical::{Calendar, Event, When};

/// An instance this close to the recording start is "the same meeting"; an adjacent day is
/// 23–25 hours away and never is.
const WINDOW_SECS: i64 = 12 * 3600;

pub struct Recording {
    pub meeting_id: String,
    pub started_at_ms: i64,
    /// Minutes east of UTC.
    pub utc_offset_min: i32,
    pub time_zone: Option<String>,
}

#[derive(Debug, PartialEq)]
pub struct Meeting {
    pub title: Option<String>,
    pub description: String,
    pub organizer: Option<String>,
    pub attendees: Vec<MeetingAttendee>,
}

#[derive(Debug, PartialEq)]
pub struct MeetingAttendee {
    pub name: String,
    pub response: Response,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Copy, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Response {
    Accepted,
    Declined,
    Tentative,
    NeedsAction,
}

impl Response {
    pub fn as_str(self) -> &'static str {
        match self {
            Response::Accepted => "accepted",
            Response::Declined => "declined",
            Response::Tentative => "tentative",
            Response::NeedsAction => "needs_action",
        }
    }

    /// RFC 5545: a missing `PARTSTAT` is `NEEDS-ACTION`; so is any other value (`DELEGATED`).
    fn from_partstat(partstat: Option<&str>) -> Self {
        match partstat.map(str::to_ascii_uppercase).as_deref() {
            Some("ACCEPTED") => Response::Accepted,
            Some("DECLINED") => Response::Declined,
            Some("TENTATIVE") => Response::Tentative,
            _ => Response::NeedsAction,
        }
    }
}

/// The event `recording` belongs to, or `None`.
pub fn pick(calendar: &Calendar, recording: &Recording) -> Option<Meeting> {
    let clock = Clock {
        calendar,
        recording,
        rec: recording.started_at_ms.div_euclid(1000),
    };
    let candidates: Vec<(&Event, i64)> = calendar
        .events
        .iter()
        .filter(|e| mentions(e, &recording.meeting_id) && clock.live(e))
        .filter_map(|e| Some((e, clock.absolute(&e.start)?)))
        .collect();
    let instance = candidates
        .iter()
        .filter(|(_, start)| start.abs_diff(clock.rec) <= WINDOW_SECS.unsigned_abs())
        .min_by_key(|(e, start)| (start.abs_diff(clock.rec), e.series));
    let series = || {
        candidates
            .iter()
            .filter(|(e, start)| e.series && *start <= clock.rec)
            .max_by_key(|(_, start)| *start)
    };
    instance.or_else(series).map(|(e, _)| meeting(e))
}

struct Clock<'a> {
    calendar: &'a Calendar,
    recording: &'a Recording,
    /// Recording start, seconds since the epoch.
    rec: i64,
}

impl Clock<'_> {
    /// A civil time in the recording's own zone, as seconds since the epoch.
    fn local(&self, secs: i64) -> i64 {
        secs.saturating_sub(i64::from(self.recording.utc_offset_min) * 60)
    }

    /// `None` for an all-day date. A foreign `TZID` takes whichever of its `VTIMEZONE` offsets
    /// lands closer to the recording (no DST rules); an unknown one is read as local.
    fn absolute(&self, when: &When) -> Option<i64> {
        match when {
            When::Utc(secs) => Some(*secs),
            When::Floating(secs) => Some(self.local(*secs)),
            When::Zoned { tzid, secs } => {
                let own = self.recording.time_zone.as_deref() == Some(tzid.as_str());
                let zone = self.calendar.zones.get(tzid).filter(|_| !own);
                let closest = zone.and_then(|z| {
                    [z.standard, z.daylight]
                        .into_iter()
                        .flatten()
                        .map(|offset| secs.saturating_sub(i64::from(offset)))
                        .min_by_key(|t| t.abs_diff(self.rec))
                });
                Some(closest.unwrap_or_else(|| self.local(*secs)))
            }
            When::Date(_) => None,
        }
    }

    /// Not cancelled, not all-day, not a series that ended or skips an occurrence near the
    /// recording.
    fn live(&self, event: &Event) -> bool {
        if event.cancelled || matches!(event.start, When::Date(_)) {
            return false;
        }
        if !event.series {
            return true;
        }
        let ended = event.until.as_ref().is_some_and(|until| {
            let end = match until {
                // A date-only `UNTIL` runs to the end of that local day.
                When::Date(day) => self.local(day.saturating_add(86_400)),
                other => self.absolute(other).unwrap_or(i64::MAX),
            };
            end < self.rec
        });
        let skipped = event.exdates.iter().any(|exdate| {
            self.absolute(exdate)
                .is_some_and(|t| t.abs_diff(self.rec) <= WINDOW_SECS.unsigned_abs())
        });
        !ended && !skipped
    }
}

/// The ID appears in a matchable field as a whole token. The raw description is searched, so a
/// link inside an `href` still matches.
fn mentions(event: &Event, id: &str) -> bool {
    [
        &event.location,
        &event.description,
        &event.url,
        &event.conference,
    ]
    .into_iter()
    .flatten()
    .any(|text| has_token(text, id))
}

/// `id` occurs in `text` with no letter or digit right before or after it.
fn has_token(text: &str, id: &str) -> bool {
    if id.is_empty() {
        return false;
    }
    text.match_indices(id).any(|(at, _)| {
        let before = text.get(..at).and_then(|t| t.chars().next_back());
        let after = text.get(at + id.len()..).and_then(|t| t.chars().next());
        !before.is_some_and(char::is_alphanumeric) && !after.is_some_and(char::is_alphanumeric)
    })
}

fn meeting(event: &Event) -> Meeting {
    Meeting {
        title: event.summary.clone().filter(|s| !s.trim().is_empty()),
        description: event.plain_description(),
        organizer: event.organizer.as_deref().and_then(person),
        attendees: event
            .attendees
            .iter()
            .filter(|a| {
                !a.cutype.as_deref().is_some_and(|c| {
                    c.eq_ignore_ascii_case("ROOM") || c.eq_ignore_ascii_case("RESOURCE")
                })
            })
            .filter_map(|a| {
                Some(MeetingAttendee {
                    name: person(a.name.as_deref()?)?,
                    response: Response::from_partstat(a.partstat.as_deref()),
                })
            })
            .collect(),
    }
}

/// A `CN` that names a person: not empty, not an email address (D10).
fn person(cn: &str) -> Option<String> {
    let cn = cn.trim();
    (!cn.is_empty() && !cn.contains('@')).then(|| cn.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ical::{self, Attendee, Zone};

    const MEET: &str = "abc-defg-hij";
    const ZOOM: &str = "81234567890";
    /// 2026-10-01 00:00:00 UTC.
    const OCT_1: i64 = 1_790_812_800;
    const DHAKA: i32 = 6 * 60;

    #[test]
    fn a_response_serialises_as_its_name() {
        for response in [
            Response::Accepted,
            Response::Declined,
            Response::Tentative,
            Response::NeedsAction,
        ] {
            let json = serde_json::to_value(response).unwrap();
            assert_eq!(json, response.as_str());
            assert_eq!(serde_json::from_value::<Response>(json).unwrap(), response);
        }
        assert!(serde_json::from_str::<Response>("\"delegated\"").is_err());
    }

    /// A Dhaka recording of `id` at October `day`, `hour`:`minute` local time.
    fn dhaka(id: &str, day: i64, hour: i64, minute: i64) -> Recording {
        Recording {
            meeting_id: id.to_owned(),
            started_at_ms: utc(day, hour, minute) * 1000 - i64::from(DHAKA) * 60_000,
            utc_offset_min: DHAKA,
            time_zone: Some("Asia/Dhaka".to_owned()),
        }
    }

    /// October `day`, `hour`:`minute`, read as UTC.
    fn utc(day: i64, hour: i64, minute: i64) -> i64 {
        OCT_1 + (day - 1) * 86_400 + hour * 3600 + minute * 60
    }

    fn picked(ics: &str, recording: &Recording) -> Option<Meeting> {
        pick(&ical::parse(ics).expect("fixture parses"), recording)
    }

    fn title(ics: &str, recording: &Recording) -> Option<String> {
        picked(ics, recording).and_then(|m| m.title)
    }

    macro_rules! fixture {
        ($name:literal) => {
            include_str!(concat!("../tests/fixtures/calendar/", $name, ".ics"))
        };
    }

    #[test]
    fn picks_a_single_event_an_hour_before() {
        let ics = fixture!("single_hour_before");
        assert_eq!(
            title(ics, &dhaka(MEET, 22, 10, 0)).as_deref(),
            Some("Single an hour before")
        );
    }

    #[test]
    fn picks_an_override_at_the_recording_time_beside_its_series() {
        let ics = fixture!("override_beside_series");
        assert_eq!(
            title(ics, &dhaka(MEET, 22, 10, 0)).as_deref(),
            Some("Daily override")
        );
    }

    #[test]
    fn picks_the_later_of_two_series_sharing_a_link() {
        let ics = fixture!("two_series_split");
        assert_eq!(
            title(ics, &dhaka(MEET, 29, 10, 0)).as_deref(),
            Some("New series")
        );
    }

    #[test]
    fn picks_the_later_series_three_minutes_before_its_first_start() {
        let ics = fixture!("two_series_split");
        assert_eq!(
            title(ics, &dhaka(MEET, 15, 9, 57)).as_deref(),
            Some("New series")
        );
    }

    #[test]
    fn picks_a_new_series_three_minutes_before_its_first_start() {
        let ics = fixture!("new_series_before_start");
        assert_eq!(
            title(ics, &dhaka(MEET, 22, 9, 57)).as_deref(),
            Some("New weekly series")
        );
    }

    #[test]
    fn an_override_of_the_first_occurrence_beats_the_series_on_a_tie() {
        let m = picked(
            fixture!("new_series_first_overridden"),
            &dhaka(MEET, 22, 10, 0),
        )
        .expect("matched");
        assert_eq!(m.title.as_deref(), Some("Overridden first occurrence"));
        assert_eq!(m.description, "Changed agenda");
    }

    #[test]
    fn a_cancelled_instance_is_no_match() {
        let ics = fixture!("cancelled_instance");
        assert_eq!(picked(ics, &dhaka(MEET, 22, 10, 0)), None);
    }

    #[test]
    fn a_series_with_an_exdate_at_the_recording_is_no_match() {
        let ics = fixture!("exdate_at_recording");
        assert_eq!(picked(ics, &dhaka(MEET, 22, 10, 0)), None);
    }

    #[test]
    fn picks_the_latest_series_started_before_the_recording() {
        let ics = fixture!("latest_series");
        assert_eq!(
            title(ics, &dhaka(MEET, 22, 10, 0)).as_deref(),
            Some("Newer series")
        );
    }

    #[test]
    fn a_series_that_ended_is_no_match() {
        let ics = fixture!("ended_series");
        assert_eq!(picked(ics, &dhaka(MEET, 22, 10, 0)), None);
    }

    #[test]
    fn matches_a_utc_event_on_the_next_utc_day() {
        // New York 23:50 on the 21st is 03:50 UTC on the 22nd.
        let recording = Recording {
            meeting_id: MEET.to_owned(),
            started_at_ms: utc(22, 3, 50) * 1000,
            utc_offset_min: -4 * 60,
            time_zone: Some("America/New_York".to_owned()),
        };
        assert_eq!(
            title(fixture!("utc_next_day"), &recording).as_deref(),
            Some("UTC event next UTC day")
        );
    }

    #[test]
    fn matches_a_midnight_event_recorded_two_minutes_before() {
        let ics = fixture!("local_midnight_single");
        assert_eq!(
            title(ics, &dhaka(MEET, 22, 23, 58)).as_deref(),
            Some("Midnight single")
        );
    }

    #[test]
    fn a_midnight_series_beats_the_previous_days_override() {
        let ics = fixture!("midnight_series_prev_override");
        assert_eq!(
            title(ics, &dhaka(MEET, 22, 23, 58)).as_deref(),
            Some("Midnight daily series")
        );
    }

    #[test]
    fn an_exdate_a_day_away_in_new_york_does_not_skip_the_series() {
        // Dhaka Thursday 07:00 is New York Wednesday 21:00.
        let ics = fixture!("ny_exdate_thursday");
        assert_eq!(
            title(ics, &dhaka(MEET, 22, 7, 0)).as_deref(),
            Some("NY daily series")
        );
    }

    #[test]
    fn a_zoom_id_inside_a_longer_digit_run_is_no_match() {
        let ics = fixture!("zoom_in_digit_run");
        assert_eq!(picked(ics, &dhaka(ZOOM, 22, 10, 0)), None);
    }

    #[test]
    fn leaves_out_rooms_resources_and_names_that_are_not_names() {
        let m = picked(fixture!("attendees"), &dhaka(MEET, 22, 10, 0)).expect("matched");
        assert_eq!(m.organizer.as_deref(), Some("Doe, John"));
        let attendees: Vec<(&str, &str)> = m
            .attendees
            .iter()
            .map(|a| (a.name.as_str(), a.response.as_str()))
            .collect();
        assert_eq!(
            attendees,
            vec![
                ("Doe, John", "accepted"),
                ("Ali", "tentative"),
                ("Bo", "declined"),
                ("Cy", "needs_action"),
                ("Di", "needs_action"),
            ]
        );
        assert!(!format!("{m:?}").contains('@'));
    }

    #[test]
    fn matches_a_new_york_override_that_is_tomorrow_locally() {
        let ics = fixture!("ny_override_next_day");
        assert_eq!(
            title(ics, &dhaka(MEET, 22, 7, 0)).as_deref(),
            Some("NY evening override")
        );
    }

    #[test]
    fn never_picks_thursdays_new_york_override_for_wednesdays_meeting() {
        let ics = fixture!("ny_daily_override_thursday");
        assert_eq!(
            title(ics, &dhaka(MEET, 22, 7, 0)).as_deref(),
            Some("NY daily series")
        );
    }

    #[test]
    fn a_los_angeles_series_beats_tomorrows_override() {
        // Los Angeles Wednesday 18:00 is Dhaka Thursday 07:00.
        let ics = fixture!("la_daily_tomorrow_override");
        assert_eq!(
            title(ics, &dhaka(MEET, 22, 7, 0)).as_deref(),
            Some("LA daily series")
        );
    }

    #[test]
    fn an_own_zone_series_beats_overrides_a_day_either_side() {
        let ics = fixture!("own_zone_daily_overrides");
        assert_eq!(
            title(ics, &dhaka(MEET, 22, 10, 0)).as_deref(),
            Some("Own-zone daily series")
        );
    }

    #[test]
    fn a_zoom_personal_id_used_yesterday_is_no_match() {
        let ics = fixture!("zoom_personal_yesterday");
        assert_eq!(picked(ics, &dhaka(ZOOM, 22, 10, 0)), None);
    }

    #[test]
    fn a_foreign_tzid_without_vtimezone_is_read_as_local() {
        // Accepted wrong match: Los Angeles 10:00 is really 13 hours later.
        let ics = fixture!("foreign_tzid_no_vtimezone");
        assert_eq!(
            title(ics, &dhaka(MEET, 22, 10, 0)).as_deref(),
            Some("Foreign zone read as local")
        );
    }

    #[test]
    fn the_id_must_be_a_whole_token() {
        assert!(has_token("https://meet.google.com/abc-defg-hij", MEET));
        assert!(has_token("(abc-defg-hij)", MEET));
        assert!(has_token("abc-defg-hij", MEET));
        assert!(has_token("xabc-defg-hijx abc-defg-hij?", MEET));
        assert!(!has_token("xabc-defg-hijx", MEET));
        assert!(!has_token("abc-defg-hijk", MEET));
        assert!(!has_token("éabc-defg-hij", MEET));
        assert!(!has_token("zoom.us/j/9812345678901", ZOOM));
        assert!(!has_token("anything", ""));
    }

    #[test]
    fn matches_the_id_inside_an_html_description() {
        let cal = ical::parse(fixture!("google")).expect("parses");
        // The series starts New York Wednesday 09:00, Dhaka 19:00.
        let m = pick(&cal, &dhaka(MEET, 21, 19, 0)).expect("matched");
        assert_eq!(m.title.as_deref(), Some("Weekly sync, team; all"));
        assert_eq!(m.organizer.as_deref(), Some("Doe, John"));
        assert_eq!(m.attendees.len(), 1);
        assert!(m.description.starts_with("Agenda:\n1. Plans & goals"));
    }

    fn calendar_with(zones: &[(&str, Zone)]) -> Calendar {
        Calendar {
            events: Vec::new(),
            zones: zones
                .iter()
                .map(|(id, z)| ((*id).to_owned(), z.clone()))
                .collect(),
        }
    }

    fn zoned(tzid: &str, secs: i64) -> When {
        When::Zoned {
            tzid: tzid.to_owned(),
            secs,
        }
    }

    #[test]
    fn absolute_reads_each_time_form() {
        let ny = Zone {
            standard: Some(-5 * 3600),
            daylight: Some(-4 * 3600),
        };
        let cal = calendar_with(&[
            ("America/New_York", ny),
            (
                "Asia/Dhaka",
                Zone {
                    standard: Some(5 * 3600),
                    daylight: Some(7 * 3600),
                },
            ),
            ("Empty", Zone::default()),
        ]);
        let recording = dhaka(MEET, 22, 10, 0);
        let clock = Clock {
            calendar: &cal,
            recording: &recording,
            rec: recording.started_at_ms / 1000,
        };
        let nine = utc(22, 9, 0);
        assert_eq!(clock.absolute(&When::Utc(nine)), Some(nine));
        assert_eq!(clock.absolute(&When::Floating(nine)), Some(nine - 6 * 3600));
        // Own zone, a zone with no offsets, and an unknown zone are local.
        for tzid in ["Asia/Dhaka", "Empty", "Europe/Berlin"] {
            assert_eq!(clock.absolute(&zoned(tzid, nine)), Some(nine - 6 * 3600));
        }
        assert_eq!(
            clock.absolute(&zoned("America/New_York", nine)),
            Some(nine + 4 * 3600)
        );
        assert_eq!(clock.absolute(&When::Date(utc(22, 0, 0))), None);
    }

    #[test]
    fn a_foreign_zone_takes_the_offset_closer_to_the_recording() {
        let cal = calendar_with(&[(
            "America/New_York",
            Zone {
                standard: Some(-5 * 3600),
                daylight: Some(-4 * 3600),
            },
        )]);
        // 09:00 New York on 2026-10-21 (EDT) and on 2027-01-13 (EST), recorded on time.
        let january = utc(21, 0, 0) + 84 * 86_400;
        for (civil, real) in [
            (utc(21, 9, 0), utc(21, 13, 0)),
            (january + 9 * 3600, january + 14 * 3600),
        ] {
            let recording = Recording {
                meeting_id: MEET.to_owned(),
                started_at_ms: real * 1000,
                utc_offset_min: DHAKA,
                time_zone: None,
            };
            let clock = Clock {
                calendar: &cal,
                recording: &recording,
                rec: real,
            };
            assert_eq!(
                clock.absolute(&zoned("America/New_York", civil)),
                Some(real)
            );
        }
    }

    #[test]
    fn partstat_maps_to_a_response() {
        for (partstat, response) in [
            (Some("ACCEPTED"), Response::Accepted),
            (Some("accepted"), Response::Accepted),
            (Some("DECLINED"), Response::Declined),
            (Some("Tentative"), Response::Tentative),
            (Some("NEEDS-ACTION"), Response::NeedsAction),
            (Some("DELEGATED"), Response::NeedsAction),
            (None, Response::NeedsAction),
        ] {
            assert_eq!(Response::from_partstat(partstat), response);
        }
    }

    fn event(attendees: Vec<Attendee>, organizer: Option<&str>) -> Event {
        Event {
            summary: Some("  ".to_owned()),
            description: None,
            location: None,
            url: None,
            conference: Some(MEET.to_owned()),
            cancelled: false,
            start: When::Utc(utc(22, 4, 0)),
            series: false,
            until: None,
            exdates: Vec::new(),
            is_override: false,
            organizer: organizer.map(str::to_owned),
            attendees,
        }
    }

    fn attendee(name: Option<&str>, cutype: Option<&str>) -> Attendee {
        Attendee {
            name: name.map(str::to_owned),
            partstat: None,
            cutype: cutype.map(str::to_owned),
        }
    }

    #[test]
    fn email_names_rooms_and_resources_are_left_out() {
        let e = event(
            vec![
                attendee(Some("Ann"), Some("individual")),
                attendee(Some("Room 4"), Some("room")),
                attendee(Some("Beamer"), Some("Resource")),
                attendee(Some("ann@example.com"), None),
                attendee(Some(""), None),
                attendee(None, None),
            ],
            Some("boss@example.com"),
        );
        let m = meeting(&e);
        assert_eq!(m.organizer, None);
        assert_eq!(m.title, None);
        assert_eq!(m.description, "");
        assert_eq!(
            m.attendees,
            vec![MeetingAttendee {
                name: "Ann".to_owned(),
                response: Response::NeedsAction,
            }]
        );
        assert_eq!(meeting(&event(Vec::new(), None)).organizer, None);
    }

    fn one(event: Event, recording: &Recording) -> Option<Meeting> {
        let cal = Calendar {
            events: vec![event],
            zones: Default::default(),
        };
        pick(&cal, recording)
    }

    #[test]
    fn an_instance_matches_up_to_twelve_hours_away() {
        let recording = dhaka(MEET, 22, 10, 0);
        let at = |hours: i64| Event {
            start: When::Utc(utc(22, 4, 0) + hours * 3600),
            ..event(Vec::new(), None)
        };
        assert!(one(at(11), &recording).is_some());
        assert!(one(at(-12), &recording).is_some());
        assert_eq!(one(at(13), &recording), None);
        assert_eq!(one(at(-13), &recording), None);
    }

    #[test]
    fn the_url_field_is_searched_for_the_id() {
        let e = Event {
            conference: None,
            url: Some(format!("https://meet.google.com/{MEET}")),
            ..event(Vec::new(), None)
        };
        assert!(one(e, &dhaka(MEET, 22, 10, 0)).is_some());
    }

    #[test]
    fn an_all_day_event_is_skipped() {
        let e = Event {
            start: When::Date(utc(22, 0, 0)),
            ..event(Vec::new(), None)
        };
        assert_eq!(one(e, &dhaka(MEET, 22, 10, 0)), None);
    }

    #[test]
    fn a_date_only_until_runs_to_the_end_of_that_local_day() {
        let series = |until_day: i64| Event {
            start: When::Utc(utc(1, 4, 0)),
            series: true,
            until: Some(When::Date(utc(until_day, 0, 0))),
            ..event(Vec::new(), None)
        };
        let recording = dhaka(MEET, 22, 10, 0);
        assert!(one(series(22), &recording).is_some());
        assert_eq!(one(series(21), &recording), None);
    }

    #[test]
    fn never_panics_at_the_edges() {
        let recording = |id: &str, ms: i64, offset: i32| Recording {
            meeting_id: id.to_owned(),
            started_at_ms: ms,
            utc_offset_min: offset,
            time_zone: None,
        };
        let extreme = Event {
            until: Some(When::Utc(i64::MIN)),
            exdates: vec![When::Utc(i64::MIN), When::Date(i64::MAX - 1)],
            series: true,
            ..event(Vec::new(), None)
        };
        let far = Event {
            start: When::Floating(i64::MAX),
            ..event(Vec::new(), None)
        };
        let cal = Calendar {
            events: vec![extreme, far],
            zones: Default::default(),
        };
        for r in [
            recording(MEET, i64::MIN, 840),
            recording(MEET, i64::MAX, -720),
            recording("", 0, 0),
        ] {
            let _ = pick(&cal, &r);
            let _ = pick(&Calendar::default(), &r);
        }
    }
}
