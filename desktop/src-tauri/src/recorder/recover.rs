//! Crash recovery on launch and `recorder_retry` (PRD desktop-app FR-7, continue-recording
//! FR-12). A meeting folder directly inside `recordings_dir` that holds `recording.partial.wav`
//! and no other `recording.<ext>` is a recording a crash, a failed conversion or a full disk left
//! behind: its WAV is repaired from the file length, converted and handed off as a stopped
//! recording is. With continue recording built in, so is a continued part: a
//! `recording-part-N.partial.wav` beside the meeting's `recording.m4a`, converted to its part and
//! handed off with `--continue`; and a part's hard link left under `recordings_dir` is settled
//! first. Logs carry counts only.

use std::fs;
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::thread::{self, JoinHandle};
use std::time::UNIX_EPOCH;

use meeting::Language;

use super::handoff::{self, M4A, Tools, WAV};
use super::notice::{Kind, Notice};
use super::{Clock, Recorder, Refused, SAMPLE_RATE, State, folder, lock, same_file, wav};

/// What a conversion killed mid-way leaves; never a recording.
const PART: &str = "recording.m4a.part";
const ARCHIVE: &str = "Archive";

/// What a folder holds to recover.
#[derive(Debug, PartialEq)]
enum Leftover {
    /// The first recording's WAV, and no other `recording.*`.
    First,
    /// The numbers of continued parts' WAVs beside `recording.m4a`, ascending, each without its
    /// m4a.
    Parts(Vec<u32>),
}

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
        let parts = self.inner.deps.continue_enabled;
        if parts {
            self.settle_links(&config.recordings_dir);
        }
        let folders = candidates(&config.recordings_dir, parts);
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
        let starts = self.inner.next_id.load(Ordering::Relaxed);
        if self.session_folder().as_deref() == Some(folder) {
            return None;
        }
        match leftover(folder, self.inner.deps.continue_enabled)? {
            Leftover::First => self.recover_first(folder, language, ffmpeg, starts),
            Leftover::Parts(parts) => self.recover_parts(folder, &parts, language, ffmpeg, starts),
        }
    }

    fn recover_first(
        &self,
        folder: &Path,
        language: Language,
        ffmpeg: &Path,
        starts: u64,
    ) -> Option<Notice> {
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
        let notice = handoff::finish_recording(&tools, folder, language, frames, "");
        self.remember_recovered(starts, folder, frames / u64::from(SAMPLE_RATE), language, 1);
        Some(notice)
    }

    /// Converts each part in ascending order, then hands the meeting off once with `--continue`
    /// (FR-12). The meeting's `recording.m4a`, `context.json` and folder are never touched; a part
    /// killed at its start (no audio) is deleted. A failure keeps that part's WAV and those after
    /// it for a retry.
    fn recover_parts(
        &self,
        folder: &Path,
        parts: &[u32],
        language: Language,
        ffmpeg: &Path,
        starts: u64,
    ) -> Option<Notice> {
        // The meeting's totals before these parts, known when it is still the last recording.
        let earlier = self
            .stored_last()
            .filter(|last| Path::new(&last.folder) == folder);
        let tools = Tools {
            spawn: self.inner.deps.spawn_continue,
            ..self.tools(ffmpeg.to_path_buf())
        };
        let rate = u64::from(SAMPLE_RATE);
        // The meeting's totals once `frames` more are converted up to part `n`.
        let totals = |frames: u64, n: u32| {
            earlier.as_ref().map_or((frames / rate, n), |last| {
                (last.recorded_s + frames / rate, last.parts.max(n))
            })
        };
        let (mut frames, mut highest) = (0, None);
        for &n in parts {
            let wav = folder.join(handoff::part_wav(n));
            let failed = match wav::repair(&wav) {
                Ok(0) => {
                    let _ = fs::remove_file(&wav);
                    continue;
                }
                Ok(part) => match handoff::convert_part(&tools, folder, n, "") {
                    Ok(()) => {
                        frames += part;
                        highest = Some(n);
                        continue;
                    }
                    Err(notice) => notice,
                },
                Err(e) => {
                    tracing::warn!("recorder: a leftover part could not be repaired; it is kept");
                    Notice::retry(format!("{e:#}"), folder)
                }
            };
            // Parts converted before the failure count in the meeting's totals, so the retry
            // that converts the rest hands off the whole meeting with its right length.
            if let Some(converted) = highest {
                let (recorded_s, parts) = totals(frames, converted);
                self.remember_recovered(starts, folder, recorded_s, language, parts);
            }
            return Some(failed);
        }
        let n = highest?;
        let notice = match &earlier {
            Some(last) => {
                let total = last.recorded_s * rate + frames;
                handoff::hand_off(&tools, folder, language, total, "")
            }
            // Another recording became the last one since: the earlier total is unknown, so the
            // meeting is handed off whatever its length and the tool decides (a lost hand-off is
            // worse than a too-short one).
            None => handoff::hand_off(&tools, folder, language, u64::MAX, ""),
        };
        let (recorded_s, parts) = totals(frames, n);
        self.remember_recovered(starts, folder, recorded_s, language, parts);
        Some(notice)
    }

    /// Counted from now, the recovery (FR-4). Never replaces a recording started meanwhile or
    /// still running: checked under the state lock, which a start takes before it clears the
    /// last recording. `Saving` without a folder is a retry's own state.
    fn remember_recovered(
        &self,
        starts: u64,
        folder: &Path,
        recorded_s: u64,
        language: Language,
        parts: u32,
    ) {
        let state = self.state();
        let no_session = matches!(*state, State::Idle | State::Saving { folder: None, .. });
        if no_session && self.inner.next_id.load(Ordering::Relaxed) == starts {
            self.remember_last(folder, recorded_s, language, parts);
        }
    }

    /// Settles each part link `.continuing-<meeting>-part-N.partial.wav` left directly under `dir`
    /// (D17) before the folders are scanned, in name order. A link of the running session is
    /// left alone; other hidden files and names that do not parse are ignored.
    fn settle_links(&self, dir: &Path) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        let mut links: Vec<(PathBuf, String, u32)> = entries
            .flatten()
            .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
            .filter_map(|e| {
                let name = e.file_name();
                let (meeting, n) = parse_link(name.to_str()?)?;
                Some((e.path(), meeting.to_string(), n))
            })
            .collect();
        links.sort();
        for (link, meeting, n) in links {
            self.settle_link(dir, &link, &meeting, n);
        }
    }

    /// The part's WAV still in its meeting folder beside `recording.m4a`: the link goes and the
    /// folder scan recovers the part. The part already converted: the link goes. Otherwise the
    /// meeting was moved or its recording is gone: the link becomes a new meeting's WAV.
    fn settle_link(&self, dir: &Path, link: &Path, meeting: &str, n: u32) {
        let _one = lock(&self.inner.converting);
        let folder = dir.join(meeting);
        if self.session_folder().as_deref() == Some(folder.as_path()) {
            return;
        }
        let left = folder.join(handoff::part_wav(n));
        let same = same_file(&left, link);
        let converted = folder.join(handoff::part_m4a(n)).is_file();
        if (same && folder.join(M4A).is_file()) || (converted && !left.exists()) {
            let _ = fs::remove_file(link);
        } else {
            self.link_to_new_meeting(dir, link, &left, same);
        }
    }

    /// Moves `link` into a new meeting folder as its `recording.partial.wav`, for the folder scan
    /// to recover as a new meeting (FR-12: today's recovery into a new folder). The same audio
    /// left as `left` in a meeting folder without its recording goes, as a stop does it (D17).
    /// On failure the link is kept for the next launch.
    fn link_to_new_meeting(&self, dir: &Path, link: &Path, left: &Path, same: bool) {
        let started = self.started_at(link);
        let now_s = u64::try_from(started.now_ms.div_euclid(1000)).unwrap_or(0);
        let Ok(new) = folder::new_folder(dir, self.inner.deps.local_minute, now_s) else {
            tracing::warn!("recorder: a leftover part could not become a new meeting; it is kept");
            return;
        };
        if fs::rename(link, new.join(WAV)).is_err() {
            let _ = fs::remove_dir(&new);
            tracing::warn!("recorder: a leftover part could not be moved; it is kept");
            return;
        }
        if same {
            let _ = fs::remove_file(left);
        }
    }

    /// The folder of the recording that is starting, running or saving: never touched by
    /// recovery.
    pub(super) fn session_folder(&self) -> Option<PathBuf> {
        match &*self.state() {
            State::Starting(folder) => folder.clone(),
            State::Active(s) => Some(s.folder.clone()),
            State::Saving { folder, .. } => folder.clone(),
            State::Idle => None,
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
    /// only a direct subfolder of `recordings_dir` with a leftover WAV (a part's included) is
    /// accepted. Answers at once; the returned thread converts, hands off, sets the notice and
    /// returns to `idle`.
    pub fn retry(&self, folder: &str) -> Result<JoinHandle<()>, Refused> {
        let config = self
            .inner
            .env
            .config()
            .map_err(|message| Refused::new("not_found", message))?;
        let parts = self.inner.deps.continue_enabled;
        let folder = retry_folder(&config.recordings_dir, Path::new(folder), parts)
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

/// Direct subfolders of `dir` holding audio to recover, oldest first (the name is the start
/// time). Part WAVs whose meeting has no `recording.m4a` are kept untouched and only counted.
fn candidates(dir: &Path, parts: bool) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let (mut folders, mut kept) = (Vec::new(), 0);
    for entry in entries.flatten() {
        if !entry.file_type().is_ok_and(|t| t.is_dir()) || entry.file_name() == ARCHIVE {
            continue;
        }
        let folder = entry.path();
        if holds_audio(&folder, parts) {
            folders.push(folder);
        } else if parts {
            kept += part_wavs(&folder).len();
        }
    }
    if kept > 0 {
        tracing::info!("recorder: recovery: {kept} parts kept, their meeting has no recording");
    }
    folders.sort();
    folders
}

/// Whether `folder` holds the first recording's WAV, or (with `parts`) a part's WAV beside
/// `recording.m4a`.
fn holds_audio(folder: &Path, parts: bool) -> bool {
    folder.join(WAV).is_file()
        || (parts && folder.join(M4A).is_file() && !part_wavs(folder).is_empty())
}

/// What `folder` holds to recover, after removing stale `.part` files: the WAV with no other
/// `recording.*`, else (with `parts`) the part WAVs beside `recording.m4a` that have no m4a.
fn leftover(folder: &Path, parts: bool) -> Option<Leftover> {
    let _ = fs::remove_file(folder.join(PART));
    let names: Vec<String> = fs::read_dir(folder)
        .ok()?
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    let others = names
        .iter()
        .any(|n| n != WAV && n.starts_with("recording."));
    if names.iter().any(|n| n == WAV) && !others {
        return Some(Leftover::First);
    }
    if !parts || !folder.join(M4A).is_file() {
        return None;
    }
    let left: Vec<u32> = part_wavs(folder)
        .into_iter()
        .filter(|&n| {
            let m4a = handoff::part_m4a(n);
            let _ = fs::remove_file(folder.join(format!("{m4a}.part")));
            !folder.join(m4a).exists()
        })
        .collect();
    (!left.is_empty()).then_some(Leftover::Parts(left))
}

/// The numbers of the `recording-part-N.partial.wav` files in `folder`, ascending.
fn part_wavs(folder: &Path) -> Vec<u32> {
    let Ok(entries) = fs::read_dir(folder) else {
        return Vec::new();
    };
    let mut parts: Vec<u32> = entries
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .filter_map(|e| {
            let name = e.file_name();
            let n = name
                .to_str()?
                .strip_prefix("recording-part-")?
                .strip_suffix(".partial.wav")?;
            part_number(n)
        })
        .collect();
    parts.sort_unstable();
    parts
}

/// The meeting folder name and part number of a part link's file name
/// (`handoff::part_link`); the last `-part-` splits them.
fn parse_link(name: &str) -> Option<(&str, u32)> {
    let rest = name
        .strip_prefix(".continuing-")?
        .strip_suffix(".partial.wav")?;
    let (meeting, n) = rest.rsplit_once("-part-")?;
    let n = part_number(n)?;
    (!matches!(meeting, "" | "." | "..")).then_some((meeting, n))
}

/// A part number: digits only, 2 or more (part 1 is `recording.m4a`).
fn part_number(digits: &str) -> Option<u32> {
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok().filter(|&n| n >= 2)
}

/// `recordings_dir/<name>` when `folder` is a direct subfolder of `recordings_dir` (not
/// `Archive`) holding audio to recover (`holds_audio`). Canonicalising resolves `..` and symlinks
/// before the check; the answer is in the configured form, as the recorder's own notices name
/// folders.
fn retry_folder(recordings_dir: &Path, folder: &Path, parts: bool) -> Option<PathBuf> {
    let dir = recordings_dir.canonicalize().ok()?;
    let canonical = folder.canonicalize().ok()?;
    let name = canonical.file_name().filter(|n| *n != ARCHIVE)?;
    let direct = canonical.parent() == Some(dir.as_path());
    (direct && holds_audio(&canonical, parts)).then(|| recordings_dir.join(name))
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
    use crate::recorder::continuable::LastRecording;
    use crate::recorder::continuable::tests::store_last;
    use crate::recorder::convert;
    use crate::recorder::convert::tests::{ffmpeg, probe, tone_wav};
    use crate::recorder::handoff::CONTEXT;
    use crate::recorder::handoff::tests::{clock, continues_under, spawns_under};
    use crate::recorder::tests::{Rig, SECOND, ok_capture, push, rig, rig_unflagged, write_config};
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
        let found: Vec<_> = candidates(&recordings(&rig), true)
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

    thread_local! {
        /// The recorder a racing capture runs recovery on.
        static RACING: std::cell::RefCell<Option<Recorder>> = const { std::cell::RefCell::new(None) };
    }

    fn recover_racing() {
        RACING.with(|r| r.borrow().as_ref().expect("set by the test").recover_all());
    }

    /// Runs recovery once the WAV exists while the state is still `Starting`, as a link start
    /// at launch does (#262), then starts the capture.
    fn start_racing_recovery(
        samples: crate::capture::Samples,
        events: crate::capture::Events,
    ) -> anyhow::Result<crate::capture::Stream> {
        // A panic here makes the start fail, which the test sees.
        RACING.with(|r| {
            let recorder = r.borrow().clone().expect("set by the test");
            assert_eq!(recorder.status().state, "idle");
            let second = recorder.start(Language::En, || Ok(None));
            assert_eq!(second.unwrap_err().code, "already_recording");
        });
        recover_racing();
        (ok_capture().start)(samples, events)
    }

    fn start_racing_recovery_broken(
        _: crate::capture::Samples,
        _: crate::capture::Events,
    ) -> anyhow::Result<crate::capture::Stream> {
        recover_racing();
        Err(anyhow::anyhow!("broken"))
    }

    fn racing_rig(
        name: &str,
        start: fn(
            crate::capture::Samples,
            crate::capture::Events,
        ) -> anyhow::Result<crate::capture::Stream>,
    ) -> Rig {
        let mut capture = ok_capture();
        capture.start = start;
        let rig = rig(name, capture);
        RACING.with(|r| *r.borrow_mut() = Some(rig.recorder.clone()));
        rig
    }

    #[test]
    fn a_starting_recording_is_never_recovered() {
        let rig = racing_rig("recover-starting", start_racing_recovery);
        let old = leftover_in(&rig, "meeting-old", 1);
        let folder = rig.start().unwrap();
        assert!(folder.join(WAV).is_file());
        assert!(old.join(M4A).is_file());
        push(40 * SECOND);
        let status = rig.stop(Reason::User);
        assert_eq!(status.notice.unwrap().kind, Kind::Saved);
        assert!(folder.join(M4A).is_file());
    }

    #[test]
    fn a_failed_start_leaves_nothing_for_recovery() {
        let rig = racing_rig("recover-starting-failed", start_racing_recovery_broken);
        let refused = rig.start().unwrap_err();
        assert_eq!(refused.code, "capture_failed");
        assert_eq!(rig.state(), "idle");
        assert!(candidates(&recordings(&rig), true).is_empty());
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
    fn dismissing_an_error_notice_keeps_the_wav_for_recovery() {
        let rig = rig("dismiss-error-notice", ok_capture());
        write_config(&rig.env(), broken_ffmpeg(&rig));
        let folder = rig.start().unwrap();
        push(11 * SECOND);
        let notice = rig.stop(Reason::User).notice.unwrap();
        assert_eq!(notice, Notice::retry("ffmpeg failed: boom", &folder));
        rig.recorder.dismiss_notice();
        // Cleared whole: no `folder` is left for the window to retry.
        assert_eq!(rig.recorder.status().notice, None);
        assert!(folder.join(WAV).is_file());

        // The next launch's recovery still converts the kept WAV.
        write_config(&rig.env(), ffmpeg());
        rig.recorder.recover_all();
        assert!(folder.join(M4A).is_file());
        assert!(!folder.join(WAV).exists());
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

    /// The stored last recording's folder, recorded seconds and stop time.
    fn last_recording(rig: &Rig) -> Option<(String, u64, i64)> {
        let raw = rig
            .env()
            .store()
            .unwrap()
            .setting("last_recording")
            .unwrap()?;
        let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
        (!v.is_null()).then(|| {
            (
                v["folder"].as_str().unwrap().to_string(),
                v["recorded_s"].as_u64().unwrap(),
                v["stopped_at"].as_i64().unwrap(),
            )
        })
    }

    fn offered(rig: &Rig) -> Option<String> {
        rig.recorder.status().continuable.map(|c| c.folder)
    }

    #[test]
    fn a_recovered_recording_counts_from_recovery_time() {
        let rig = rig("continuable-recovered", ok_capture());
        let older = leftover_in(&rig, "meeting-2026-11-13_0800", 2);
        let newest = recordings(&rig).join("meeting-2026-11-13_0900");
        fs::create_dir_all(&newest).unwrap();
        killed_wav(&newest.join(WAV), 12);
        rig.recorder.recover_all();
        assert!(older.join(M4A).is_file());
        // The rig's fixed clock, not the WAV's time (today on the real clock).
        assert_eq!(
            last_recording(&rig),
            Some((newest.display().to_string(), 12, clock().now_ms))
        );
        assert_eq!(offered(&rig).as_deref(), newest.to_str());
    }

    #[test]
    fn recovery_does_not_replace_a_running_recording() {
        let rig = rig("continuable-recover-running", ok_capture());
        let folder = rig.start().unwrap();
        push(3 * SECOND);
        let old = leftover_in(&rig, "meeting-old", 2);
        rig.recorder.recover_all();
        assert!(old.join(M4A).is_file());
        assert_eq!(last_recording(&rig), None);
        rig.stop(Reason::User);
        assert_eq!(offered(&rig).as_deref(), folder.to_str());
    }

    #[test]
    fn recovery_does_not_replace_a_recording_started_meanwhile() {
        let rig = rig("continuable-recover-meanwhile", ok_capture());
        // An ffmpeg that waits for `go`, so a recording starts and stops mid-recovery.
        let (started, go) = (rig.home.join("started"), rig.home.join("go"));
        let slow = rig.home.join("ffmpeg-slow");
        fs::write(
            &slow,
            format!(
                "#!/bin/sh\ntouch '{}'\nwhile [ ! -f '{}' ]; do sleep 0.05; done\nexec '{}' \"$@\"\n",
                started.display(),
                go.display(),
                ffmpeg().display()
            ),
        )
        .unwrap();
        fs::set_permissions(&slow, fs::Permissions::from_mode(0o755)).unwrap();
        write_config(&rig.env(), slow);
        let old = leftover_in(&rig, "meeting-old", 12);

        let recovering = {
            let recorder = rig.recorder.clone();
            thread::spawn(move || recorder.recover_all())
        };
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        while !started.exists() {
            assert!(
                std::time::Instant::now() < deadline,
                "recovery never converted"
            );
            thread::sleep(Duration::from_millis(20));
        }
        let folder = rig.start().unwrap();
        push(3 * SECOND);
        fs::write(&go, b"").unwrap();
        rig.stop(Reason::User);
        recovering.join().unwrap();
        assert!(old.join(M4A).is_file());
        assert_eq!(offered(&rig).as_deref(), folder.to_str());
    }

    #[test]
    fn a_failed_conversion_is_not_continuable_until_retried() {
        let rig = rig("continuable-retry", ok_capture());
        write_config(&rig.env(), broken_ffmpeg(&rig));
        let folder = rig.start().unwrap();
        push(11 * SECOND);
        rig.stop(Reason::User);
        assert!(folder.join(WAV).is_file());
        assert_eq!(
            last_recording(&rig).map(|l| l.0).as_deref(),
            folder.to_str()
        );
        assert_eq!(offered(&rig), None);

        write_config(&rig.env(), ffmpeg());
        wait_retry(rig.recorder.retry(folder.to_str().unwrap()).unwrap());
        assert_eq!(offered(&rig).as_deref(), folder.to_str());
        assert_eq!(last_recording(&rig).map(|l| l.1), Some(11));
    }

    #[test]
    fn a_full_disk_is_not_continuable_until_retried() {
        let rig = rig("continuable-disk", ok_capture());
        let folder = rig.start().unwrap();
        push(3 * SECOND);
        rig.stop(Reason::DiskFull);
        assert_eq!(offered(&rig), None);
        wait_retry(rig.recorder.retry(folder.to_str().unwrap()).unwrap());
        assert_eq!(offered(&rig).as_deref(), folder.to_str());
    }

    // Continued parts (continue-recording FR-12).

    const MEETING: &str = "meeting-2026-11-13_0500";

    /// A meeting folder with `recording.m4a` and `context.json`, stored as the last recording
    /// with `recorded_s` (1 part, Bengali).
    fn meeting_in(rig: &Rig, recorded_s: u64) -> PathBuf {
        let folder = recordings(rig).join(MEETING);
        fs::create_dir_all(&folder).unwrap();
        tone_wav(&folder.join(WAV), 1);
        convert::to_m4a(&ffmpeg(), &folder.join(WAV), &folder.join(M4A)).unwrap();
        fs::remove_file(folder.join(WAV)).unwrap();
        handoff::write_context(&folder, &clock(), None);
        store_last(
            rig,
            &LastRecording {
                folder: folder.display().to_string(),
                recorded_s,
                language: Language::Bn,
                stopped_at: clock().now_ms - 60_000,
                parts: 1,
            },
        );
        folder
    }

    fn part_link(rig: &Rig, meeting: &str, n: u32) -> PathBuf {
        recordings(rig).join(handoff::part_link(meeting, n))
    }

    /// The names directly under the recordings folder, sorted.
    fn entries(rig: &Rig) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(recordings(rig))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    fn bytes(path: &Path) -> Vec<u8> {
        fs::read(path).unwrap()
    }

    fn stored(rig: &Rig) -> LastRecording {
        rig.recorder.stored_last().expect("a last recording")
    }

    #[test]
    fn a_killed_part_is_converted_into_its_meeting_and_handed_off_with_continue() {
        let rig = rig("recover-part-killed", ok_capture());
        let folder = meeting_in(&rig, 20);
        let (m4a, context) = (bytes(&folder.join(M4A)), bytes(&folder.join(CONTEXT)));
        killed_wav(&folder.join(handoff::part_wav(2)), 12);
        // The language last started with, as for a first recording.
        rig.env()
            .store()
            .unwrap()
            .set_setting("language", "en")
            .unwrap();
        rig.recorder.recover_all();

        let duration = duration_s(&folder.join(handoff::part_m4a(2)));
        assert!((duration - 12.0).abs() <= 1.0, "{duration}");
        assert!(!folder.join(handoff::part_wav(2)).exists());
        assert_eq!(bytes(&folder.join(M4A)), m4a);
        assert_eq!(bytes(&folder.join(CONTEXT)), context);
        assert_eq!(entries(&rig), [MEETING]);
        assert_eq!(
            continues_under(&folder),
            [(
                rig.home.join("meeting"),
                folder.join(M4A),
                Some(Language::En)
            )]
        );
        assert!(spawns_under(&folder).is_empty());
        assert_eq!(
            rig.recorder.status().notice,
            Some(Notice::new(Kind::Recovered, "Recovered 1 recording"))
        );
        assert_eq!(
            stored(&rig),
            LastRecording {
                folder: folder.display().to_string(),
                recorded_s: 32,
                language: Language::En,
                stopped_at: clock().now_ms,
                parts: 2,
            }
        );
        assert_eq!(
            rig.recorder.status().continuable.map(|c| c.folder),
            folder.to_str().map(String::from)
        );
    }

    #[test]
    fn two_parts_are_both_converted_with_one_hand_off() {
        let rig = rig("recover-part-two", ok_capture());
        let folder = meeting_in(&rig, 20);
        killed_wav(&folder.join(handoff::part_wav(3)), 2);
        killed_wav(&folder.join(handoff::part_wav(2)), 3);
        rig.recorder.recover_all();
        for n in [2, 3] {
            assert!(folder.join(handoff::part_m4a(n)).is_file(), "{n}");
            assert!(!folder.join(handoff::part_wav(n)).exists(), "{n}");
        }
        assert_eq!(continues_under(&folder).len(), 1);
        let last = stored(&rig);
        assert_eq!((last.recorded_s, last.parts), (25, 3));
        assert_eq!(
            rig.recorder.status().notice.unwrap().message,
            "Recovered 1 recording"
        );
    }

    #[test]
    fn part_wavs_are_listed_in_ascending_number() {
        let dir = crate::recorder::wav::tests::temp("recover-part-order");
        for name in [
            "recording-part-10.partial.wav",
            "recording-part-2.partial.wav",
            "recording-part-3.partial.wav",
            "recording-part-1.partial.wav",
            "recording-part-x.partial.wav",
            "recording-part-4.m4a",
        ] {
            fs::write(dir.join(name), b"").unwrap();
        }
        assert_eq!(part_wavs(&dir), [2, 3, 10]);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_part_without_audio_is_deleted_and_the_meeting_kept() {
        let rig = rig("recover-part-empty", ok_capture());
        let folder = meeting_in(&rig, 20);
        let before = stored(&rig);
        killed_wav(&folder.join(handoff::part_wav(2)), 0);
        rig.recorder.recover_all();
        assert!(!folder.join(handoff::part_wav(2)).exists());
        assert!(folder.join(M4A).is_file());
        assert!(folder.join(CONTEXT).is_file());
        assert!(continues_under(&rig.home).is_empty());
        assert_eq!(rig.recorder.status().notice, None);
        assert_eq!(stored(&rig), before);
    }

    #[test]
    fn a_meeting_under_ten_seconds_in_total_is_kept_but_not_handed_off() {
        let rig = rig("recover-part-short", ok_capture());
        let folder = meeting_in(&rig, 5);
        killed_wav(&folder.join(handoff::part_wav(2)), 3);
        rig.recorder.recover_all();
        assert!(folder.join(handoff::part_m4a(2)).is_file());
        assert!(continues_under(&rig.home).is_empty());
        assert_eq!(
            rig.recorder.status().notice.unwrap().message,
            "Recovered 1 recording"
        );
        assert_eq!(stored(&rig).recorded_s, 8);
    }

    #[test]
    fn a_part_whose_earlier_total_is_unknown_is_handed_off_anyway() {
        let rig = rig("recover-part-unknown", ok_capture());
        let folder = meeting_in(&rig, 600);
        // A later Start replaced the last recording.
        let other = leftover_in(&rig, "meeting-later", 1);
        fs::rename(other.join(WAV), other.join(M4A)).unwrap();
        store_last(
            &rig,
            &LastRecording {
                folder: other.display().to_string(),
                recorded_s: 1,
                language: Language::Bn,
                stopped_at: clock().now_ms,
                parts: 1,
            },
        );
        killed_wav(&folder.join(handoff::part_wav(2)), 1);
        rig.recorder.recover_all();
        assert_eq!(continues_under(&folder).len(), 1);
        let last = stored(&rig);
        assert_eq!(
            (last.folder.as_str(), last.recorded_s, last.parts),
            (folder.to_str().unwrap(), 1, 2)
        );
    }

    #[test]
    fn a_link_beside_its_part_is_deleted_and_the_part_recovered_once() {
        let rig = rig("recover-link-beside", ok_capture());
        let folder = meeting_in(&rig, 20);
        let wav = folder.join(handoff::part_wav(2));
        killed_wav(&wav, 12);
        let link = part_link(&rig, MEETING, 2);
        fs::hard_link(&wav, &link).unwrap();
        rig.recorder.recover_all();
        assert!(!link.exists());
        assert!(folder.join(handoff::part_m4a(2)).is_file());
        assert_eq!(continues_under(&rig.home).len(), 1);
        assert!(spawns_under(&rig.home).is_empty());
        assert_eq!(entries(&rig), [MEETING]);
        assert_eq!(
            rig.recorder.status().notice.unwrap().message,
            "Recovered 1 recording"
        );
    }

    /// Asserts the link was recovered as a new meeting, handed off without `--continue`.
    fn recovered_as_new(rig: &Rig, link: &Path) -> PathBuf {
        assert!(!link.exists());
        let new = recordings(rig).join("meeting-2026-11-13_0600");
        assert!(new.join(M4A).is_file());
        assert!(new.join(CONTEXT).is_file());
        assert!(!new.join(WAV).exists());
        let duration = duration_s(&new.join(M4A));
        assert!((duration - 12.0).abs() <= 1.0, "{duration}");
        assert_eq!(
            spawns_under(&new),
            [(rig.home.join("meeting"), new.join(M4A), Some(Language::Bn))]
        );
        assert!(continues_under(&rig.home).is_empty());
        assert_eq!(
            rig.recorder.status().notice.unwrap().message,
            "Recovered 1 recording"
        );
        new
    }

    #[test]
    fn a_link_whose_meeting_is_gone_becomes_a_new_meeting() {
        let rig = rig("recover-link-gone", ok_capture());
        fs::create_dir_all(recordings(&rig)).unwrap();
        let link = part_link(&rig, MEETING, 2);
        killed_wav(&link, 12);
        rig.recorder.recover_all();
        recovered_as_new(&rig, &link);
        assert_eq!(entries(&rig), ["meeting-2026-11-13_0600"]);
    }

    #[test]
    fn a_link_whose_meeting_was_archived_becomes_a_new_meeting() {
        let rig = rig("recover-link-archived", ok_capture());
        let folder = meeting_in(&rig, 20);
        let wav = folder.join(handoff::part_wav(2));
        killed_wav(&wav, 12);
        let link = part_link(&rig, MEETING, 2);
        fs::hard_link(&wav, &link).unwrap();
        let archived = recordings(&rig).join(ARCHIVE).join(MEETING);
        fs::create_dir_all(archived.parent().unwrap()).unwrap();
        fs::rename(&folder, &archived).unwrap();
        rig.recorder.recover_all();
        recovered_as_new(&rig, &link);
        // The archived copy is not looked at.
        assert!(archived.join(handoff::part_wav(2)).is_file());
        assert!(!archived.join(handoff::part_m4a(2)).exists());
    }

    #[test]
    fn a_link_whose_meeting_lost_its_recording_becomes_a_new_meeting() {
        let rig = rig("recover-link-no-m4a", ok_capture());
        let folder = meeting_in(&rig, 20);
        let wav = folder.join(handoff::part_wav(2));
        killed_wav(&wav, 12);
        let link = part_link(&rig, MEETING, 2);
        fs::hard_link(&wav, &link).unwrap();
        fs::remove_file(folder.join(M4A)).unwrap();
        rig.recorder.recover_all();
        recovered_as_new(&rig, &link);
        // The copy left in the folder was the same audio, as a stop does it.
        assert!(!wav.exists());
        assert!(folder.join(CONTEXT).is_file());
    }

    #[test]
    fn a_link_of_a_saved_part_is_deleted_and_nothing_sent() {
        let rig = rig("recover-link-saved", ok_capture());
        let folder = meeting_in(&rig, 20);
        fs::write(folder.join(handoff::part_m4a(2)), b"saved").unwrap();
        let link = part_link(&rig, MEETING, 2);
        killed_wav(&link, 12);
        rig.recorder.recover_all();
        assert!(!link.exists());
        assert_eq!(bytes(&folder.join(handoff::part_m4a(2))), b"saved");
        assert_eq!(entries(&rig), [MEETING]);
        assert!(continues_under(&rig.home).is_empty());
        assert!(spawns_under(&rig.home).is_empty());
        assert_eq!(rig.recorder.status().notice, None);
    }

    #[test]
    fn other_hidden_files_and_unparsed_links_are_left_alone() {
        let rig = rig("recover-link-other", ok_capture());
        fs::create_dir_all(recordings(&rig)).unwrap();
        let names = [
            ".DS_Store",
            ".continuing-meeting-a-part-1.partial.wav",
            ".continuing-meeting-a-part-x.partial.wav",
            ".continuing-meeting-a-part-2.wav",
            ".continuing--part-2.partial.wav",
            ".continuing-..-part-2.partial.wav",
            ".other-meeting-a-part-2.partial.wav",
        ];
        for name in names {
            killed_wav(&recordings(&rig).join(name), 1);
        }
        rig.recorder.recover_all();
        let mut expected: Vec<_> = names.iter().map(|n| n.to_string()).collect();
        expected.sort();
        assert_eq!(entries(&rig), expected);
        assert_eq!(rig.recorder.status().notice, None);
        assert_eq!(
            parse_link(".continuing-a-part-3-part-12.partial.wav"),
            Some(("a-part-3", 12))
        );
    }

    #[test]
    fn a_continuing_session_keeps_its_link_and_part() {
        let rig = rig("recover-link-live", ok_capture());
        let folder = meeting_in(&rig, 20);
        assert_eq!(rig.recorder.continue_recording().unwrap(), folder);
        push(SECOND);
        rig.recorder.recover_all();
        assert!(part_link(&rig, MEETING, 2).is_file());
        assert!(folder.join(handoff::part_wav(2)).is_file());
        assert!(!folder.join(handoff::part_m4a(2)).exists());
        assert_eq!(rig.state(), "recording");
        rig.stop(Reason::User);
        assert!(!part_link(&rig, MEETING, 2).exists());
        assert_eq!(continues_under(&folder).len(), 1);
    }

    #[test]
    fn a_failed_part_conversion_is_kept_reported_and_retried() {
        let rig = rig("recover-part-ffmpeg", ok_capture());
        let folder = meeting_in(&rig, 20);
        let m4a = bytes(&folder.join(M4A));
        killed_wav(&folder.join(handoff::part_wav(2)), 12);
        write_config(&rig.env(), broken_ffmpeg(&rig));
        rig.recorder.recover_all();
        let notice = rig.recorder.status().notice.unwrap();
        assert_eq!(
            notice,
            Notice::retry(
                "Recovered 0 recordings. Could not convert 1: ffmpeg failed: boom",
                &folder
            )
        );
        assert!(folder.join(handoff::part_wav(2)).is_file());
        assert!(!folder.join(handoff::part_m4a(2)).exists());
        assert_eq!(bytes(&folder.join(M4A)), m4a);
        assert!(continues_under(&folder).is_empty());
        assert_eq!(stored(&rig).parts, 1);

        // The notice's Retry converts it and hands it off.
        write_config(&rig.env(), ffmpeg());
        wait_retry(
            rig.recorder
                .retry(notice.folder.as_deref().unwrap())
                .unwrap(),
        );
        assert_eq!(rig.recorder.status().notice.unwrap().kind, Kind::Saved);
        assert!(folder.join(handoff::part_m4a(2)).is_file());
        assert!(!folder.join(handoff::part_wav(2)).exists());
        assert_eq!(continues_under(&folder).len(), 1);
        assert_eq!(stored(&rig).parts, 2);
        assert_eq!(
            rig.recorder
                .retry(folder.to_str().unwrap())
                .unwrap_err()
                .code,
            "not_found"
        );
    }

    #[test]
    fn parts_converted_before_a_failure_count_in_the_retry() {
        let rig = rig("recover-part-later-fails", ok_capture());
        let folder = meeting_in(&rig, 20);
        killed_wav(&folder.join(handoff::part_wav(2)), 3);
        fs::write(folder.join(handoff::part_wav(3)), b"short").unwrap();
        rig.recorder.recover_all();
        assert!(folder.join(handoff::part_m4a(2)).is_file());
        assert_eq!(bytes(&folder.join(handoff::part_wav(3))), b"short");
        assert_eq!(rig.recorder.status().notice.unwrap().kind, Kind::Error);
        assert!(continues_under(&folder).is_empty());
        let last = stored(&rig);
        assert_eq!((last.recorded_s, last.parts), (23, 2));

        // A good part 3 now: the retry hands off the whole meeting, counted once.
        fs::remove_file(folder.join(handoff::part_wav(3))).unwrap();
        killed_wav(&folder.join(handoff::part_wav(3)), 4);
        wait_retry(rig.recorder.retry(folder.to_str().unwrap()).unwrap());
        assert_eq!(continues_under(&folder).len(), 1);
        let last = stored(&rig);
        assert_eq!((last.recorded_s, last.parts), (27, 3));
    }

    #[test]
    fn only_part_wavs_beside_the_meeting_recording_are_recovered() {
        let rig = rig("recover-part-skip", ok_capture());
        let folder = meeting_in(&rig, 20);
        // A conversion killed mid-way: its `.part` goes, the WAV is converted again.
        tone_wav(&folder.join(handoff::part_wav(2)), 1);
        fs::write(folder.join("recording-part-2.m4a.part"), b"half").unwrap();
        // Already converted: not recovered again.
        tone_wav(&folder.join(handoff::part_wav(3)), 1);
        fs::write(folder.join(handoff::part_m4a(3)), b"saved").unwrap();
        // No `recording.m4a` beside it.
        let lone = recordings(&rig).join("meeting-lone");
        fs::create_dir_all(&lone).unwrap();
        tone_wav(&lone.join(handoff::part_wav(2)), 1);
        // In the archive, and behind a symlink.
        let archived = recordings(&rig).join(ARCHIVE).join("meeting-old");
        fs::create_dir_all(&archived).unwrap();
        tone_wav(&archived.join(WAV), 1);
        fs::rename(archived.join(WAV), archived.join(M4A)).unwrap();
        tone_wav(&archived.join(handoff::part_wav(2)), 1);
        let outside = rig.home.join("elsewhere");
        fs::create_dir_all(&outside).unwrap();
        fs::copy(archived.join(M4A), outside.join(M4A)).unwrap();
        tone_wav(&outside.join(handoff::part_wav(2)), 1);
        symlink(&outside, recordings(&rig).join("meeting-link")).unwrap();

        rig.recorder.recover_all();
        assert!(!folder.join("recording-part-2.m4a.part").exists());
        assert!(folder.join(handoff::part_m4a(2)).is_file());
        assert!(folder.join(handoff::part_wav(3)).is_file());
        assert_eq!(bytes(&folder.join(handoff::part_m4a(3))), b"saved");
        assert_eq!(fs::read_dir(&lone).unwrap().count(), 1);
        assert!(lone.join(handoff::part_wav(2)).is_file());
        assert!(archived.join(handoff::part_wav(2)).is_file());
        assert!(!archived.join(handoff::part_m4a(2)).exists());
        assert!(outside.join(handoff::part_wav(2)).is_file());
        assert!(!outside.join(handoff::part_m4a(2)).exists());
        assert_eq!(continues_under(&rig.home).len(), 1);
        assert_eq!(
            rig.recorder.status().notice.unwrap().message,
            "Recovered 1 recording"
        );
    }

    #[test]
    fn a_continue_starting_while_recovery_scans_is_never_recovered() {
        let rig = racing_rig("recover-part-starting", start_racing_recovery);
        let folder = meeting_in(&rig, 20);
        assert_eq!(rig.recorder.continue_recording().unwrap(), folder);
        assert!(folder.join(handoff::part_wav(2)).is_file());
        assert!(part_link(&rig, MEETING, 2).is_file());
        push(12 * SECOND);
        let status = rig.stop(Reason::User);
        assert_eq!(status.notice.unwrap().kind, Kind::Saved);
        assert!(folder.join(handoff::part_m4a(2)).is_file());
        assert_eq!(continues_under(&folder).len(), 1);
    }

    #[test]
    fn flag_off_recovery_ignores_parts_and_links() {
        let rig = rig_unflagged("recover-part-off");
        let folder = meeting_in(&rig, 20);
        killed_wav(&folder.join(handoff::part_wav(2)), 12);
        let link = part_link(&rig, "meeting-gone", 2);
        killed_wav(&link, 12);
        rig.recorder.recover_all();
        assert!(folder.join(handoff::part_wav(2)).is_file());
        assert!(!folder.join(handoff::part_m4a(2)).exists());
        assert!(link.is_file());
        assert_eq!(
            entries(&rig),
            [link.file_name().unwrap().to_str().unwrap(), MEETING]
        );
        assert!(continues_under(&rig.home).is_empty());
        assert!(spawns_under(&rig.home).is_empty());
        assert_eq!(rig.recorder.status().notice, None);
        assert_eq!(
            rig.recorder
                .retry(folder.to_str().unwrap())
                .unwrap_err()
                .code,
            "not_found"
        );
    }
}
