//! `context.json`: what the extension read from the meeting tab, checked against the limits
//! table of `docs/prds/meeting-context.md`. A reject rule drops the whole context, a cut rule
//! trims the field and lists it in `truncated`. Rule names are the only thing ever logged.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::summarise::is_hh_mm_ss;

const MAX_BYTES: usize = 256 * 1024;
const MAX_TEXT: usize = 300;
const MAX_NAME: usize = 100;
const MAX_PARTICIPANTS: usize = 200;
const MAX_SPEAKING: usize = 5_000;
const MAX_SPEAKERS_PER_INTERVAL: usize = 10;
const KNOWN_TRUNCATED: [&str; 4] = ["tab_title", "meeting_title", "participants", "speaking"];

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Platform {
    Meet,
    Zoom,
    Teams,
    Other,
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
        && !["meet", "zoom", "teams", "other"].contains(&p.as_str())
    {
        return Err("platform");
    }
    let mut c: Context =
        serde_json::from_value(raw.clone()).map_err(|_| "wrong type or unknown field")?;
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
    for f in cut {
        if !c.truncated.iter().any(|t| t == f) {
            c.truncated.push(f.into());
        }
    }
    Ok(c)
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

fn time_zone_ok(z: &str) -> bool {
    !z.is_empty()
        && z.chars().count() <= 64
        && z.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '_' | '+' | '-'))
}

/// Cuts `s` to `max` characters; true when it changed.
fn cut_name(s: &mut String) -> bool {
    if s.chars().count() <= MAX_NAME {
        return false;
    }
    *s = s.chars().take(MAX_NAME).collect();
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
        cut |= cut_name(&mut p.name);
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
            cut |= cut_name(n);
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

    #[test]
    fn unknown_truncated_names_are_dropped() {
        let c = check(&with("truncated", json!(["speaking", "bogus"]))).unwrap();
        assert_eq!(c.truncated, ["speaking"]);
    }
}
