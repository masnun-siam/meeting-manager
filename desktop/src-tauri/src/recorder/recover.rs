//! Crash recovery on launch and `recorder_retry` (PRD desktop-app FR-7). A meeting folder directly
//! inside `recordings_dir` that holds `recording.partial.wav` and no other `recording.<ext>` is a
//! recording a crash, a failed conversion or a full disk left behind: its WAV is repaired from the
//! file length, converted and handed off as a stopped recording is. Logs carry counts only.

use std::fs;
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::thread::{self, JoinHandle};
use std::time::UNIX_EPOCH;

use meeting::Language;

use super::handoff::{self, WAV};
use super::notice::{Kind, Notice};
use super::{Clock, Recorder, Refused, State, lock, wav};

/// What a conversion killed mid-way leaves; never a recording.
const PART: &str = "recording.m4a.part";
const ARCHIVE: &str = "Archive";

impl Recorder {
    /// Recovers every leftover recording, oldest first, then sets one notice (none when nothing
    /// was found). Does not change the recorder's state: the user may start a recording meanwhile,
    /// and then that recording's notice is kept.
    pub(crate) fn recover_all(&self) {
        let config = match self.inner.env.config() {
            Ok(config) => config,
            Err(_) => {
                tracing::warn!("recorder: recovery skipped: the config cannot be read");
                return;
            }
        };
        let folders = candidates(&config.recordings_dir);
        if folders.is_empty() {
            return;
        }
        let starts = self.inner.next_id.load(Ordering::Relaxed);
        let language = self.saved_language();
        let (mut recovered, mut failed) = (0, Vec::new());
        for folder in &folders {
            match self.recover_one(folder, language, &config.ffmpeg_path) {
                None => {}
                Some(notice) if notice.folder.is_some() => failed.push(notice),
                Some(_) => recovered += 1,
            }
        }
        tracing::info!(
            "recorder: recovery: {recovered} recovered, {} failed",
            failed.len()
        );
        let Some(notice) = summary(recovered, &failed) else {
            return;
        };
        let _state = self.state();
        if self.inner.next_id.load(Ordering::Relaxed) == starts {
            *lock(&self.inner.notice) = Some(notice);
            drop(_state);
            self.changed();
        }
    }

    /// Converts and hands off one leftover. `None` when there is nothing to report: the folder
    /// no longer qualifies, belongs to the running session, or held no audio (then the WAV and,
    /// if empty, the folder are removed). A notice with a `folder` is a failure the user can retry.
    fn recover_one(&self, folder: &Path, language: Language, ffmpeg: &Path) -> Option<Notice> {
        let _one = lock(&self.inner.converting);
        if self.session_folder().as_deref() == Some(folder) || !leftover(folder) {
            return None;
        }
        let wav = folder.join(WAV);
        let frames = match wav::repair(&wav) {
            Ok(0) => {
                let _ = fs::remove_file(&wav);
                let _ = fs::remove_dir(folder);
                return None;
            }
            Ok(frames) => frames,
            Err(e) => {
                tracing::warn!("recorder: a leftover WAV could not be repaired; it is kept");
                return Some(Notice::retry(format!("{e:#}"), folder));
            }
        };
        handoff::ensure_context(folder, &self.started_at(&wav), None);
        let tools = self.tools(ffmpeg.to_path_buf());
        Some(handoff::finish_recording(
            &tools, folder, language, frames, "",
        ))
    }

    /// The folder of the recording that is running or saving: never touched by recovery.
    fn session_folder(&self) -> Option<PathBuf> {
        match &*self.state() {
            State::Active(s) => Some(s.folder.clone()),
            State::Saving { folder, .. } => folder.clone(),
            State::Idle | State::Starting => None,
        }
    }

    /// The clock with the WAV's creation time (else its modified time) as the start.
    fn started_at(&self, wav: &Path) -> Clock {
        let meta = fs::metadata(wav).ok();
        let at = meta
            .as_ref()
            .and_then(|m| m.created().or_else(|_| m.modified()).ok())
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .and_then(|d| i64::try_from(d.as_millis()).ok());
        let now = (self.inner.deps.clock)();
        Clock {
            now_ms: at.unwrap_or(now.now_ms),
            ..now
        }
    }

    /// Retries a failed conversion (FR-7, D33). `folder` comes from a notice and is re-validated:
    /// only a direct subfolder of `recordings_dir` with a leftover WAV is accepted. Answers at
    /// once; the returned thread converts, hands off, sets the notice and returns to `idle`.
    pub fn retry(&self, folder: &str) -> Result<JoinHandle<()>, Refused> {
        let config = self
            .inner
            .env
            .config()
            .map_err(|message| Refused::new("not_found", message))?;
        let folder = retry_folder(&config.recordings_dir, Path::new(folder))
            .ok_or_else(|| Refused::new("not_found", "That recording is not there to retry"))?;
        let saving = self.begin_saving().ok_or_else(|| {
            Refused::new(
                "already_recording",
                "A recording is already running; stop it first",
            )
        })?;
        let language = self.saved_language();
        let recorder = self.clone();
        Ok(thread::spawn(move || {
            let outcome = panic::catch_unwind(AssertUnwindSafe(|| {
                recorder.recover_one(&folder, language, &config.ffmpeg_path)
            }))
            .unwrap_or_else(|_| Some(Notice::retry("Saving failed unexpectedly; retry", &folder)));
            if let Some(notice) = outcome {
                *lock(&recorder.inner.notice) = Some(notice);
            }
            // Back to `idle`, which announces the change.
            drop(saving);
        }))
    }
}

/// Direct subfolders of `dir` holding the WAV, oldest first (the name is the start time).
fn candidates(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut folders: Vec<PathBuf> = entries
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()) && e.file_name() != ARCHIVE)
        .map(|e| e.path())
        .filter(|folder| folder.join(WAV).is_file())
        .collect();
    folders.sort();
    folders
}

/// Whether `folder` holds the WAV and no other `recording.*`, after removing a stale `.part`.
fn leftover(folder: &Path) -> bool {
    let _ = fs::remove_file(folder.join(PART));
    let Ok(entries) = fs::read_dir(folder) else {
        return false;
    };
    let mut wav = false;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name == WAV {
            wav = true;
        } else if name.starts_with("recording.") {
            return false;
        }
    }
    wav
}

/// `recordings_dir/<name>` when `folder` is a direct subfolder of `recordings_dir` (not
/// `Archive`) holding the WAV. Canonicalising resolves `..` and symlinks before the check; the
/// answer is in the configured form, as the recorder's own notices name folders.
fn retry_folder(recordings_dir: &Path, folder: &Path) -> Option<PathBuf> {
    let dir = recordings_dir.canonicalize().ok()?;
    let canonical = folder.canonicalize().ok()?;
    let name = canonical.file_name().filter(|n| *n != ARCHIVE)?;
    let direct = canonical.parent() == Some(dir.as_path());
    (direct && canonical.join(WAV).is_file()).then(|| recordings_dir.join(name))
}

/// The one notice for a recovery run.
fn summary(recovered: usize, failed: &[Notice]) -> Option<Notice> {
    let plural = if recovered == 1 { "" } else { "s" };
    let done = format!("Recovered {recovered} recording{plural}");
    match failed.first() {
        None if recovered == 0 => None,
        None => Some(Notice::new(Kind::Recovered, done)),
        Some(first) => Some(Notice {
            kind: Kind::Error,
            message: format!(
                "{done}. Could not convert {}: {}",
                failed.len(),
                first.message
            ),
            folder: first.folder.clone(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::{PermissionsExt, symlink};
    use std::sync::Arc;
    use std::sync::atomic::AtomicU64;
    use std::time::Duration;

    use super::*;
    use crate::recorder::Reason;
    use crate::recorder::convert::tests::{ffmpeg, probe, tone_wav};
    use crate::recorder::handoff::tests::spawns_under;
    use crate::recorder::handoff::{CONTEXT, M4A};
    use crate::recorder::tests::{Rig, SECOND, ok_capture, push, rig, write_config};
    use crate::recorder::wav::{Chunk, WavWriter};

    fn recordings(rig: &Rig) -> PathBuf {
        rig.home.join("Recordings")
    }

    /// A meeting folder with `secs` of audio in a finished WAV.
    fn leftover_in(rig: &Rig, name: &str, secs: usize) -> PathBuf {
        let folder = recordings(rig).join(name);
        fs::create_dir_all(&folder).unwrap();
        tone_wav(&folder.join(WAV), secs);
        folder
    }

    /// What `kill -9` leaves: the writer flushed the audio but never patched the final sizes,
    /// and the last sample is cut in half.
    fn killed_wav(path: &Path, secs: usize) {
        let writer = WavWriter::create(
            path,
            Duration::from_secs(3600),
            1024,
            Arc::new(AtomicU64::new(0)),
            Box::new(|_| {}),
        )
        .unwrap();
        for _ in 0..secs * 10 {
            writer
                .sender()
                .send(Chunk::Samples(vec![2_000; 4_800]))
                .unwrap();
        }
        writer.finish().unwrap();
        let mut bytes = fs::read(path).unwrap();
        bytes[4..8].copy_from_slice(&36u32.to_le_bytes());
        bytes[40..44].copy_from_slice(&0u32.to_le_bytes());
        bytes.push(7);
        fs::write(path, bytes).unwrap();
    }

    fn broken_ffmpeg(rig: &Rig) -> PathBuf {
        let broken = rig.home.join("ffmpeg-broken");
        fs::write(&broken, "#!/bin/sh\necho boom >&2\nexit 1\n").unwrap();
        fs::set_permissions(&broken, fs::Permissions::from_mode(0o755)).unwrap();
        broken
    }

    fn duration_s(m4a: &Path) -> f64 {
        let info = probe(m4a);
        let (_, after) = info.split_once("Duration: ").expect("a duration");
        let hms = &after[..11];
        let parts: Vec<f64> = hms.split(':').map(|p| p.parse().unwrap()).collect();
        parts[0] * 3600.0 + parts[1] * 60.0 + parts[2]
    }

    #[test]
    fn a_killed_recording_is_repaired_converted_and_sent_once() {
        let rig = rig("recover-killed", ok_capture());
        let folder = recordings(&rig).join("meeting-2026-11-13_0600");
        fs::create_dir_all(&folder).unwrap();
        killed_wav(&folder.join(WAV), 12);
        // The language last started with, not the default.
        rig.env()
            .store()
            .unwrap()
            .set_setting("language", "en")
            .unwrap();
        rig.recorder.recover_all();
        let duration = duration_s(&folder.join(M4A));
        assert!((duration - 12.0).abs() <= 1.0, "{duration}");
        assert!(!folder.join(WAV).exists());
        assert_eq!(
            spawns_under(&folder),
            [(
                rig.home.join("meeting"),
                folder.join(M4A),
                Some(Language::En)
            )]
        );
        let notice = rig.recorder.status().notice.unwrap();
        assert_eq!(
            notice,
            Notice::new(Kind::Recovered, "Recovered 1 recording")
        );
        // A missing context.json is written, without an event.
        let ctx = meeting::context::load(&folder.join(CONTEXT)).unwrap();
        assert_eq!(ctx.calendar_event, None);
        assert_eq!(rig.state(), "idle");
    }

    #[test]
    fn the_context_written_at_start_is_kept() {
        let rig = rig("recover-context", ok_capture());
        let folder = rig.start().unwrap();
        push(11 * SECOND);
        let before = fs::read(folder.join(CONTEXT)).unwrap();
        // As after a crash: the session is gone, its files stay.
        let saved = rig.recorder.stop(Reason::DiskFull).unwrap();
        saved.join().unwrap();
        rig.recorder.recover_all();
        assert_eq!(fs::read(folder.join(CONTEXT)).unwrap(), before);
        assert!(folder.join(M4A).is_file());
        assert_eq!(spawns_under(&folder).len(), 1);
    }

    #[test]
    fn under_ten_seconds_is_converted_but_not_sent() {
        let rig = rig("recover-short", ok_capture());
        let folder = leftover_in(&rig, "meeting-a", 5);
        rig.recorder.recover_all();
        assert!(folder.join(M4A).is_file());
        assert!(spawns_under(&folder).is_empty());
        assert_eq!(
            rig.recorder.status().notice.unwrap().message,
            "Recovered 1 recording"
        );
    }

    #[test]
    fn only_direct_subfolders_with_only_the_wav_are_touched() {
        let rig = rig("recover-skip", ok_capture());
        let done = leftover_in(&rig, "meeting-done", 2);
        fs::write(done.join(M4A), b"converted").unwrap();
        let archived = recordings(&rig).join(ARCHIVE).join("meeting-old");
        fs::create_dir_all(&archived).unwrap();
        tone_wav(&archived.join(WAV), 2);
        let other = recordings(&rig).join("notes");
        fs::create_dir_all(&other).unwrap();
        fs::write(other.join("a.txt"), b"x").unwrap();

        rig.recorder.recover_all();
        assert_eq!(fs::read(done.join(M4A)).unwrap(), b"converted");
        assert!(done.join(WAV).is_file());
        assert!(archived.join(WAV).is_file());
        assert!(!archived.join(M4A).exists());
        assert!(other.join("a.txt").is_file());
        assert_eq!(rig.recorder.status().notice, None);
    }

    #[test]
    fn a_stale_part_file_is_removed_and_does_not_count() {
        let rig = rig("recover-part", ok_capture());
        let folder = leftover_in(&rig, "meeting-a", 2);
        fs::write(folder.join(PART), b"half").unwrap();
        rig.recorder.recover_all();
        assert!(!folder.join(PART).exists());
        assert!(folder.join(M4A).is_file());
    }

    #[test]
    fn folders_are_recovered_oldest_first() {
        let rig = rig("recover-order", ok_capture());
        for name in [
            "meeting-2026-11-13_0900",
            "meeting-2026-11-12_1000",
            "meeting-2026-11-13_0800",
        ] {
            leftover_in(&rig, name, 1);
        }
        let found: Vec<_> = candidates(&recordings(&rig))
            .iter()
            .map(|f| f.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            found,
            [
                "meeting-2026-11-12_1000",
                "meeting-2026-11-13_0800",
                "meeting-2026-11-13_0900"
            ]
        );
        rig.recorder.recover_all();
        assert_eq!(
            rig.recorder.status().notice.unwrap().message,
            "Recovered 3 recordings"
        );
    }

    #[test]
    fn a_wav_that_is_not_ours_is_kept_and_reported() {
        let rig = rig("recover-corrupt", ok_capture());
        let folder = recordings(&rig).join("meeting-a");
        fs::create_dir_all(&folder).unwrap();
        fs::write(folder.join(WAV), b"short").unwrap();
        rig.recorder.recover_all();
        assert_eq!(fs::read(folder.join(WAV)).unwrap(), b"short");
        let notice = rig.recorder.status().notice.unwrap();
        assert_eq!(notice.kind, Kind::Error);
        assert_eq!(notice.folder.as_deref(), folder.to_str());
        assert!(
            notice
                .message
                .starts_with("Recovered 0 recordings. Could not convert 1: "),
            "{}",
            notice.message
        );
    }

    #[test]
    fn a_wav_without_audio_is_removed_with_its_empty_folder() {
        let rig = rig("recover-empty", ok_capture());
        let empty = leftover_in(&rig, "meeting-empty", 0);
        let with_context = leftover_in(&rig, "meeting-context", 0);
        fs::write(with_context.join(CONTEXT), b"{}").unwrap();
        rig.recorder.recover_all();
        assert!(!empty.exists());
        assert!(!with_context.join(WAV).exists());
        assert!(with_context.join(CONTEXT).is_file());
        assert_eq!(rig.recorder.status().notice, None);
    }

    #[test]
    fn a_failure_among_successes_gives_one_error_notice_with_its_folder() {
        let rig = rig("recover-mixed", ok_capture());
        leftover_in(&rig, "meeting-1", 1);
        let bad = recordings(&rig).join("meeting-2");
        fs::create_dir_all(&bad).unwrap();
        fs::write(
            bad.join(WAV),
            b"RIFF but nothing else, not ours at all.......",
        )
        .unwrap();
        leftover_in(&rig, "meeting-3", 1);
        rig.recorder.recover_all();
        let notice = rig.recorder.status().notice.unwrap();
        assert_eq!(notice.kind, Kind::Error);
        assert_eq!(notice.folder.as_deref(), bad.to_str());
        assert!(
            notice
                .message
                .starts_with("Recovered 2 recordings. Could not convert 1: "),
            "{}",
            notice.message
        );
        assert!(notice.message.contains("is not a recording this app wrote"));
    }

    #[test]
    fn a_running_recording_is_never_recovered() {
        let rig = rig("recover-running", ok_capture());
        let folder = rig.start().unwrap();
        push(SECOND);
        let old = leftover_in(&rig, "meeting-old", 1);
        rig.recorder.recover_all();
        assert!(!folder.join(M4A).exists());
        assert!(folder.join(WAV).is_file());
        assert!(old.join(M4A).is_file());
        assert_eq!(rig.state(), "recording");
        rig.stop(Reason::User);
    }

    #[test]
    fn nothing_found_shows_nothing() {
        let rig = rig("recover-none", ok_capture());
        rig.recorder.recover_all();
        fs::create_dir_all(recordings(&rig)).unwrap();
        rig.recorder.recover_all();
        assert_eq!(rig.recorder.status().notice, None);
    }

    #[test]
    fn the_summary_names_successes_and_the_first_failure() {
        assert_eq!(summary(0, &[]), None);
        assert_eq!(
            summary(2, &[]),
            Some(Notice::new(Kind::Recovered, "Recovered 2 recordings"))
        );
        let failed = [
            Notice::retry("ffmpeg failed: boom", Path::new("/r/a")),
            Notice::retry("ffmpeg failed: bang", Path::new("/r/b")),
        ];
        assert_eq!(
            summary(1, &failed),
            Some(Notice::retry(
                "Recovered 1 recording. Could not convert 2: ffmpeg failed: boom",
                Path::new("/r/a")
            ))
        );
    }

    fn wait_retry(saving: JoinHandle<()>) {
        saving.join().unwrap();
    }

    #[test]
    fn retry_refuses_paths_outside_the_recordings_folder() {
        let rig = rig("retry-paths", ok_capture());
        let folder = leftover_in(&rig, "meeting-a", 1);
        let outside = rig.home.join("elsewhere");
        fs::create_dir_all(&outside).unwrap();
        tone_wav(&outside.join(WAV), 1);
        let link = recordings(&rig).join("meeting-link");
        symlink(&outside, &link).unwrap();
        let archived = recordings(&rig).join(ARCHIVE).join("meeting-old");
        fs::create_dir_all(&archived).unwrap();
        tone_wav(&archived.join(WAV), 1);
        let no_wav = recordings(&rig).join("meeting-no-wav");
        fs::create_dir_all(&no_wav).unwrap();

        let dotdot = folder.join("..").join("..").join("elsewhere");
        for path in [
            outside.clone(),
            dotdot,
            link,
            archived,
            no_wav,
            recordings(&rig).join("meeting-missing"),
            recordings(&rig),
            PathBuf::from("relative"),
        ] {
            let refused = rig.recorder.retry(path.to_str().unwrap()).unwrap_err();
            assert_eq!(refused.code, "not_found", "{}", path.display());
        }
        assert!(!outside.join(M4A).exists());
        assert_eq!(rig.state(), "idle");
    }

    #[test]
    fn retry_is_refused_while_busy() {
        let rig = rig("retry-busy", ok_capture());
        let folder = leftover_in(&rig, "meeting-a", 1);
        let path = folder.to_str().unwrap();
        rig.start().unwrap();
        assert_eq!(
            rig.recorder.retry(path).unwrap_err().code,
            "already_recording"
        );
        rig.recorder.pause().unwrap();
        assert_eq!(
            rig.recorder.retry(path).unwrap_err().code,
            "already_recording"
        );
        rig.stop(Reason::User);
        let saving = rig.recorder.begin_saving().unwrap();
        assert_eq!(
            rig.recorder.retry(path).unwrap_err().code,
            "already_recording"
        );
        drop(saving);
        assert!(folder.join(WAV).is_file());
    }

    #[test]
    fn retry_after_a_failed_conversion_converts_and_sends() {
        let rig = rig("retry-ffmpeg", ok_capture());
        write_config(&rig.env(), broken_ffmpeg(&rig));
        let folder = rig.start().unwrap();
        push(11 * SECOND);
        let notice = rig.stop(Reason::User).notice.unwrap();
        assert_eq!(notice.folder.as_deref(), folder.to_str());

        // Still broken: the notice offers the retry again.
        wait_retry(rig.recorder.retry(folder.to_str().unwrap()).unwrap());
        let again = rig.recorder.status().notice.unwrap();
        assert_eq!(again, Notice::retry("ffmpeg failed: boom", &folder));
        assert!(spawns_under(&folder).is_empty());

        write_config(&rig.env(), ffmpeg());
        let saving = rig
            .recorder
            .retry(notice.folder.as_deref().unwrap())
            .unwrap();
        assert_eq!(rig.state(), "saving");
        wait_retry(saving);
        let status = rig.recorder.status();
        assert_eq!(status.state, "idle");
        assert_eq!(status.notice.unwrap().kind, Kind::Saved);
        assert!(folder.join(M4A).is_file());
        assert!(!folder.join(WAV).exists());
        // Sent in the language the recording was started in (`Rig::start` uses English).
        assert_eq!(spawns_under(&folder)[0].2, Some(Language::En));
        assert_eq!(
            rig.recorder
                .retry(folder.to_str().unwrap())
                .unwrap_err()
                .code,
            "not_found"
        );
    }

    #[test]
    fn retry_after_a_full_disk_converts_the_kept_wav() {
        let rig = rig("retry-disk", ok_capture());
        let folder = rig.start().unwrap();
        push(3 * SECOND);
        rig.stop(Reason::DiskFull);
        assert!(!folder.join(M4A).exists());
        wait_retry(rig.recorder.retry(folder.to_str().unwrap()).unwrap());
        assert_eq!(rig.recorder.status().notice.unwrap().kind, Kind::TooShort);
        assert!(folder.join(M4A).is_file());
    }
}
