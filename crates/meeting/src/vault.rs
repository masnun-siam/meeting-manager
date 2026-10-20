//! Writes the meeting note into the Obsidian vault, one note per recording.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, bail};

use crate::Language;

pub struct Note<'a> {
    /// `YYYY-MM-DDTHH:MM`, optionally followed by seconds and `Z`.
    pub recorded_at: &'a str,
    pub title: &'a str,
    pub language: Language,
    pub speakers: &'a [String],
    pub recording: &'a Path,
    pub transcript: &'a Path,
    pub body: &'a str,
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
    format!(
        "---\ndate: {date}\nlanguage: {language}\nspeakers: [{speakers}]\nrecording: {}\ntranscript: {}\n---\n\n{}",
        quote(&note.recording.to_string_lossy()),
        quote(&note.transcript.to_string_lossy()),
        note.body
    )
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
}
