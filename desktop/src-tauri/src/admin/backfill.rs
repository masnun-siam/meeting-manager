//! Backfill: reads the meeting folders on disk into the notes store (FR-14). Read-only on the
//! user's folders; the one file it may move is a broken store, set aside before a rebuild.

use std::ffi::OsString;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use anyhow::Context as _;
use meeting::config::{Config, RECORDING_EXTENSIONS, check_recording};
use meeting::jobs::{Job, JobStore, Stage, now_utc};
use meeting::lock::{self, Lock};
use meeting::meta::{Meta, recorded_at_for};
use meeting::store::{self, OpenError, RunResult, Status, Store};
use meeting::{context, pipeline, summary_md, transcript};

/// The store setting that marks the first backfill as done.
pub const BACKFILL_DONE: &str = "backfill_done";

const NO_SPEECH: &str = "No speech detected";

/// A failed folder with no record of why: no `meta.json` run and no job say so.
const UNKNOWN_FAILURE: &str = "this meeting was not summarised and no reason was recorded; \
                               rerun the summary";

/// What `rescan_status` reports. One per app: a second run never starts while one runs.
#[derive(Default)]
pub struct Progress {
    pub running: AtomicBool,
    pub done: AtomicUsize,
    pub total: AtomicUsize,
}

impl Progress {
    /// Claims the run; `false` when one is already running.
    pub fn start(&self) -> bool {
        let claimed = self
            .running
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok();
        if claimed {
            self.done.store(0, Ordering::SeqCst);
            self.total.store(0, Ordering::SeqCst);
        }
        claimed
    }
}

/// Ends the run when dropped, so `running` goes back to false on every path, panics included.
pub struct Running(pub Arc<Progress>);

impl Drop for Running {
    fn drop(&mut self) {
        self.0.running.store(false, Ordering::SeqCst);
    }
}

/// Every `recording.<ext>` directly inside a folder of the recordings directory or its
/// `Archive/`, as the canonical path the pipeline stores rows by. Sorted.
pub fn candidates(recordings_dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for (dir, top) in [
        (recordings_dir.to_path_buf(), true),
        (recordings_dir.join("Archive"), false),
    ] {
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(e) => {
                if e.kind() != ErrorKind::NotFound {
                    tracing::warn!(kind = ?e.kind(), "cannot read a recordings folder");
                }
                continue;
            }
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let skipped = name.to_string_lossy().starts_with('.') || (top && name == "Archive");
            if skipped || !entry.path().is_dir() {
                continue;
            }
            let Some(recording) = recording_in(&entry.path()) else {
                continue;
            };
            if let Ok(canonical) = check_recording(recordings_dir, &recording) {
                found.push(canonical);
            }
        }
    }
    found.sort();
    found.dedup();
    found
}

/// The first `recording.<ext>` in `RECORDING_EXTENSIONS` order.
fn recording_in(folder: &Path) -> Option<PathBuf> {
    RECORDING_EXTENSIONS
        .iter()
        .map(|ext| folder.join(format!("recording.{ext}")))
        .find(|path| path.is_file())
}

/// The row a folder's files describe. Never fails: a file that can't be read counts as
/// missing, logged by its name only.
pub fn read_folder(recording: &Path, jobs: &[Job]) -> RunResult {
    let folder = recording.parent().unwrap_or(recording);
    let meta = Meta::load(&Meta::path_for(recording)).unwrap_or_else(|e| {
        tracing::warn!(kind = store::error_kind(&e), "meta.json ignored");
        None
    });
    let summary = read_optional(&folder.join("summary.md"));
    let transcript_md = read_optional(&folder.join("transcript.md"));
    let ctx = context::load(&folder.join("context.json"));
    let recorded_at = meta
        .as_ref()
        .map(|m| m.recorded_at.clone())
        .or_else(|| recorded_at_for(recording).ok())
        .unwrap_or_default();
    let mut result = RunResult {
        status: Status::Failed,
        failed_stage: None,
        error: None,
        title: None,
        body_md: None,
        speakers: vec![],
        language: meta.as_ref().map(|m| m.language),
        duration_s: meta
            .as_ref()
            .and_then(|m| m.duration_s)
            .map(|secs| secs.round() as i64),
        transcript: transcript_md.as_deref().map(transcript::utterance_lines),
        event: ctx
            .as_ref()
            .and_then(|c| c.calendar_event.as_ref())
            .map(pipeline::event_link),
        source: pipeline::source_of(recording, ctx.as_ref()),
        recorded_at,
        attendees: Some(
            ctx.as_ref()
                .map(context::attendee_names)
                .unwrap_or_default(),
        ),
    };
    if let Some(summary) = summary {
        result.status = Status::Done;
        result.title = meta
            .as_ref()
            .and_then(|m| m.title.clone())
            .filter(|t| !t.is_empty())
            .or_else(|| heading(&summary))
            .filter(|t| !t.is_empty());
        result.body_md = Some(summary_md::body_without_title(&summary).to_string());
        result.speakers = meta
            .iter()
            .flat_map(|m| &m.speakers)
            .map(|s| (s.label.clone(), s.name.clone(), s.evidence.clone()))
            .collect();
    } else if transcript_md.as_deref().is_some_and(says_no_speech) {
        result.status = Status::NoSpeech;
        result.title = Some(NO_SPEECH.into());
    } else {
        let (stage, error) = failure(recording, meta.as_ref(), jobs);
        result.failed_stage = stage;
        result.error = Some(error);
    }
    result
}

fn read_optional(path: &Path) -> Option<String> {
    match fs::read_to_string(path) {
        Ok(text) => Some(text),
        Err(e) => {
            if e.kind() != ErrorKind::NotFound {
                let file = path.file_name().unwrap_or_default().to_string_lossy();
                tracing::warn!(kind = ?e.kind(), "{file} ignored");
            }
            None
        }
    }
}

/// The text of the summary's leading `# <title>` line.
fn heading(summary: &str) -> Option<String> {
    let first = summary.lines().next()?;
    first.strip_prefix("# ").map(|t| t.trim().to_string())
}

fn says_no_speech(transcript_md: &str) -> bool {
    !transcript::has_speech(transcript_md) && transcript_md.lines().any(|l| l == NO_SPEECH)
}

/// Why a folder failed: its newest `meta.json` run with an error, else its newest job with
/// one, else a fixed message.
fn failure(recording: &Path, meta: Option<&Meta>, jobs: &[Job]) -> (Option<String>, String) {
    let from_meta = meta.and_then(|m| m.runs.iter().rev().find_map(|r| r.error.clone()));
    // `jobs` is newest first.
    let from_job = || {
        jobs.iter()
            .filter(|job| job.recording == recording)
            .find_map(|job| job.error.clone())
    };
    match from_meta.or_else(from_job) {
        Some(error) => split_failure(&error),
        None => (None, UNKNOWN_FAILURE.into()),
    }
}

/// `"<stage>: <reason>"` (as the pipeline writes it) into its stage and reason; text without a
/// stage prefix stays whole.
fn split_failure(error: &str) -> (Option<String>, String) {
    if let Some((prefix, reason)) = error.split_once(": ")
        && serde_json::from_value::<Stage>(prefix.into()).is_ok()
    {
        return (Some(prefix.into()), reason.into());
    }
    (None, error.into())
}

/// Reads every candidate folder into `store`, then removes the rows whose recording is gone.
/// A folder being processed (lock held) is left to the pipeline. A store write error skips
/// that folder and leaves `backfill_done` unset; the run reports the first one.
pub fn run(
    config: &Config,
    jobs: &JobStore,
    store: &Store,
    progress: &Progress,
) -> anyhow::Result<()> {
    let found = candidates(&config.recordings_dir);
    progress.total.store(found.len(), Ordering::SeqCst);
    // Listed once: `JobStore::list` reads every job file.
    let jobs = jobs.list(usize::MAX).unwrap_or_else(|e| {
        tracing::warn!(kind = store::error_kind(&e), "cannot read the jobs");
        Vec::new()
    });
    let mut first_error = None;
    for recording in &found {
        if !lock::is_held(&Lock::path_for(recording))
            && let Err(e) = store.merge_run(recording, &read_folder(recording, &jobs))
        {
            tracing::warn!(
                kind = store::error_kind(&e),
                "backfill: a folder not stored"
            );
            first_error.get_or_insert(e);
        }
        progress.done.fetch_add(1, Ordering::SeqCst);
    }
    // After the folder pass, so a folder moved by hand is found at its new path first. An
    // unreadable recordings folder (an unmounted volume) would make every recording look gone,
    // so nothing is removed then.
    let mut removed = 0;
    let readable = fs::read_dir(&config.recordings_dir).is_ok();
    if !readable {
        tracing::warn!("recordings folder unreadable: no rows removed");
    }
    for path in store.paths()?.into_iter().filter(|_| readable) {
        if !path.is_file() {
            store.removed(&path)?;
            removed += 1;
        }
    }
    tracing::info!(folders = found.len(), removed, "backfill finished");
    match first_error {
        Some(e) => Err(e),
        None => store.set_setting(BACKFILL_DONE, "1"),
    }
}

/// FR-3's one-time fill: every row with no attendees yet gets them from its folder's
/// `context.json` (`[]` when there is none usable), then `attendees_filled` is set. A row whose
/// write fails stays null and the pass goes on; only a store that cannot list its rows stops
/// it, with the setting unset.
pub fn fill_attendees(store: &Store) -> anyhow::Result<()> {
    let rows = store.without_attendees()?;
    let (mut filled, mut failed) = (0, 0);
    for (id, recording) in &rows {
        let names = context::load(&recording.with_file_name("context.json"))
            .as_ref()
            .map(context::attendee_names)
            .unwrap_or_default();
        match store.fill_attendees(*id, &names) {
            Ok(()) => filled += 1,
            Err(e) => {
                tracing::warn!(
                    kind = store::error_kind(&e),
                    "attendee fill: a row not stored"
                );
                failed += 1;
            }
        }
    }
    tracing::info!(filled, failed, "attendee fill finished");
    store.set_setting(store::ATTENDEES_FILLED, "true")
}

/// The store at `path`; a corrupt or newer file (`OpenError`) is first moved aside, with its
/// `-wal` and `-shm` files, and a fresh store created in its place. Any other open error is
/// returned and nothing moves.
pub fn open_or_rebuild(path: &Path) -> anyhow::Result<Store> {
    match Store::open(path) {
        Err(e) if e.downcast_ref::<OpenError>().is_some() => {
            tracing::warn!(
                kind = store::error_kind(&e),
                "rebuilding the notes database"
            );
            move_aside(path, &stamp(&now_utc()))?;
            Store::open(path)
        }
        opened => opened,
    }
}

/// `2026-11-14T01:32:05Z` as `20261114T013205Z`.
fn stamp(utc: &str) -> String {
    utc.replace(['-', ':'], "")
}

/// Renames `<path>-wal`, `<path>-shm` and `<path>` to `<path>.broken-<stamp>[-wal|-shm]`.
/// The database goes last: if a move fails it stays put, so the next rescan still sees it
/// broken and no old WAL is left beside a fresh file. Refuses to replace an existing file: a
/// moved-aside store is never lost.
fn move_aside(path: &Path, stamp: &str) -> anyhow::Result<()> {
    let sides = ["-wal", "-shm", ""];
    let target = |side: &str| suffixed(path, &format!(".broken-{stamp}{side}"));
    if let Some(taken) = sides.iter().map(|side| target(side)).find(|to| to.exists()) {
        anyhow::bail!(
            "{} already exists; press Rescan folders again in a second",
            taken.display()
        );
    }
    for side in sides {
        let from = suffixed(path, side);
        match fs::rename(&from, target(side)) {
            Err(e) if e.kind() == ErrorKind::NotFound && !side.is_empty() => {}
            moved => moved.with_context(|| format!("cannot move {} aside", from.display()))?,
        }
    }
    Ok(())
}

fn suffixed(path: &Path, suffix: &str) -> PathBuf {
    let mut name = OsString::from(path.as_os_str());
    name.push(suffix);
    PathBuf::from(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failure_splits_only_at_a_stage_name() {
        assert_eq!(
            split_failure("transcribing: boom"),
            (Some("transcribing".into()), "boom".into())
        );
        assert_eq!(
            split_failure("summarising: claude: exit 1"),
            (Some("summarising".into()), "claude: exit 1".into())
        );
        assert_eq!(
            split_failure("network: down"),
            (None, "network: down".into())
        );
        assert_eq!(split_failure("boom"), (None, "boom".into()));
    }

    #[test]
    fn the_stamp_is_compact_utc() {
        assert_eq!(stamp("2026-11-14T01:32:05Z"), "20261114T013205Z");
    }

    #[test]
    fn the_heading_is_the_first_line_only() {
        assert_eq!(heading("# Standup\n\nWe met.\n"), Some("Standup".into()));
        assert_eq!(heading("We met.\n# Not a title\n"), None);
        assert_eq!(heading(""), None);
    }

    #[test]
    fn no_speech_needs_the_line_and_no_utterance() {
        assert!(says_no_speech("# Transcript\n\nNo speech detected\n"));
        assert!(!says_no_speech(
            "No speech detected\n[00:00:01] Speaker 0: hi\n"
        ));
        assert!(!says_no_speech("# Transcript\n\n"));
    }

    #[test]
    fn the_side_files_move_with_the_database_and_nothing_is_replaced() {
        let dir =
            std::env::temp_dir().join(format!("meeting-desktop-aside-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let db = dir.join("notes.sqlite");
        for (side, text) in [("", "db"), ("-wal", "wal"), ("-shm", "shm")] {
            fs::write(suffixed(&db, side), text).unwrap();
        }
        move_aside(&db, "20261114T013205Z").unwrap();
        let read = |name: &str| fs::read_to_string(dir.join(name)).ok();
        let moved = (
            read("notes.sqlite.broken-20261114T013205Z"),
            read("notes.sqlite.broken-20261114T013205Z-wal"),
            read("notes.sqlite.broken-20261114T013205Z-shm"),
            db.exists(),
        );
        // Only the database itself is required; a second move in the same second refuses.
        fs::write(&db, "db2").unwrap();
        let again = move_aside(&db, "20261114T013205Z");
        let kept = (
            read("notes.sqlite"),
            read("notes.sqlite.broken-20261114T013205Z"),
        );
        fs::remove_dir_all(&dir).unwrap();
        assert_eq!(
            moved,
            (
                Some("db".into()),
                Some("wal".into()),
                Some("shm".into()),
                false
            )
        );
        assert!(again.unwrap_err().to_string().contains("already exists"));
        assert_eq!(kept, (Some("db2".into()), Some("db".into())));
    }

    #[test]
    fn a_second_start_is_refused_until_the_run_ends() {
        let progress = Arc::new(Progress::default());
        progress.done.store(7, Ordering::SeqCst);
        assert!(progress.start());
        assert_eq!(progress.done.load(Ordering::SeqCst), 0);
        assert!(!progress.start());
        drop(Running(progress.clone()));
        assert!(!progress.running.load(Ordering::SeqCst));
        assert!(progress.start());
    }
}
