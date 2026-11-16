//! `meeting import`: validate a source file, name a new meeting folder and copy it in.
//! Every check runs before anything is written; the original is only ever read.

use std::ffi::OsString;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, bail};

use crate::config::{self, RECORDING_EXTENSIONS};
use crate::jobs::unix_secs;

const DATE: &str = "/bin/date";

/// Copies `source` to `<recordings_dir>/meeting-<stamp>[-n]/recording.<ext>` and returns that
/// path. `date` is a local `YYYY-MM-DD HH:MM`; without it the source's modified time names the
/// folder. `local_minute(unix_secs)` returns `YYYY-MM-DD_HHMM` in local time.
pub fn import(
    recordings_dir: &Path,
    source: &Path,
    date: Option<&str>,
    local_minute: fn(u64) -> anyhow::Result<String>,
) -> anyhow::Result<PathBuf> {
    let source =
        fs::canonicalize(source).with_context(|| format!("cannot read {}", source.display()))?;
    if !source.is_file() {
        bail!("{} is not a regular file", source.display());
    }
    let ext = source
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .filter(|e| RECORDING_EXTENSIONS.contains(&e.as_str()))
        .with_context(|| {
            format!(
                "{} is not a recording; accepted: {}",
                source.display(),
                RECORDING_EXTENSIONS.join(", ")
            )
        })?;
    if config::check_recording(recordings_dir, &source).is_ok() {
        bail!(
            "{p} is already a meeting recording; run meeting process {p}",
            p = source.display()
        );
    }
    let stamp = match date {
        Some(d) => parse_date(d)?,
        None => local_minute(unix_secs(fs::metadata(&source)?.modified()?))?,
    };

    fs::create_dir_all(recordings_dir)
        .with_context(|| format!("cannot create {}", recordings_dir.display()))?;
    let folder = create_folder(recordings_dir, &stamp)?;
    let dest = folder.join(format!("recording.{ext}"));
    let mut part = OsString::from(dest.as_os_str());
    part.push(".part");
    let copied = fs::copy(&source, &part).and_then(|_| fs::rename(&part, &dest));
    if let Err(e) = copied {
        let cause = format!(
            "cannot copy {} to {}: {e}",
            source.display(),
            dest.display()
        );
        return match fs::remove_dir_all(&folder) {
            Ok(()) => Err(anyhow::anyhow!(cause)),
            Err(r) => Err(anyhow::anyhow!(
                "{cause}; also could not remove {}: {r}; delete that folder",
                folder.display()
            )),
        };
    }
    let dest = fs::canonicalize(&dest)?;
    tracing::info!(source = %source.display(), dest = %dest.display(), "imported recording");
    Ok(dest)
}

/// Creates `meeting-<stamp>`, then `-2`, `-3`… on the first name `create_dir` accepts, so two
/// imports in one minute never share a folder.
pub fn create_folder(recordings_dir: &Path, stamp: &str) -> anyhow::Result<PathBuf> {
    for n in 1.. {
        let name = if n == 1 {
            format!("meeting-{stamp}")
        } else {
            format!("meeting-{stamp}-{n}")
        };
        let folder = recordings_dir.join(name);
        match fs::create_dir(&folder) {
            Ok(()) => return Ok(folder),
            Err(e) if e.kind() == ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e).with_context(|| format!("cannot create {}", folder.display())),
        }
    }
    unreachable!()
}

/// `YYYY-MM-DD HH:MM` (local time, a real calendar date) to `YYYY-MM-DD_HHMM`.
fn parse_date(date: &str) -> anyhow::Result<String> {
    let bad = || anyhow::anyhow!("--date {date:?} is not a valid local \"YYYY-MM-DD HH:MM\"");
    let b = date.as_bytes();
    let shaped = b.len() == 16
        && b.iter().enumerate().all(|(i, c)| match i {
            4 | 7 => *c == b'-',
            10 => *c == b' ',
            13 => *c == b':',
            _ => c.is_ascii_digit(),
        });
    if !shaped {
        return Err(bad());
    }
    let num = |r: std::ops::Range<usize>| date[r].parse::<u32>().unwrap();
    let (year, month, day, hour, minute) =
        (num(0..4), num(5..7), num(8..10), num(11..13), num(14..16));
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let days = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return Err(bad()),
    };
    if day < 1 || day > days || hour > 23 || minute > 59 {
        return Err(bad());
    }
    Ok(format!(
        "{}_{}{}",
        &date[..10],
        &date[11..13],
        &date[14..16]
    ))
}

/// The real `local_minute`: macOS `/bin/date`, which knows the local time zone.
pub fn local_minute(secs: u64) -> anyhow::Result<String> {
    let out = Command::new(DATE)
        .args(["-r", &secs.to_string(), "+%Y-%m-%d_%H%M"])
        .output()
        .with_context(|| format!("cannot run {DATE}"))?;
    if !out.status.success() {
        bail!("{DATE} failed with {}", out.status);
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::temp;
    use std::fs::File;
    use std::os::unix::fs::PermissionsExt;
    use std::time::{Duration, UNIX_EPOCH};

    fn stub(secs: u64) -> anyhow::Result<String> {
        Ok(format!("stub-{secs}"))
    }

    fn source(dir: &Path, name: &str) -> PathBuf {
        let p = dir.join(name);
        fs::write(&p, "media").unwrap();
        p
    }

    fn entries(dir: &Path) -> Vec<String> {
        let mut v: Vec<_> = fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        v.sort();
        v
    }

    #[test]
    fn accepts_listed_extensions_in_any_case() {
        let dir = temp("import-ext");
        let rec = dir.join("rec");
        let got = import(
            &rec,
            &source(&dir, "call.M4A"),
            Some("2026-10-01 14:00"),
            stub,
        )
        .unwrap();
        assert_eq!(
            got,
            fs::canonicalize(&rec)
                .unwrap()
                .join("meeting-2026-10-01_1400/recording.m4a")
        );
        let err = import(&rec, &source(&dir, "notes.txt"), None, stub).unwrap_err();
        fs::remove_dir_all(&dir).unwrap();
        assert!(
            err.to_string().contains("accepted: webm, mp4, m4a"),
            "{err}"
        );
    }

    #[test]
    fn refuses_before_writing() {
        let dir = temp("import-refuse");
        let rec = dir.join("rec");
        fs::create_dir_all(rec.join("m")).unwrap();
        let existing = source(&rec.join("m"), "recording.webm");
        let good = source(&dir, "ok.webm");
        let mut errs = vec![
            import(&rec, &dir.join("missing.webm"), None, stub).unwrap_err(),
            import(&rec, &dir, None, stub).unwrap_err(),
            import(&rec, &existing, None, stub).unwrap_err(),
        ];
        assert!(errs[2].to_string().contains("meeting process"));
        for d in [
            "2026-10-01",
            "2026-13-01 10:00",
            "2026-02-30 10:00",
            "2026-10-01 24:00",
            "2026-10-01 10:60",
            "2026-10-01T10:00",
        ] {
            errs.push(import(&rec, &good, Some(d), stub).unwrap_err());
        }
        let left = entries(&rec);
        fs::remove_dir_all(&dir).unwrap();
        assert_eq!(errs.len(), 9);
        assert_eq!(left, ["m"]);
    }

    #[test]
    fn imports_a_flat_file_inside_the_recordings_dir() {
        let dir = temp("import-flat");
        let rec = dir.join("rec");
        fs::create_dir_all(&rec).unwrap();
        let got = import(&rec, &source(&rec, "old.webm"), None, stub).unwrap();
        let left = entries(&rec);
        fs::remove_dir_all(&dir).unwrap();
        assert!(got.ends_with("recording.webm"));
        assert_eq!(left.len(), 2);
    }

    #[test]
    fn names_the_folder_from_date_or_modified_time() {
        let dir = temp("import-name");
        let rec = dir.join("rec");
        let src = source(&dir, "a.webm");
        File::options()
            .write(true)
            .open(&src)
            .unwrap()
            .set_modified(UNIX_EPOCH + Duration::from_secs(1234))
            .unwrap();
        let dated = import(&rec, &src, Some("2026-10-01 14:00"), stub).unwrap();
        let stamped = import(&rec, &src, None, stub).unwrap();
        fs::remove_dir_all(&dir).unwrap();
        assert!(
            dated
                .to_string_lossy()
                .contains("/meeting-2026-10-01_1400/")
        );
        assert!(stamped.to_string_lossy().contains("/meeting-stub-1234/"));
    }

    #[test]
    fn adds_a_suffix_when_the_name_is_taken() {
        let dir = temp("import-suffix");
        let rec = dir.join("rec");
        let src = source(&dir, "a.webm");
        import(&rec, &src, Some("2026-10-01 14:00"), stub).unwrap();
        let second = import(&rec, &src, Some("2026-10-01 14:00"), stub).unwrap();
        fs::remove_dir_all(&dir).unwrap();
        assert!(
            second
                .to_string_lossy()
                .contains("/meeting-2026-10-01_1400-2/")
        );
    }

    #[test]
    fn copy_leaves_the_source_unchanged() {
        let dir = temp("import-same");
        let src = source(&dir, "a.webm");
        let before = fs::metadata(&src).unwrap().modified().unwrap();
        let got = import(&dir.join("rec"), &src, None, stub).unwrap();
        let same = fs::read(&got).unwrap() == b"media" && fs::read(&src).unwrap() == b"media";
        let after = fs::metadata(&src).unwrap().modified().unwrap();
        fs::remove_dir_all(&dir).unwrap();
        assert!(same);
        assert_eq!(before, after);
    }

    #[test]
    fn a_failed_copy_removes_the_folder() {
        let dir = temp("import-fail");
        let rec = dir.join("rec");
        let src = source(&dir, "a.webm");
        fs::set_permissions(&src, fs::Permissions::from_mode(0o000)).unwrap();
        let err = import(&rec, &src, None, stub).unwrap_err();
        let left = entries(&rec);
        fs::set_permissions(&src, fs::Permissions::from_mode(0o644)).unwrap();
        fs::remove_dir_all(&dir).unwrap();
        assert!(err.to_string().contains("cannot copy"), "{err}");
        assert!(left.is_empty(), "{left:?}");
    }

    // BSD `date -r`: macOS only; the Linux hook image skips it, macOS CI runs it.
    #[cfg(target_os = "macos")]
    #[test]
    fn local_minute_runs_bin_date() {
        let got = local_minute(1_700_000_000).unwrap();
        assert_eq!(got.len(), 15, "{got}");
        assert_eq!(&got[10..11], "_");
    }
}
