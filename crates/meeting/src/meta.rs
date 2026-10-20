use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Context;
use serde::{Deserialize, Serialize};

use crate::Language;
use crate::jobs::{Stage, format_utc, unix_secs, write_atomic};

/// `meta.json` in a recording's folder: stored language, speakers and run history.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Meta {
    pub recording: PathBuf,
    pub language: Language,
    pub recorded_at: String,
    pub duration_s: Option<f64>,
    pub title: Option<String>,
    pub speakers: Vec<Speaker>,
    pub runs: Vec<Run>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Speaker {
    pub label: String,
    pub name: Option<String>,
    pub evidence: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Run {
    pub job_id: String,
    pub started_at: String,
    pub ended_at: Option<String>,
    pub stage: Stage,
    pub error: Option<String>,
}

impl Meta {
    pub fn path_for(recording: &Path) -> PathBuf {
        recording.with_file_name("meta.json")
    }

    pub fn load(path: &Path) -> anyhow::Result<Option<Meta>> {
        let bytes = match fs::read(path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            r => r.with_context(|| format!("cannot read {}", path.display()))?,
        };
        let meta = serde_json::from_slice(&bytes)
            .with_context(|| format!("invalid meta {}", path.display()))?;
        Ok(Some(meta))
    }

    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        write_atomic(path, &serde_json::to_vec_pretty(self)?)
    }

    /// Appends the run, or replaces the entry with the same `job_id`.
    pub fn upsert_run(&mut self, run: Run) {
        match self.runs.iter_mut().find(|r| r.job_id == run.job_id) {
            Some(existing) => *existing = run,
            None => self.runs.push(run),
        }
    }
}

/// `YYYY-MM-DDTHH:MM` from a `meeting-YYYY-MM-DD_HHMM[...]` folder name; any other folder
/// name uses the file's modified time in UTC.
pub fn recorded_at_for(recording: &Path) -> anyhow::Result<String> {
    let folder = (recording.parent())
        .and_then(Path::file_name)
        .and_then(|s| s.to_str())
        .unwrap_or("");
    if let Some(at) = folder.strip_prefix("meeting-").and_then(from_name) {
        return Ok(at);
    }
    let modified = fs::metadata(recording)
        .and_then(|m| m.modified())
        .with_context(|| format!("cannot read the modified time of {}", recording.display()))?;
    Ok(format_utc(unix_secs(modified)))
}

fn from_name(rest: &str) -> Option<String> {
    let b = rest.as_bytes().get(..15)?;
    let digits = |r: std::ops::Range<usize>| b[r].iter().all(u8::is_ascii_digit);
    let shaped = digits(0..4) && b[4] == b'-' && digits(5..7) && b[7] == b'-' && digits(8..10);
    if !(shaped && b[10] == b'_' && digits(11..15)) {
        return None;
    }
    Some(format!(
        "{}T{}:{}",
        &rest[..10],
        &rest[11..13],
        &rest[13..15]
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("meeting-test-{}-{name}", std::process::id()))
    }

    fn run(job_id: &str, stage: Stage) -> Run {
        Run {
            job_id: job_id.into(),
            started_at: "2026-10-19T14:30:05Z".into(),
            ended_at: None,
            stage,
            error: None,
        }
    }

    fn sample() -> Meta {
        Meta {
            recording: "/r/meeting-2026-10-19_1430/recording.webm".into(),
            language: Language::Bn,
            recorded_at: "2026-10-19T14:30".into(),
            duration_s: Some(61.5),
            title: None,
            speakers: vec![Speaker {
                label: "A".into(),
                name: Some("Rahim".into()),
                evidence: "introduced himself".into(),
            }],
            runs: vec![],
        }
    }

    #[test]
    fn path_sits_in_the_recording_folder() {
        assert_eq!(
            Meta::path_for(Path::new("/r/meeting-2026-10-19_1430/recording.webm")),
            PathBuf::from("/r/meeting-2026-10-19_1430/meta.json")
        );
    }

    #[test]
    fn save_then_load_round_trips() {
        let dir = temp("meta-round-trip");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.meta.json");
        sample().save(&path).unwrap();
        let loaded = Meta::load(&path).unwrap();
        fs::remove_dir_all(&dir).unwrap();
        assert_eq!(loaded, Some(sample()));
    }

    #[test]
    fn load_of_missing_file_is_none() {
        assert_eq!(Meta::load(&temp("missing.meta.json")).unwrap(), None);
    }

    #[test]
    fn upsert_run_appends_then_replaces() {
        let mut meta = sample();
        meta.upsert_run(run("a", Stage::Queued));
        meta.upsert_run(run("b", Stage::Queued));
        meta.upsert_run(run("a", Stage::Done));
        assert_eq!(meta.runs, [run("a", Stage::Done), run("b", Stage::Queued)]);
    }

    #[test]
    fn recorded_at_comes_from_the_folder_name() {
        for name in [
            "meeting-2026-10-19_1430",
            "meeting-2026-10-19_1430-2",
            "meeting-2026-10-19_1430-recovered",
        ] {
            let at = recorded_at_for(&Path::new(name).join("recording.webm")).unwrap();
            assert_eq!(at, "2026-10-19T14:30", "{name}");
        }
    }

    #[test]
    fn recorded_at_falls_back_to_modified_time() {
        let dir = temp("recorded-at");
        fs::create_dir_all(&dir).unwrap();
        for name in ["call", "meeting-notes", "meeting-2026-1x-19_1430"] {
            let path = dir.join(name).join("recording.webm");
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, "x").unwrap();
            let at = recorded_at_for(&path).unwrap();
            assert!(at.ends_with('Z') && at.len() == 20, "{name}: {at}");
        }
        fs::remove_dir_all(&dir).unwrap();
    }
}
