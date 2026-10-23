//! Writes the meeting note into the Obsidian vault, one note per recording.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, bail};

use crate::calendar::{Meeting, Response};
use crate::{Language, context};

/// Values from other people are cut to this many characters (PRD FR-10).
const MAX_VALUE_CHARS: usize = 300;

pub struct Note<'a> {
    /// `YYYY-MM-DDTHH:MM`, optionally followed by seconds and `Z`.
    pub recorded_at: &'a str,
    pub title: &'a str,
    pub language: Language,
    pub speakers: &'a [String],
    pub recording: &'a Path,
    pub transcript: &'a Path,
    pub body: &'a str,
    /// Calendar title, else Meet's; from other people.
    pub meeting_title: Option<&'a str>,
    /// From other people.
    pub attendees: &'a [String],
    pub calendar: Option<CalendarOutcome>,
}

/// How the calendar match went; written only when a calendar is configured.
// ponytail: unused until #103 supplies it.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CalendarOutcome {
    Matched,
    NoMatch,
    FetchFailed,
}

impl CalendarOutcome {
    fn as_str(self) -> &'static str {
        match self {
            CalendarOutcome::Matched => "matched",
            CalendarOutcome::NoMatch => "no_match",
            CalendarOutcome::FetchFailed => "fetch_failed",
        }
    }
}

/// The note's `meeting_title` and `attendees`: the calendar title, else Meet's; the calendar
/// attendees who did not decline, else the Meet roster.
// ponytail: unused until #103 supplies it.
#[allow(dead_code)]
pub fn note_people(
    context: Option<&context::Context>,
    meeting: Option<&Meeting>,
) -> (Option<String>, Vec<String>) {
    let title = meeting
        .and_then(|m| m.title.clone())
        .filter(|t| !clean(t).trim().is_empty())
        .or_else(|| context.and_then(|c| c.meeting_title.clone()));
    let invited: Vec<String> = meeting
        .into_iter()
        .flat_map(|m| &m.attendees)
        .filter(|a| a.response != Response::Declined && !clean(&a.name).is_empty())
        .map(|a| a.name.clone())
        .collect();
    let attendees = if invited.is_empty() {
        context
            .into_iter()
            .flat_map(|c| &c.participants)
            .map(|p| p.name.clone())
            .collect()
    } else {
        invited
    };
    (title, attendees)
}

/// Writes `note` to `<vault_dir>/<folder>/YYYY-MM-DD HHMM <title>.md`, replacing the
/// existing note for the same recording (matched by its `recording` frontmatter).
pub fn write_note(vault_dir: &Path, folder: &str, note: &Note) -> anyhow::Result<PathBuf> {
    if !vault_dir.is_dir() {
        bail!(
            "vault directory {} does not exist; run `meeting install`",
            vault_dir.display()
        );
    }
    let at = note.recorded_at;
    let (Some(date), Some(hour), Some(minute)) = (at.get(0..10), at.get(11..13), at.get(14..16))
    else {
        bail!("recorded_at {at:?} is not YYYY-MM-DDTHH:MM");
    };

    let dir = vault_dir.join(folder);
    fs::create_dir_all(&dir).with_context(|| format!("cannot create {}", dir.display()))?;

    let existing = find_existing(&dir, note.recording);
    let stem = format!("{date} {hour}{minute} {}", sanitise(note.title));
    let mut target = dir.join(format!("{stem}.md"));
    let mut n = 2;
    while target.exists() && existing.as_ref() != Some(&target) {
        target = dir.join(format!("{stem} {n}.md"));
        n += 1;
    }

    let tmp = dir.join(format!(
        ".{}.tmp",
        target.file_name().unwrap_or_default().to_string_lossy()
    ));
    fs::write(&tmp, render(date, note))
        .with_context(|| format!("cannot write {}", tmp.display()))?;
    fs::rename(&tmp, &target).with_context(|| format!("cannot write {}", target.display()))?;

    // New note first, so a crash here leaves two notes rather than none.
    if let Some(old) = existing.filter(|old| *old != target) {
        fs::remove_file(&old).with_context(|| format!("cannot remove {}", old.display()))?;
    }
    tracing::info!("wrote note {}", target.display());
    Ok(target)
}

/// Points the note for `old_recording` at the new recording and transcript paths, touching
/// only those two frontmatter lines. No note for the recording is not an error.
pub fn repoint_note(
    vault_dir: &Path,
    folder: &str,
    old_recording: &Path,
    new_recording: &Path,
    new_transcript: &Path,
) -> anyhow::Result<()> {
    let Some(note) = find_existing(&vault_dir.join(folder), old_recording) else {
        return Ok(());
    };
    let text =
        fs::read_to_string(&note).with_context(|| format!("cannot read {}", note.display()))?;
    let quote =
        |p: &Path| serde_json::to_string(&p.to_string_lossy()).expect("a string serialises");
    let (recording, transcript) = (quote(new_recording), quote(new_transcript));
    let mut in_frontmatter = false;
    let mut out = String::with_capacity(text.len());
    for (i, line) in text.split_inclusive('\n').enumerate() {
        let body = line.trim_end_matches(['\r', '\n']);
        let ending = &line[body.len()..];
        if i == 0 {
            in_frontmatter = body == "---";
        } else if in_frontmatter && body == "---" {
            in_frontmatter = false;
        }
        match (in_frontmatter && i > 0, body) {
            (true, b) if b.starts_with("recording: ") => {
                out.push_str(&format!("recording: {recording}{ending}"))
            }
            (true, b) if b.starts_with("transcript: ") => {
                out.push_str(&format!("transcript: {transcript}{ending}"))
            }
            _ => out.push_str(line),
        }
    }
    let tmp = note.with_file_name(format!(
        ".{}.tmp",
        note.file_name().unwrap_or_default().to_string_lossy()
    ));
    fs::write(&tmp, out).with_context(|| format!("cannot write {}", tmp.display()))?;
    fs::rename(&tmp, &note).with_context(|| format!("cannot write {}", note.display()))
}

fn render(date: &str, note: &Note) -> String {
    let language = match note.language {
        Language::Bn => "bn",
        Language::En => "en",
    };
    // A JSON string is a valid YAML double-quoted scalar, so no YAML crate is needed.
    let quote = |s: &str| serde_json::to_string(s).expect("a string always serialises");
    let speakers = note
        .speakers
        .iter()
        .map(|s| quote(s))
        .collect::<Vec<_>>()
        .join(", ");
    let mut extra = String::new();
    if let Some(title) = note.meeting_title.map(clean).filter(|t| !t.is_empty()) {
        extra.push_str(&format!("meeting_title: {}\n", quote(&title)));
    }
    let attendees: Vec<String> = note
        .attendees
        .iter()
        .map(|a| clean(a))
        .filter(|a| !a.is_empty())
        .map(|a| quote(&a))
        .collect();
    if !attendees.is_empty() {
        extra.push_str(&format!("attendees: [{}]\n", attendees.join(", ")));
    }
    if let Some(outcome) = note.calendar {
        extra.push_str(&format!("calendar: {}\n", quote(outcome.as_str())));
    }
    format!(
        "---\ndate: {date}\nlanguage: {language}\nspeakers: [{speakers}]\nrecording: {}\ntranscript: {}\n{extra}---\n\n{}",
        quote(&note.recording.to_string_lossy()),
        quote(&note.transcript.to_string_lossy()),
        note.body
    )
}

/// A value from other people, safe on one frontmatter line: no control characters or line
/// separators (a newline could forge a `recording:` line), at most 300 characters.
fn clean(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_control() && !matches!(c, '\u{2028}' | '\u{2029}'))
        .take(MAX_VALUE_CHARS)
        .collect()
}

/// The `.md` file directly in `dir` whose `recording` frontmatter equals `recording`.
fn find_existing(dir: &Path, recording: &Path) -> Option<PathBuf> {
    let want = recording.to_string_lossy();
    let mut notes: Vec<PathBuf> = fs::read_dir(dir)
        .ok()?
        .filter_map(|entry| Some(entry.ok()?.path()))
        .filter(|path| path.extension().is_some_and(|ext| ext == "md"))
        .collect();
    notes.sort();
    notes.into_iter().find(|path| {
        fs::read_to_string(path)
            .ok()
            .and_then(|text| frontmatter_recording(&text))
            .is_some_and(|found| found == want)
    })
}

fn frontmatter_recording(text: &str) -> Option<String> {
    let mut lines = text.lines();
    if lines.next()? != "---" {
        return None;
    }
    lines
        .take_while(|line| *line != "---")
        .find_map(|line| line.strip_prefix("recording: "))
        .and_then(|value| serde_json::from_str(value).ok())
}

fn sanitise(title: &str) -> String {
    let clean: String = title
        .chars()
        .map(|c| {
            if matches!(c, '/' | '\\' | ':') || c.is_control() {
                '-'
            } else {
                c
            }
        })
        .collect();
    let clean = clean
        .trim_matches(' ')
        .trim_start_matches('.')
        .trim_matches(' ');
    if clean.is_empty() {
        "Meeting".into()
    } else {
        clean.into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const REC: &str = "/Users/a/Downloads/Meetings/meeting-2026-10-19_1430/recording.webm";

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("meeting-test-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn note<'a>(title: &'a str, body: &'a str, speakers: &'a [String], rec: &'a Path) -> Note<'a> {
        Note {
            recorded_at: "2026-10-19T14:30",
            title,
            language: Language::Bn,
            speakers,
            recording: rec,
            transcript: Path::new(
                "/Users/a/Downloads/Meetings/meeting-2026-10-19_1430/transcript.md",
            ),
            body,
            meeting_title: None,
            attendees: &[],
            calendar: None,
        }
    }

    fn speakers() -> Vec<String> {
        vec!["Rahim (inferred)".into(), "Speaker 1".into()]
    }

    fn md_files(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn first_write_has_exact_name_and_contents() {
        let vault = temp("first");
        let sp = speakers();
        let path = write_note(
            &vault,
            "Meetings",
            &note("Weekly sync", "<body>", &sp, Path::new(REC)),
        )
        .unwrap();
        assert_eq!(path, vault.join("Meetings/2026-10-19 1430 Weekly sync.md"));
        let expected = format!(
            "---\ndate: 2026-10-19\nlanguage: bn\nspeakers: [\"Rahim (inferred)\", \"Speaker 1\"]\nrecording: \"{REC}\"\ntranscript: \"/Users/a/Downloads/Meetings/meeting-2026-10-19_1430/transcript.md\"\n---\n\n<body>"
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), expected);
        assert_eq!(
            md_files(&vault.join("Meetings")).len(),
            1,
            "no .tmp left behind"
        );
    }

    #[test]
    fn rerun_with_new_title_leaves_one_note() {
        let vault = temp("retitle");
        let sp = speakers();
        write_note(
            &vault,
            "Meetings",
            &note("Weekly sync", "old", &sp, Path::new(REC)),
        )
        .unwrap();
        let path = write_note(
            &vault,
            "Meetings",
            &note("Q4 planning", "new", &sp, Path::new(REC)),
        )
        .unwrap();
        assert_eq!(
            md_files(&vault.join("Meetings")),
            ["2026-10-19 1430 Q4 planning.md"]
        );
        assert!(fs::read_to_string(path).unwrap().ends_with("new"));
    }

    #[test]
    fn rerun_with_same_title_overwrites() {
        let vault = temp("same");
        let sp = speakers();
        write_note(
            &vault,
            "Meetings",
            &note("Weekly sync", "old", &sp, Path::new(REC)),
        )
        .unwrap();
        let path = write_note(
            &vault,
            "Meetings",
            &note("Weekly sync", "new", &sp, Path::new(REC)),
        )
        .unwrap();
        assert_eq!(md_files(&vault.join("Meetings")).len(), 1);
        assert!(fs::read_to_string(path).unwrap().ends_with("new"));
    }

    #[test]
    fn other_recordings_note_is_kept() {
        let vault = temp("other");
        let sp = speakers();
        let a = write_note(
            &vault,
            "Meetings",
            &note("Weekly sync", "a", &sp, Path::new("/r/a.webm")),
        )
        .unwrap();
        let b = write_note(
            &vault,
            "Meetings",
            &note("Weekly sync", "b", &sp, Path::new("/r/b.webm")),
        )
        .unwrap();
        assert_eq!(b, vault.join("Meetings/2026-10-19 1430 Weekly sync 2.md"));
        assert!(fs::read_to_string(a).unwrap().ends_with('a'));
        let c = write_note(
            &vault,
            "Meetings",
            &note("Weekly sync", "c", &sp, Path::new("/r/c.webm")),
        )
        .unwrap();
        assert_eq!(c, vault.join("Meetings/2026-10-19 1430 Weekly sync 3.md"));
    }

    #[test]
    fn renamed_note_is_still_found() {
        let vault = temp("renamed");
        let sp = speakers();
        let first = write_note(
            &vault,
            "Meetings",
            &note("Weekly sync", "old", &sp, Path::new(REC)),
        )
        .unwrap();
        let renamed = vault.join("Meetings/whatever.md");
        fs::rename(first, &renamed).unwrap();
        write_note(
            &vault,
            "Meetings",
            &note("Weekly sync", "new", &sp, Path::new(REC)),
        )
        .unwrap();
        assert_eq!(
            md_files(&vault.join("Meetings")),
            ["2026-10-19 1430 Weekly sync.md"]
        );
    }

    #[test]
    fn titles_become_valid_file_names() {
        assert_eq!(sanitise("a/b: c"), "a-b- c");
        assert_eq!(sanitise(".hidden"), "hidden");
        assert_eq!(sanitise("x\ny\\z"), "x-y-z");
        assert_eq!(sanitise("  ..  "), "Meeting");
    }

    #[test]
    fn recording_with_quote_and_backslash_round_trips() {
        let vault = temp("quote");
        let sp = speakers();
        let rec = Path::new("/r/we\"ird\\name.webm");
        write_note(&vault, "Meetings", &note("One", "old", &sp, rec)).unwrap();
        write_note(&vault, "Meetings", &note("Two", "new", &sp, rec)).unwrap();
        assert_eq!(
            md_files(&vault.join("Meetings")),
            ["2026-10-19 1430 Two.md"]
        );
    }

    #[test]
    fn missing_vault_fails_and_missing_folder_is_created() {
        let sp = speakers();
        let missing =
            std::env::temp_dir().join(format!("meeting-test-{}-nope", std::process::id()));
        let err =
            write_note(&missing, "Meetings", &note("T", "b", &sp, Path::new(REC))).unwrap_err();
        assert_eq!(
            err.to_string(),
            format!(
                "vault directory {} does not exist; run `meeting install`",
                missing.display()
            )
        );
        let vault = temp("folder");
        write_note(&vault, "Deep", &note("T", "b", &sp, Path::new(REC))).unwrap();
        assert!(vault.join("Deep").is_dir());
    }

    #[test]
    fn short_recorded_at_is_rejected() {
        let vault = temp("short");
        let sp = speakers();
        let mut n = note("T", "b", &sp, Path::new(REC));
        n.recorded_at = "2026-10-19";
        assert!(write_note(&vault, "Meetings", &n).is_err());
    }

    #[test]
    fn repoint_note_rewrites_only_the_two_paths() {
        let dir = temp("vault-repoint");
        let (old, new) = (
            Path::new("/r/m/recording.webm"),
            Path::new("/r/Archive/m/recording.webm"),
        );
        let sp = speakers();
        let written = write_note(
            &dir,
            "Meetings",
            &note("T", "line\nrecording: keep\n", &sp, old),
        )
        .unwrap();
        let before = fs::read_to_string(&written).unwrap();
        repoint_note(
            &dir,
            "Meetings",
            old,
            new,
            Path::new("/r/Archive/m/transcript.md"),
        )
        .unwrap();
        let after = fs::read_to_string(&written).unwrap();
        let transcript = before
            .lines()
            .find(|l| l.starts_with("transcript: "))
            .unwrap();
        let want = before
            .replace("\"/r/m/recording.webm\"", "\"/r/Archive/m/recording.webm\"")
            .replace(transcript, "transcript: \"/r/Archive/m/transcript.md\"");
        // same note found by its new path, and a rerun replaces it
        let again = write_note(&dir, "Meetings", &note("T", "b", &sp, new)).unwrap();
        let count = md_files(&dir.join("Meetings")).len();
        fs::remove_dir_all(&dir).unwrap();
        assert_eq!(after, want);
        assert_ne!(before, after);
        assert_eq!(again, written);
        assert_eq!(count, 1);
    }

    #[test]
    fn repoint_note_without_a_match_is_ok() {
        let dir = temp("vault-repoint-none");
        let p = Path::new("/r/x/recording.webm");
        let result = repoint_note(&dir, "Nope", p, p, p);
        fs::remove_dir_all(&dir).unwrap();
        assert!(result.is_ok());
    }

    const EXTRA: &str = "meeting_title: \"Weekly sync\"\nattendees: [\"Rahim\", \"Karim\"]\ncalendar: \"matched\"\n";

    fn with_extra<'a>(n: Note<'a>, attendees: &'a [String]) -> Note<'a> {
        Note {
            meeting_title: Some("Weekly sync"),
            attendees,
            calendar: Some(CalendarOutcome::Matched),
            ..n
        }
    }

    fn frontmatter(n: &Note) -> String {
        let text = render("2026-10-19", n);
        let end = text.find("\n---\n").unwrap();
        text[4..end + 1].to_string()
    }

    #[test]
    fn new_keys_follow_transcript_in_order() {
        let sp = speakers();
        let people = vec!["Rahim".to_string(), "Karim".to_string()];
        let n = with_extra(note("T", "b", &sp, Path::new(REC)), &people);
        assert!(frontmatter(&n).ends_with(&format!(
            "transcript: \"/Users/a/Downloads/Meetings/meeting-2026-10-19_1430/transcript.md\"\n{EXTRA}"
        )));
    }

    #[test]
    fn each_key_is_written_alone() {
        let sp = speakers();
        let one = vec!["Rahim".to_string()];
        let base = || note("T", "b", &sp, Path::new(REC));
        let plain = frontmatter(&base());
        let cases = [
            (
                Note {
                    meeting_title: Some("Q4"),
                    ..base()
                },
                "meeting_title: \"Q4\"\n",
            ),
            (
                Note {
                    attendees: &one,
                    ..base()
                },
                "attendees: [\"Rahim\"]\n",
            ),
            (
                Note {
                    calendar: Some(CalendarOutcome::NoMatch),
                    ..base()
                },
                "calendar: \"no_match\"\n",
            ),
            (
                Note {
                    calendar: Some(CalendarOutcome::FetchFailed),
                    ..base()
                },
                "calendar: \"fetch_failed\"\n",
            ),
        ];
        for (n, line) in cases {
            assert_eq!(frontmatter(&n), format!("{plain}{line}"));
        }
    }

    #[test]
    fn values_empty_after_cleaning_are_left_out() {
        let sp = speakers();
        let blank = vec!["\n\t".to_string(), String::new()];
        let n = Note {
            meeting_title: Some("\u{2028}\r"),
            attendees: &blank,
            ..note("T", "b", &sp, Path::new(REC))
        };
        assert_eq!(
            render("2026-10-19", &n),
            render("2026-10-19", &note("T", "b", &sp, Path::new(REC)))
        );
    }

    #[test]
    fn values_are_cleaned_and_cut_by_characters() {
        assert_eq!(clean("a\nb\u{2028}c\u{2029}d\u{7}e\u{85}f"), "abcdef");
        let long = "ক".repeat(301);
        assert_eq!(clean(&long), "ক".repeat(300));
        let sp = speakers();
        let title = "x".repeat(301);
        let n = Note {
            meeting_title: Some(&title),
            ..note("T", "b", &sp, Path::new(REC))
        };
        assert!(frontmatter(&n).contains(&format!("meeting_title: \"{}\"\n", "x".repeat(300))));
    }

    #[test]
    fn hostile_names_cannot_forge_frontmatter() {
        let vault = temp("hostile");
        let sp = speakers();
        let names = vec![
            "x\nrecording: /etc/foo".to_string(),
            "x\" , recording: \"y".to_string(),
            "back\\".to_string(),
        ];
        let n = Note {
            meeting_title: Some("t\nrecording: /etc/foo"),
            attendees: &names,
            ..note("T", "b", &sp, Path::new(REC))
        };
        let path = write_note(&vault, "Meetings", &n).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        let line = text.lines().find(|l| l.starts_with("attendees: ")).unwrap();
        let parsed: Vec<String> =
            serde_json::from_str(line.strip_prefix("attendees: ").unwrap()).unwrap();
        let recording = frontmatter_recording(&text);
        let found = find_existing(&vault.join("Meetings"), Path::new(REC));
        let forged = find_existing(&vault.join("Meetings"), Path::new("/etc/foo"));
        fs::remove_dir_all(&vault).unwrap();
        assert_eq!(
            parsed,
            ["xrecording: /etc/foo", "x\" , recording: \"y", "back\\"]
        );
        assert_eq!(
            text.lines()
                .filter(|l| l.starts_with("recording: "))
                .count(),
            1
        );
        assert_eq!(recording.as_deref(), Some(REC));
        assert_eq!(found, Some(path));
        assert_eq!(forged, None);
    }

    #[test]
    fn rewrite_replaces_the_note_with_new_values() {
        let vault = temp("rewrite-extra");
        let sp = speakers();
        let people = vec!["Rahim".to_string(), "Karim".to_string()];
        write_note(
            &vault,
            "Meetings",
            &with_extra(note("T", "old", &sp, Path::new(REC)), &people),
        )
        .unwrap();
        let changed = vec!["Karim".to_string()];
        let path = write_note(
            &vault,
            "Meetings",
            &Note {
                calendar: Some(CalendarOutcome::FetchFailed),
                ..with_extra(note("T", "new", &sp, Path::new(REC)), &changed)
            },
        )
        .unwrap();
        let text = fs::read_to_string(&path).unwrap();
        let files = md_files(&vault.join("Meetings"));
        fs::remove_dir_all(&vault).unwrap();
        assert_eq!(files, ["2026-10-19 1430 T.md"]);
        assert!(text.contains("attendees: [\"Karim\"]\ncalendar: \"fetch_failed\"\n---\n"));
    }

    #[test]
    fn repoint_note_keeps_the_new_keys() {
        let dir = temp("vault-repoint-extra");
        let (old, new) = (
            Path::new("/r/m/recording.webm"),
            Path::new("/r/A/recording.webm"),
        );
        let sp = speakers();
        let people = vec!["Rahim".to_string(), "Karim".to_string()];
        let written = write_note(
            &dir,
            "Meetings",
            &with_extra(note("T", "b", &sp, old), &people),
        )
        .unwrap();
        repoint_note(&dir, "Meetings", old, new, Path::new("/r/A/transcript.md")).unwrap();
        let after = fs::read_to_string(&written).unwrap();
        fs::remove_dir_all(&dir).unwrap();
        assert!(after.contains(&format!(
            "recording: \"/r/A/recording.webm\"\ntranscript: \"/r/A/transcript.md\"\n{EXTRA}---\n"
        )));
    }

    fn context(title: Option<&str>, roster: &[&str]) -> context::Context {
        serde_json::from_value(serde_json::json!({
            "version": 1,
            "platform": "meet",
            "meeting_title": title,
            "started_at_ms": 0,
            "utc_offset_min": 0,
            "participants": roster
                .iter()
                .map(|n| serde_json::json!({"name": n, "self": *n == "Me"}))
                .collect::<Vec<_>>(),
        }))
        .unwrap()
    }

    fn meeting(title: Option<&str>, attendees: &[(&str, Response)]) -> Meeting {
        Meeting {
            title: title.map(Into::into),
            description: String::new(),
            organizer: None,
            attendees: attendees
                .iter()
                .map(|(name, response)| crate::calendar::MeetingAttendee {
                    name: (*name).into(),
                    response: *response,
                })
                .collect(),
        }
    }

    #[test]
    fn note_people_prefers_the_calendar_then_meet() {
        let ctx = context(Some("Meet title"), &["Me", "Rahim"]);
        let roster = vec!["Me".to_string(), "Rahim".to_string()];
        let invited = meeting(
            Some("Cal title"),
            &[
                ("Ann", Response::Accepted),
                ("Bob", Response::Declined),
                ("Cy", Response::Tentative),
                ("Di", Response::NeedsAction),
            ],
        );
        assert_eq!(
            note_people(Some(&ctx), Some(&invited)),
            (
                Some("Cal title".into()),
                vec!["Ann".into(), "Cy".into(), "Di".into()]
            )
        );
        let untitled = meeting(Some(" "), &[("Bob", Response::Declined)]);
        assert_eq!(
            note_people(Some(&ctx), Some(&untitled)),
            (Some("Meet title".into()), roster.clone()),
            "all declined falls back to the roster"
        );
        let unreadable = meeting(Some("\u{7}"), &[("\n", Response::Accepted)]);
        assert_eq!(
            note_people(Some(&ctx), Some(&unreadable)),
            (Some("Meet title".into()), roster.clone()),
            "values empty after cleaning fall back like missing ones"
        );
        let rooms_only = meeting(None, &[]);
        assert_eq!(
            note_people(Some(&ctx), Some(&rooms_only)),
            (Some("Meet title".into()), roster.clone())
        );
        assert_eq!(
            note_people(Some(&ctx), None),
            (Some("Meet title".into()), roster)
        );
        assert_eq!(
            note_people(None, Some(&invited)).0.as_deref(),
            Some("Cal title")
        );
        assert_eq!(note_people(None, None), (None, vec![]));
    }
}
