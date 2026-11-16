//! A small iCalendar reader for the calendar match (`docs/prds/context/meeting-context.md`, FR-8). It
//! reads only what the match needs, keeps times raw (no time-zone conversion) and returns an
//! error, never a panic, on malformed input. The error text never contains input text.

use std::collections::HashMap;
use std::fmt;

/// An unfolded content line longer than this is an error (a 5 MB `DESCRIPTION` is not read).
const MAX_LINE: usize = 1024 * 1024;
/// `plain_description` keeps at most this many characters.
const MAX_DESCRIPTION: usize = 20_000;
/// Real feeds nest 3 deep (`VCALENDAR`, `VEVENT`, `VALARM`); deeper is hostile.
const MAX_DEPTH: usize = 16;

#[derive(Debug, Default, PartialEq)]
pub struct Calendar {
    pub events: Vec<Event>,
    pub zones: HashMap<String, Zone>,
}

/// A `VTIMEZONE`'s `TZOFFSETTO` values, in seconds east of UTC.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Zone {
    pub standard: Option<i32>,
    pub daylight: Option<i32>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Event {
    pub summary: Option<String>,
    /// Unescaped, HTML intact (a Meet link inside an `href` stays matchable).
    pub description: Option<String>,
    pub location: Option<String>,
    pub url: Option<String>,
    /// `X-GOOGLE-CONFERENCE`.
    pub conference: Option<String>,
    pub cancelled: bool,
    pub start: When,
    /// Has an `RRULE`.
    pub series: bool,
    pub until: Option<When>,
    pub exdates: Vec<When>,
    /// Has a `RECURRENCE-ID`.
    pub is_override: bool,
    pub organizer: Option<String>,
    pub attendees: Vec<Attendee>,
}

/// Raw `CN`, `PARTSTAT` and `CUTYPE` parameters of an `ATTENDEE`.
#[derive(Debug, Clone, PartialEq)]
pub struct Attendee {
    pub name: Option<String>,
    pub partstat: Option<String>,
    pub cutype: Option<String>,
}

/// A date-time as written. `secs` is the civil date-time read as if it were UTC.
#[derive(Debug, Clone, PartialEq)]
pub enum When {
    Utc(i64),
    Floating(i64),
    Zoned {
        tzid: String,
        secs: i64,
    },
    /// All-day (`VALUE=DATE`), midnight of that day.
    Date(i64),
}

#[derive(Debug, PartialEq)]
pub struct Error(&'static str);

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "cannot read the calendar: {}", self.0)
    }
}

impl std::error::Error for Error {}

const NOT_ICALENDAR: Error = Error("not an iCalendar file");
const BAD_DATE: Error = Error("bad date");
const BAD_RRULE: Error = Error("bad RRULE");

pub fn parse(text: &str) -> Result<Calendar, Error> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut reader = Reader::default();
    let mut line = String::new();
    for raw in text.split('\n') {
        let raw = raw.strip_suffix('\r').unwrap_or(raw);
        if let Some(rest) = raw.strip_prefix([' ', '\t']) {
            if line.is_empty() {
                return Err(Error("continuation line without a line to continue"));
            }
            push_capped(&mut line, rest)?;
        } else {
            if !line.is_empty() {
                reader.line(&line)?;
                line.clear();
            }
            push_capped(&mut line, raw)?;
        }
    }
    if !line.is_empty() {
        reader.line(&line)?;
    }
    reader.finish()
}

fn push_capped(line: &mut String, part: &str) -> Result<(), Error> {
    if line.len() + part.len() > MAX_LINE {
        return Err(Error("line too long"));
    }
    line.push_str(part);
    Ok(())
}

impl Event {
    /// The description as plain text: HTML reduced to text, cut to 20,000 characters.
    pub fn plain_description(&self) -> String {
        let text = html_to_text(self.description.as_deref().unwrap_or(""));
        text.trim().chars().take(MAX_DESCRIPTION).collect()
    }
}

/// `text` reduced to plain text when it looks like HTML (calendar notes often are); other text is
/// returned unchanged. Linear in the input; no length cap.
pub fn html_to_text(text: &str) -> String {
    if looks_like_html(text) {
        collapse_blank_lines(&decode_entities(&strip_tags(text)))
    } else {
        text.to_owned()
    }
}

#[derive(Default)]
struct Reader {
    started: bool,
    stack: Vec<String>,
    calendar: Calendar,
    event: Option<Event>,
    start: Option<When>,
    zone: Option<(Option<String>, Zone)>,
}

struct Line<'a> {
    name: &'a str,
    params: Vec<(&'a str, &'a str)>,
    value: &'a str,
}

impl Reader {
    fn line(&mut self, raw: &str) -> Result<(), Error> {
        let line = split_line(raw)?;
        if line.name.eq_ignore_ascii_case("BEGIN") {
            return self.begin(line.value);
        }
        if line.name.eq_ignore_ascii_case("END") {
            return self.end(line.value);
        }
        if !self.started {
            return Err(NOT_ICALENDAR);
        }
        let parent = self.stack.iter().rev().nth(1).map(String::as_str);
        match (self.stack.last().map(String::as_str), parent) {
            (Some("VEVENT"), _) => self.event_property(&line),
            (Some("VTIMEZONE"), _) if line.name.eq_ignore_ascii_case("TZID") => {
                if let Some((tzid, _)) = self.zone.as_mut() {
                    *tzid = Some(unescape(line.value));
                }
                Ok(())
            }
            (Some(part @ ("STANDARD" | "DAYLIGHT")), Some("VTIMEZONE"))
                if line.name.eq_ignore_ascii_case("TZOFFSETTO") =>
            {
                let offset = parse_offset(line.value)?;
                if let Some((_, zone)) = self.zone.as_mut() {
                    // ponytail: last block wins when a zone lists several historical rules.
                    if part == "STANDARD" {
                        zone.standard = Some(offset);
                    } else {
                        zone.daylight = Some(offset);
                    }
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    fn begin(&mut self, name: &str) -> Result<(), Error> {
        let name = name.to_ascii_uppercase();
        if self.stack.is_empty() && name != "VCALENDAR" {
            return Err(NOT_ICALENDAR);
        }
        if self.stack.len() >= MAX_DEPTH {
            return Err(Error("components nested too deep"));
        }
        let in_calendar = self.stack.len() == 1;
        match name.as_str() {
            "VCALENDAR" => self.started = true,
            "VEVENT" if in_calendar => {
                self.start = None;
                self.event = Some(Event {
                    summary: None,
                    description: None,
                    location: None,
                    url: None,
                    conference: None,
                    cancelled: false,
                    // Replaced by `DTSTART` at `END:VEVENT`; an event without one is dropped.
                    start: When::Utc(0),
                    series: false,
                    until: None,
                    exdates: Vec::new(),
                    is_override: false,
                    organizer: None,
                    attendees: Vec::new(),
                });
            }
            "VTIMEZONE" if in_calendar => self.zone = Some((None, Zone::default())),
            _ => {}
        }
        self.stack.push(name);
        Ok(())
    }

    fn end(&mut self, name: &str) -> Result<(), Error> {
        if !self
            .stack
            .last()
            .is_some_and(|top| top.eq_ignore_ascii_case(name))
        {
            return Err(Error("END does not match BEGIN"));
        }
        let in_calendar = self.stack.len() == 2;
        let name = self.stack.pop().unwrap_or_default();
        match name.as_str() {
            "VEVENT" if in_calendar => {
                if let (Some(mut event), Some(start)) = (self.event.take(), self.start.take()) {
                    event.start = start;
                    self.calendar.events.push(event);
                }
            }
            "VTIMEZONE" if in_calendar => {
                if let Some((Some(tzid), zone)) = self.zone.take() {
                    self.calendar.zones.insert(tzid, zone);
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn event_property(&mut self, line: &Line) -> Result<(), Error> {
        let Some(event) = self.event.as_mut() else {
            return Ok(());
        };
        let value = line.value;
        match line.name.to_ascii_uppercase().as_str() {
            "SUMMARY" => event.summary = Some(unescape(value)),
            "DESCRIPTION" => event.description = Some(unescape(value)),
            "LOCATION" => event.location = Some(unescape(value)),
            "URL" => event.url = Some(unescape(value)),
            "X-GOOGLE-CONFERENCE" => event.conference = Some(unescape(value)),
            "STATUS" => event.cancelled = value.eq_ignore_ascii_case("CANCELLED"),
            "DTSTART" => self.start = Some(parse_when(&line.params, value)?),
            "RRULE" => {
                event.series = true;
                event.until = parse_until(value)?;
            }
            "EXDATE" => {
                for part in value.split(',') {
                    event.exdates.push(parse_when(&line.params, part)?);
                }
            }
            "RECURRENCE-ID" => event.is_override = true,
            "ORGANIZER" => event.organizer = param(&line.params, "CN").map(str::to_owned),
            "ATTENDEE" => event.attendees.push(Attendee {
                name: param(&line.params, "CN").map(str::to_owned),
                partstat: param(&line.params, "PARTSTAT").map(str::to_owned),
                cutype: param(&line.params, "CUTYPE").map(str::to_owned),
            }),
            _ => {}
        }
        Ok(())
    }

    fn finish(self) -> Result<Calendar, Error> {
        if !self.started {
            return Err(NOT_ICALENDAR);
        }
        if !self.stack.is_empty() {
            return Err(Error("truncated file"));
        }
        Ok(self.calendar)
    }
}

/// `NAME;PARAM=VALUE;...:value`, splitting at `:` and `;` outside double quotes.
fn split_line(line: &str) -> Result<Line<'_>, Error> {
    let (mut head, value) = split_unquoted(line, ':').ok_or(Error("line without a colon"))?;
    let mut params = Vec::new();
    let name = match split_unquoted(head, ';') {
        Some((name, rest)) => {
            head = rest;
            loop {
                let (param, rest) = split_unquoted(head, ';').unwrap_or((head, ""));
                if let Some((key, value)) = param.split_once('=') {
                    let value = value
                        .strip_prefix('"')
                        .and_then(|v| v.strip_suffix('"'))
                        .unwrap_or(value);
                    params.push((key, value));
                }
                if rest.is_empty() {
                    break;
                }
                head = rest;
            }
            name
        }
        None => head,
    };
    Ok(Line {
        name,
        params,
        value,
    })
}

fn split_unquoted(text: &str, separator: char) -> Option<(&str, &str)> {
    let mut quoted = false;
    let (at, _) = text.char_indices().find(|&(_, c)| {
        if c == '"' {
            quoted = !quoted;
        }
        c == separator && !quoted
    })?;
    Some((text.get(..at)?, text.get(at + separator.len_utf8()..)?))
}

fn param<'a>(params: &[(&str, &'a str)], name: &str) -> Option<&'a str> {
    params
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|&(_, value)| value)
}

fn unescape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n' | 'N') => out.push('\n'),
            Some(c @ (',' | ';' | '\\')) => out.push(c),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

/// `YYYYMMDD`, `YYYYMMDDTHHMMSS` or `YYYYMMDDTHHMMSSZ`, with the `TZID` and `VALUE` parameters.
fn parse_when(params: &[(&str, &str)], value: &str) -> Result<When, Error> {
    let days = parse_date(value.get(..8).ok_or(BAD_DATE)?)?;
    let is_date = param(params, "VALUE").is_some_and(|v| v.eq_ignore_ascii_case("DATE"));
    if value.len() == 8 {
        return Ok(When::Date(days * 86_400));
    }
    if is_date {
        return Err(BAD_DATE);
    }
    let time = value
        .get(8..)
        .and_then(|t| t.strip_prefix('T'))
        .ok_or(BAD_DATE)?;
    let (time, utc) = match time.strip_suffix('Z') {
        Some(time) => (time, true),
        None => (time, false),
    };
    if time.len() != 6 {
        return Err(BAD_DATE);
    }
    let (h, m, s) = (
        number(time, 0..2)?,
        number(time, 2..4)?,
        number(time, 4..6)?,
    );
    if h > 23 || m > 59 || s > 60 {
        return Err(BAD_DATE);
    }
    let secs = days * 86_400 + h * 3600 + m * 60 + s;
    Ok(match (utc, param(params, "TZID")) {
        (true, _) => When::Utc(secs),
        (false, Some(tzid)) => When::Zoned {
            tzid: tzid.to_owned(),
            secs,
        },
        (false, None) => When::Floating(secs),
    })
}

/// Days since 1970-01-01 of a `YYYYMMDD` date.
// ponytail: day checked against 31, not the month's length; Feb 30 reads as Mar 1/2.
fn parse_date(date: &str) -> Result<i64, Error> {
    let (y, m, d) = (
        number(date, 0..4)?,
        number(date, 4..6)?,
        number(date, 6..8)?,
    );
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return Err(BAD_DATE);
    }
    Ok(days_from_civil(y, m, d))
}

/// ASCII digits only (`parse` alone would accept a sign).
fn number(text: &str, range: std::ops::Range<usize>) -> Result<i64, Error> {
    let digits = text.get(range).ok_or(BAD_DATE)?;
    if !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Err(BAD_DATE);
    }
    digits.parse().map_err(|_| BAD_DATE)
}

/// Howard Hinnant's `days_from_civil`: (year, month, day) to days since 1970-01-01.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Reads `UNTIL` from an `RRULE`; every part must be `KEY=VALUE`.
fn parse_until(rule: &str) -> Result<Option<When>, Error> {
    let mut until = None;
    for part in rule.split(';').filter(|p| !p.is_empty()) {
        let (key, value) = part.split_once('=').ok_or(BAD_RRULE)?;
        if key.eq_ignore_ascii_case("UNTIL") {
            until = Some(parse_when(&[], value).map_err(|_| BAD_RRULE)?);
        }
    }
    Ok(until)
}

/// `+HHMM`, `-HHMM` or `+HHMMSS` to seconds.
fn parse_offset(value: &str) -> Result<i32, Error> {
    const BAD: Error = Error("bad TZOFFSETTO");
    let (sign, digits) = match (value.strip_prefix('+'), value.strip_prefix('-')) {
        (Some(d), _) => (1, d),
        (_, Some(d)) => (-1, d),
        _ => return Err(BAD),
    };
    if digits.len() != 4 && digits.len() != 6 {
        return Err(BAD);
    }
    let part = |range| number(digits, range).map_err(|_| BAD);
    let seconds = if digits.len() == 6 { part(4..6)? } else { 0 };
    let total = part(0..2)? * 3600 + part(2..4)? * 60 + seconds;
    Ok(sign * i32::try_from(total).map_err(|_| BAD)?)
}

/// `<` followed by a letter, `/` or `!`.
fn looks_like_html(text: &str) -> bool {
    text.split('<')
        .skip(1)
        .any(|after| after.starts_with(|c: char| c.is_ascii_alphabetic() || c == '/' || c == '!'))
}

/// Drops tags (and `<script>`/`<style>` contents); `<br>`, `</p>`, `</div>`, `</ul>` and `</ol>`
/// become newlines, and each `<li>` starts a `- ` line indented two spaces per nested list. A `<`
/// with no `>` after it is kept as text.
fn strip_tags(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut rest = html;
    let mut depth: usize = 0;
    while let Some((before, after)) = rest.split_once('<') {
        out.push_str(before);
        let Some((tag, tail)) = after.split_once('>') else {
            out.push('<');
            rest = after;
            break;
        };
        rest = tail;
        let closing = tag.starts_with('/');
        let name = tag
            .trim_start_matches('/')
            .split(|c: char| !c.is_ascii_alphanumeric())
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();
        match name.as_str() {
            "br" => out.push('\n'),
            "p" | "div" if closing => out.push('\n'),
            "ul" | "ol" if closing => {
                depth = depth.saturating_sub(1);
                if !out.ends_with('\n') {
                    out.push('\n');
                }
            }
            "ul" | "ol" => depth = depth.saturating_add(1).min(MAX_DEPTH),
            "li" if !closing => {
                if !out.is_empty() && !out.ends_with('\n') {
                    out.push('\n');
                }
                out.push_str(&"  ".repeat(depth.saturating_sub(1)));
                out.push_str("- ");
            }
            "script" | "style" if !closing => {
                let end = format!("</{name}");
                let at = rest
                    .as_bytes()
                    .windows(end.len())
                    .position(|w| w.eq_ignore_ascii_case(end.as_bytes()));
                rest = at.and_then(|at| rest.get(at..)).unwrap_or("");
            }
            _ => {}
        }
    }
    out.push_str(rest);
    out
}

fn decode_entities(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some((before, after)) = rest.split_once('&') {
        out.push_str(before);
        let decoded = after
            .char_indices()
            .take(11)
            .find(|&(_, c)| c == ';')
            .and_then(|(at, _)| Some((entity(after.get(..at)?)?, after.get(at + 1..)?)));
        match decoded {
            Some((c, tail)) => {
                out.push(c);
                rest = tail;
            }
            None => {
                out.push('&');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

fn entity(name: &str) -> Option<char> {
    Some(match name {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        "nbsp" => ' ',
        _ => {
            let number = name.strip_prefix('#')?;
            let code = match number.strip_prefix(['x', 'X']) {
                Some(hex) if hex.bytes().all(|b| b.is_ascii_hexdigit()) => {
                    u32::from_str_radix(hex, 16).ok()?
                }
                None if number.bytes().all(|b| b.is_ascii_digit()) => number.parse().ok()?,
                _ => return None,
            };
            char::from_u32(code)?
        }
    })
}

/// Trims each line's end and keeps at most one blank line in a row.
fn collapse_blank_lines(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut blank_before = false;
    for line in text.lines().map(str::trim_end) {
        if line.is_empty() && blank_before {
            continue;
        }
        blank_before = line.is_empty();
        out.push_str(line);
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOGLE: &str = include_str!("../tests/fixtures/calendar/google.ics");

    fn calendar(body: &str) -> String {
        format!("BEGIN:VCALENDAR\r\nVERSION:2.0\r\n{body}END:VCALENDAR\r\n")
    }

    /// One `VEVENT` with a UTC `DTSTART` and the given extra lines.
    fn event(lines: &str) -> String {
        calendar(&format!(
            "BEGIN:VEVENT\r\nDTSTART:20261021T090000Z\r\n{lines}END:VEVENT\r\n"
        ))
    }

    fn only_event(text: &str) -> Event {
        let mut events = parse(text).expect("parses").events;
        assert_eq!(events.len(), 1);
        events.remove(0)
    }

    fn description(html: &str) -> String {
        only_event(&event(""))
            .with_description(html)
            .plain_description()
    }

    impl Event {
        fn with_description(mut self, text: &str) -> Self {
            self.description = Some(text.to_owned());
            self
        }
    }

    /// 2026-10-21 00:00:00 UTC.
    const OCT_21: i64 = 1_792_540_800;

    #[test]
    fn days_from_civil_matches_known_dates() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(2000, 3, 1), 11_017);
        assert_eq!(days_from_civil(2026, 10, 21) * 86_400, OCT_21);
        assert_eq!(days_from_civil(1969, 12, 31), -1);
    }

    #[test]
    fn reads_a_google_feed() {
        let cal = parse(GOOGLE).expect("parses");
        assert_eq!(
            cal.zones.get("America/New_York"),
            Some(&Zone {
                standard: Some(-5 * 3600),
                daylight: Some(-4 * 3600),
            })
        );
        assert_eq!(
            cal.zones.get("Asia/Kolkata"),
            Some(&Zone {
                standard: Some(5 * 3600 + 30 * 60),
                daylight: None,
            })
        );
        assert_eq!(cal.events.len(), 2);

        let series = &cal.events[0];
        assert_eq!(series.summary.as_deref(), Some("Weekly sync, team; all"));
        assert_eq!(
            series.conference.as_deref(),
            Some("https://meet.google.com/abc-defg-hij")
        );
        assert_eq!(
            series.start,
            When::Zoned {
                tzid: "America/New_York".into(),
                secs: OCT_21 + 9 * 3600,
            }
        );
        assert!(series.series);
        assert_eq!(
            series.until,
            Some(When::Utc(
                days_from_civil(2026, 12, 31) * 86_400 + 13 * 3600 + 59 * 60 + 59
            ))
        );
        assert_eq!(series.exdates.len(), 2);
        assert!(!series.is_override && !series.cancelled);
        assert_eq!(series.organizer.as_deref(), Some("Doe, John"));
        assert_eq!(
            series.attendees,
            vec![
                Attendee {
                    name: Some("Doe, John".into()),
                    partstat: Some("ACCEPTED".into()),
                    cutype: Some("INDIVIDUAL".into()),
                },
                Attendee {
                    name: Some("Room 4".into()),
                    partstat: Some("ACCEPTED".into()),
                    cutype: Some("ROOM".into()),
                },
                Attendee {
                    name: Some("jane@example.com".into()),
                    partstat: Some("NEEDS-ACTION".into()),
                    cutype: None,
                },
            ]
        );
        let description = series.description.as_deref().expect("description");
        assert!(description.contains("href=\"https://meet.google.com/abc-defg-hij\""));
        assert_eq!(
            series.plain_description(),
            "Agenda:\n1. Plans & goals\n2. Q&A\n\nJoin: https://meet.google.com/abc-defg-hij"
        );

        let override_ = &cal.events[1];
        assert!(override_.is_override && override_.cancelled && !override_.series);
        assert_eq!(override_.description, None);
    }

    #[test]
    fn unfolds_crlf_lf_and_tab_continuations() {
        let text = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nDTSTART:20261021T090000Z\r\nSUMMARY:We\r\n ekly\n \tsy\n\tnc\r\nEND:VEVENT\nEND:VCALENDAR";
        assert_eq!(only_event(text).summary.as_deref(), Some("Weekly\tsync"));
    }

    #[test]
    fn unfolds_around_multi_byte_characters() {
        let text = event("SUMMARY:সাপ্তা\r\n হিক মি\r\n টিং 🙂\r\n ✓\r\n");
        assert_eq!(
            only_event(&text).summary.as_deref(),
            Some("সাপ্তাহিক মিটিং 🙂✓")
        );
    }

    #[test]
    fn unescapes_each_sequence() {
        let e = only_event(&event("DESCRIPTION:a\\nb\\Nc\\,d\\;e\\\\f\\x\\\r\n"));
        assert_eq!(e.description.as_deref(), Some("a\nb\nc,d;e\\f\\x\\"));
    }

    #[test]
    fn reads_quoted_parameters_and_values_with_colons() {
        let e = only_event(&event(
            "ATTENDEE;CN=\"Doe; John: Jr.\";PARTSTAT=TENTATIVE:mailto:j@x.com\r\nLOCATION:https://zoom.us/j/123?pwd=a:b\r\n",
        ));
        assert_eq!(e.attendees[0].name.as_deref(), Some("Doe; John: Jr."));
        assert_eq!(e.attendees[0].partstat.as_deref(), Some("TENTATIVE"));
        assert_eq!(e.location.as_deref(), Some("https://zoom.us/j/123?pwd=a:b"));
    }

    #[test]
    fn property_and_parameter_names_are_case_insensitive() {
        let text = "begin:vcalendar\r\nBegin:VEvent\r\nDtStart;tzid=Asia/Dhaka:20261021T090000\r\nsummary:x\r\nstatus:cancelled\r\nattendee;cn=A;cutype=room:mailto:a\r\nEnd:vevent\r\nend:VCALENDAR\r\n";
        let e = only_event(text);
        assert_eq!(e.summary.as_deref(), Some("x"));
        assert!(e.cancelled);
        assert_eq!(e.attendees[0].cutype.as_deref(), Some("room"));
        assert_eq!(
            e.start,
            When::Zoned {
                tzid: "Asia/Dhaka".into(),
                secs: OCT_21 + 9 * 3600,
            }
        );
    }

    #[test]
    fn reads_each_time_form() {
        let start = |line: &str| {
            let text = calendar(&format!("BEGIN:VEVENT\r\n{line}\r\nEND:VEVENT\r\n"));
            only_event(&text).start
        };
        let nine = OCT_21 + 9 * 3600;
        assert_eq!(start("DTSTART:20261021T090000Z"), When::Utc(nine));
        assert_eq!(start("DTSTART:20261021T090000"), When::Floating(nine));
        assert_eq!(
            start("DTSTART;TZID=\"W. Europe Standard Time\":20261021T090000"),
            When::Zoned {
                tzid: "W. Europe Standard Time".into(),
                secs: nine,
            }
        );
        assert_eq!(start("DTSTART;VALUE=DATE:20261021"), When::Date(OCT_21));
    }

    #[test]
    fn reads_repeated_and_comma_separated_exdates() {
        let e = only_event(&event(
            "EXDATE;TZID=Asia/Dhaka:20261022T090000,20261023T090000\r\nEXDATE:20261024T030000Z\r\n",
        ));
        let zoned = |day: i64| When::Zoned {
            tzid: "Asia/Dhaka".into(),
            secs: OCT_21 + day * 86_400 + 9 * 3600,
        };
        assert_eq!(
            e.exdates,
            vec![
                zoned(1),
                zoned(2),
                When::Utc(OCT_21 + 3 * 86_400 + 3 * 3600)
            ]
        );
    }

    #[test]
    fn nested_component_properties_do_not_replace_the_events() {
        let e = only_event(&event(
            "DESCRIPTION:agenda\r\nBEGIN:VALARM\r\nDESCRIPTION:reminder\r\nSUMMARY:alarm\r\nEND:VALARM\r\n",
        ));
        assert_eq!(e.description.as_deref(), Some("agenda"));
        assert_eq!(e.summary, None);
    }

    #[test]
    fn drops_an_event_without_dtstart_and_accepts_a_feed_without_events() {
        let text =
            calendar("BEGIN:VEVENT\r\nSUMMARY:x\r\nDTEND:20261021T100000Z\r\nEND:VEVENT\r\n");
        assert_eq!(parse(&text).expect("parses").events, vec![]);
        let zones_only = calendar(
            "BEGIN:VTIMEZONE\r\nTZID:Asia/Dhaka\r\nBEGIN:STANDARD\r\nTZOFFSETTO:+0600\r\nEND:STANDARD\r\nEND:VTIMEZONE\r\n",
        );
        let cal = parse(&zones_only).expect("parses");
        assert!(cal.events.is_empty());
        assert_eq!(cal.zones["Asia/Dhaka"].standard, Some(6 * 3600));
    }

    #[test]
    fn plain_description_reduces_google_html() {
        assert_eq!(
            description(
                "<p>Hi&nbsp;all,</p><br><br><br><div>Plan: <b>ship</b> &lt;v2&gt; &amp; &quot;more&quot; &#39;x&#39; &apos;y&apos; &#8212; &#x1F642;</div><ul><li>one</li><li>two</li></ul><script>alert(1)</script><STYLE>p{}</style>end &bogus; &#xZZ; a < b"
            ),
            "Hi all,\n\nPlan: ship <v2> & \"more\" 'x' 'y' — 🙂\n- one\n- two\nend &bogus; &#xZZ; a < b"
        );
    }

    #[test]
    fn nested_lists_become_indented_dashes() {
        assert_eq!(
            html_to_text(
                "<ol><li>Review <ol><li>Goals</li><li>On track? <br></li></ol></li><li>Plan</li></ol><br>Done"
            ),
            "- Review\n  - Goals\n  - On track?\n- Plan\n\nDone\n"
        );
        assert_eq!(html_to_text("a < b\n"), "a < b\n");
    }

    #[test]
    fn plain_description_is_linear_on_hostile_html() {
        let ampersands = format!("<b>{}", "&".repeat(1_000_000));
        assert_eq!(description(&ampersands), "&".repeat(20_000));
        let scripts = "<script></script x".repeat(60_000);
        // The last `</script x` has no `>`, so it stays as text.
        assert_eq!(description(&scripts), "</script x");
    }

    #[test]
    fn plain_description_leaves_plain_text_alone() {
        assert_eq!(
            description("a &amp; b\n\n\n\nc < 3"),
            "a &amp; b\n\n\n\nc < 3"
        );
        assert_eq!(only_event(&event("")).plain_description(), "");
    }

    #[test]
    fn plain_description_cuts_to_20000_characters() {
        let long = "ব".repeat(25_000);
        assert_eq!(description(&long).chars().count(), 20_000);
        let html = format!("<p>{}</p>", "x".repeat(25_000));
        assert_eq!(description(&html), "x".repeat(20_000));
    }

    #[test]
    fn malformed_input_is_an_error() {
        let five_mb = event(&format!("DESCRIPTION:{}\r\n", "x".repeat(5_000_000)));
        let five_mb_folded = event(&format!(
            "DESCRIPTION:x\r\n{}",
            " xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx\r\n"
                .repeat(70_000)
        ));
        let garbage = String::from_utf8_lossy(&[0xff, 0x00, 0xfe, b':', 0x80, b'\n', 0x00]);
        let bytes = event("SUMMARY:সাপ্তাহিক\r\n").into_bytes();
        let cut = String::from_utf8_lossy(bytes.get(..bytes.len() - 40).expect("long enough"));
        let cases = [
            ("leading continuation", " BEGIN:VCALENDAR\r\n".to_owned()),
            (
                "open VEVENT at EOF",
                "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nDTSTART:20261021T090000Z\r\nSUMMARY:x\r\n "
                    .to_owned(),
            ),
            (
                "missing END:VCALENDAR",
                "BEGIN:VCALENDAR\r\nVERSION:2.0\r\n".to_owned(),
            ),
            ("RRULE without =", event("RRULE:FREQ\r\n")),
            ("bad UNTIL", event("RRULE:FREQ=DAILY;UNTIL=garbage\r\n")),
            (
                "bad DTSTART",
                calendar("BEGIN:VEVENT\r\nDTSTART:notadate\r\nEND:VEVENT\r\n"),
            ),
            ("bad EXDATE", event("EXDATE:20261321T090000Z\r\n")),
            ("bad time", event("EXDATE:20261021T250000Z\r\n")),
            (
                "date with time",
                event("EXDATE;VALUE=DATE:20261021T090000\r\n"),
            ),
            ("signed digits", event("EXDATE:+2026102T090000Z\r\n")),
            (
                "bad offset",
                calendar(
                    "BEGIN:VTIMEZONE\r\nTZID:x\r\nBEGIN:DAYLIGHT\r\nTZOFFSETTO:0600\r\nEND:DAYLIGHT\r\nEND:VTIMEZONE\r\n",
                ),
            ),
            ("END mismatch", calendar("BEGIN:VEVENT\r\nEND:VTODO\r\n")),
            ("line without colon", event("SUMMARY\r\n")),
            ("5 MB description", five_mb),
            ("5 MB folded description", five_mb_folded),
            ("empty", String::new()),
            (
                "not iCalendar",
                "<html><body>Sign in</body></html>".to_owned(),
            ),
            (
                "property before BEGIN",
                "VERSION:2.0\r\nBEGIN:VCALENDAR\r\n".to_owned(),
            ),
            ("binary garbage", garbage.into_owned()),
            ("nested too deep", calendar(&"BEGIN:X\r\n".repeat(20))),
            ("cut mid-character", cut.into_owned()),
        ];
        for (name, text) in cases {
            assert!(parse(&text).is_err(), "{name} should be an error");
        }
    }

    #[test]
    fn error_text_never_contains_input() {
        let err = parse(&event("DESCRIPTION:secret\r\nRRULE:secret\r\n")).unwrap_err();
        assert_eq!(err.to_string(), "cannot read the calendar: bad RRULE");
    }

    #[test]
    fn every_truncation_of_a_feed_is_an_error_not_a_panic() {
        let bytes = GOOGLE.as_bytes();
        let end = GOOGLE
            .rfind("END:VCALENDAR")
            .expect("fixture ends the calendar");
        for len in 0..end {
            let text = String::from_utf8_lossy(bytes.get(..len).expect("in range"));
            assert!(
                parse(&text).is_err(),
                "prefix of {len} bytes should be an error"
            );
        }
    }
}
