use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Context;
use serde::{Deserialize, Serialize};

use crate::config::home;

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Stage {
    Queued,
    Extracting,
    Transcribing,
    Summarising,
    Writing,
    Done,
    Failed,
}

/// One `meeting process` run. Optional fields serialise as `null`: the extension expects the keys.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Job {
    pub job_id: String,
    pub recording: PathBuf,
    pub title: Option<String>,
    pub stage: Stage,
    pub error: Option<String>,
    pub started_at: String,
    pub updated_at: String,
    pub summary_path: Option<PathBuf>,
}

/// A directory of `<job_id>.json` files.
pub struct JobStore {
    pub dir: PathBuf,
}

impl JobStore {
    pub fn default_dir() -> anyhow::Result<PathBuf> {
        Ok(home()?.join("Library/Application Support/meeting-manager/jobs"))
    }

    pub fn write(&self, job: &Job) -> anyhow::Result<()> {
        fs::create_dir_all(&self.dir)
            .with_context(|| format!("cannot create {}", self.dir.display()))?;
        let path = self.dir.join(format!("{}.json", job.job_id));
        write_atomic(&path, &serde_json::to_vec_pretty(job)?)
    }

    pub fn read(&self, job_id: &str) -> anyhow::Result<Option<Job>> {
        let path = self.dir.join(format!("{job_id}.json"));
        let bytes = match fs::read(&path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            r => r.with_context(|| format!("cannot read {}", path.display()))?,
        };
        let job = serde_json::from_slice(&bytes)
            .with_context(|| format!("invalid job {}", path.display()))?;
        Ok(Some(job))
    }

    /// The `limit` most recently started jobs, newest first. Files that aren't job JSON are skipped.
    pub fn list(&self, limit: usize) -> anyhow::Result<Vec<Job>> {
        let entries = match fs::read_dir(&self.dir) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            r => r.with_context(|| format!("cannot read {}", self.dir.display()))?,
        };
        let mut jobs = Vec::new();
        for entry in entries {
            let path = entry?.path();
            if path.extension().is_none_or(|e| e != "json") {
                continue;
            }
            let parsed = fs::read(&path)
                .map_err(anyhow::Error::from)
                .and_then(|b| Ok(serde_json::from_slice::<Job>(&b)?));
            match parsed {
                Ok(job) => jobs.push(job),
                Err(e) => tracing::warn!("skipping {}: {e:#}", path.display()),
            }
        }
        // UTC strings in a fixed format sort correctly as strings.
        jobs.sort_by(|a, b| b.started_at.cmp(&a.started_at));
        jobs.truncate(limit);
        Ok(jobs)
    }
}

/// Writes to `<path>.tmp` then renames, so a reader never sees a half-written file.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    fs::write(&tmp, bytes).with_context(|| format!("cannot write {}", tmp.display()))?;
    fs::rename(&tmp, path).with_context(|| format!("cannot write {}", path.display()))
}

pub fn now_utc() -> String {
    format_utc(unix_secs(SystemTime::now()))
}

pub fn new_job_id() -> String {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis());
    format!("{millis}-{}", std::process::id())
}

pub fn unix_secs(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// `YYYY-MM-DDTHH:MM:SSZ` for seconds since the Unix epoch.
pub fn format_utc(secs: u64) -> String {
    let (days, rem) = (secs / 86_400, secs % 86_400);
    let (y, m, d) = civil_from_days(days);
    let (hh, mm, ss) = (rem / 3600, rem % 3600 / 60, rem % 60);
    format!("{y:04}-{m:02}-{d:02}T{hh:02}:{mm:02}:{ss:02}Z")
}

/// Howard Hinnant's `civil_from_days`: days since 1970-01-01 to (year, month, day).
fn civil_from_days(days: u64) -> (u64, u64, u64) {
    let z = days + 719_468;
    let era = z / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + u64::from(m <= 2);
    (y, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("meeting-test-{}-{name}", std::process::id()))
    }

    fn job(id: &str, started_at: &str) -> Job {
        Job {
            job_id: id.into(),
            recording: "/r/meeting-2026-10-19_1430.webm".into(),
            title: None,
            stage: Stage::Queued,
            error: None,
            started_at: started_at.into(),
            updated_at: started_at.into(),
            summary_path: None,
        }
    }

    #[test]
    fn stage_serialises_lowercase() {
        let stages = [
            (Stage::Queued, "queued"),
            (Stage::Extracting, "extracting"),
            (Stage::Transcribing, "transcribing"),
            (Stage::Summarising, "summarising"),
            (Stage::Writing, "writing"),
            (Stage::Done, "done"),
            (Stage::Failed, "failed"),
        ];
        for (stage, text) in stages {
            assert_eq!(
                serde_json::to_string(&stage).unwrap(),
                format!("\"{text}\"")
            );
        }
    }

    #[test]
    fn write_then_read_round_trips_with_exact_keys() {
        let store = JobStore {
            dir: temp("jobs-round-trip"),
        };
        let j = job("1-2", "2026-10-19T14:30:05Z");
        store.write(&j).unwrap();
        let read = store.read("1-2").unwrap();
        let raw: serde_json::Value =
            serde_json::from_slice(&fs::read(store.dir.join("1-2.json")).unwrap()).unwrap();
        fs::remove_dir_all(&store.dir).unwrap();
        assert_eq!(read, Some(j));
        let mut keys: Vec<_> = raw.as_object().unwrap().keys().cloned().collect();
        keys.sort();
        assert_eq!(
            keys,
            [
                "error",
                "job_id",
                "recording",
                "stage",
                "started_at",
                "summary_path",
                "title",
                "updated_at"
            ]
        );
        assert!(raw["title"].is_null() && raw["error"].is_null() && raw["summary_path"].is_null());
    }

    #[test]
    fn default_dir_is_under_application_support() {
        let dir = JobStore::default_dir().unwrap();
        assert!(dir.ends_with("Library/Application Support/meeting-manager/jobs"));
    }

    #[test]
    fn read_unknown_id_is_none() {
        let store = JobStore {
            dir: temp("jobs-unknown"),
        };
        assert_eq!(store.read("nope").unwrap(), None);
    }

    #[test]
    fn list_returns_newest_limited_and_skips_garbage() {
        let store = JobStore {
            dir: temp("jobs-list"),
        };
        for i in 0..12 {
            store
                .write(&job(&format!("j{i}"), &format!("2026-10-19T14:30:{i:02}Z")))
                .unwrap();
        }
        fs::write(store.dir.join("garbage.json"), "not json").unwrap();
        fs::write(store.dir.join("j0.json.tmp"), "{").unwrap();
        let listed = store.list(10).unwrap();
        fs::remove_dir_all(&store.dir).unwrap();
        let ids: Vec<_> = listed.iter().map(|j| j.job_id.as_str()).collect();
        assert_eq!(
            ids,
            ["j11", "j10", "j9", "j8", "j7", "j6", "j5", "j4", "j3", "j2"]
        );
    }

    #[test]
    fn list_of_missing_dir_is_empty() {
        let store = JobStore {
            dir: temp("jobs-no-such-dir"),
        };
        assert!(store.list(10).unwrap().is_empty());
    }

    #[test]
    fn format_utc_on_fixed_instants() {
        assert_eq!(format_utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(format_utc(1_709_164_800), "2024-02-29T00:00:00Z");
        assert_eq!(format_utc(1_792_420_205), "2026-10-19T14:30:05Z");
    }

    #[test]
    fn now_utc_has_the_fixed_shape() {
        let s = now_utc();
        assert_eq!(s.len(), 20);
        assert!(s.ends_with('Z') && s.as_bytes()[10] == b'T');
    }

    #[test]
    fn job_ids_are_distinct() {
        let a = new_job_id();
        std::thread::sleep(std::time::Duration::from_millis(2));
        assert_ne!(a, new_job_id());
    }
}
