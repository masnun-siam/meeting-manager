//! The meeting folder a recording starts in: `meeting-YYYY-MM-DD_HHMM`, then `-2`, `-3`… on a
//! clash (recording-folders FR-1), named from the local start minute.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Context;

/// `YYYY-MM-DD_HHMM` for a unix time, in the local time zone.
pub type LocalMinute = fn(u64) -> anyhow::Result<String>;

pub fn new_folder(
    recordings_dir: &Path,
    local_minute: LocalMinute,
    now_secs: u64,
) -> anyhow::Result<PathBuf> {
    fs::create_dir_all(recordings_dir)
        .with_context(|| format!("cannot create {}", recordings_dir.display()))?;
    meeting::import::create_folder(recordings_dir, &local_minute(now_secs)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recorder::wav::tests::temp;

    fn stub(secs: u64) -> anyhow::Result<String> {
        Ok(format!("stub-{secs}"))
    }

    fn broken(_: u64) -> anyhow::Result<String> {
        anyhow::bail!("/bin/date failed")
    }

    #[test]
    fn adds_a_suffix_when_the_name_is_taken() {
        let dir = temp("folder-suffix");
        let recordings = dir.join("Recordings");
        let names: Vec<_> = (0..3)
            .map(|_| new_folder(&recordings, stub, 7).unwrap())
            .collect();
        assert_eq!(
            names,
            [
                recordings.join("meeting-stub-7"),
                recordings.join("meeting-stub-7-2"),
                recordings.join("meeting-stub-7-3"),
            ]
        );
        assert!(names.iter().all(|n| n.is_dir()));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_failed_clock_creates_no_folder() {
        let dir = temp("folder-clock");
        let recordings = dir.join("Recordings");
        assert!(new_folder(&recordings, broken, 7).is_err());
        assert_eq!(fs::read_dir(&recordings).unwrap().count(), 0);
        fs::remove_dir_all(dir).unwrap();
    }
}
