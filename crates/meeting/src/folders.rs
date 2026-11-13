//! Archive and Trash for one recording folder, by recording path. The native host and the
//! desktop app both call these, so their rules cannot drift.

use std::fmt::Display;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::config::{Config, check_recording};
use crate::jobs::JobStore;
use crate::lock::{self, Lock};
use crate::meta::Meta;
use crate::store::{self, Store};
use crate::vault;

/// Why a move was not done, or not finished: a fixed code and a message for the user.
#[derive(Debug, PartialEq)]
pub struct Refusal {
    pub code: &'static str,
    pub message: String,
}

fn refusal(code: &'static str, message: impl Display) -> Refusal {
    Refusal {
        code,
        message: message.to_string(),
    }
}

/// A store failure is logged by kind and never fails the move.
fn store_moved(store: Option<&Store>, from: &Path, to: &Path) {
    if let Some(s) = store
        && let Err(e) = s.moved(from, to)
    {
        tracing::warn!("store moved failed: {}", store::error_kind(&e));
    }
}

fn store_removed(store: Option<&Store>, recording: &Path) {
    if let Some(s) = store
        && let Err(e) = s.removed(recording)
    {
        tracing::warn!("store removed failed: {}", store::error_kind(&e));
    }
}

/// The checks Archive and Trash share: the canonical path of a valid
/// `<name>/recording.<ext>`. `Ok(None)` means the recording is already gone, so its jobs and
/// its row were cleared and nothing is left to move; `failed` is the code when that fails.
fn locate(
    config: &Config,
    jobs: &JobStore,
    store: Option<&Store>,
    recording: &Path,
    failed: &'static str,
) -> Result<Option<PathBuf>, Refusal> {
    if !recording.is_file() {
        jobs.remove_for(recording)
            .map_err(|e| refusal(failed, format!("{e:#}")))?;
        store_removed(store, recording);
        return Ok(None);
    }
    check_recording(&config.recordings_dir, recording)
        .map(Some)
        .map_err(|reason| refusal("invalid_path", reason))
}

/// Moves a recording's folder to `Archive/`, repoints its store row, `meta.json` and vault
/// note, and removes its jobs. The folder moves first, so a later failure leaves it archived
/// and the jobs in place. Returns the new recording path, or `None` when the recording was
/// already gone.
pub fn archive(
    config: &Config,
    jobs: &JobStore,
    store: Option<&Store>,
    recording: &Path,
) -> Result<Option<PathBuf>, Refusal> {
    let Some(canonical) = locate(config, jobs, store, recording, "archive_failed")? else {
        return Ok(None);
    };
    // `check_recording` guarantees a parent folder with a parent of its own.
    let folder = canonical.parent().expect("recording is inside a folder");
    let root = folder
        .parent()
        .expect("folder is inside the recordings dir");
    // Not `root`'s name: the recordings directory may itself be called `Archive`.
    let top = fs::canonicalize(&config.recordings_dir).map_err(|e| {
        refusal(
            "archive_failed",
            format!("cannot read {}: {e}", config.recordings_dir.display()),
        )
    })?;
    if root != top {
        return Err(refusal(
            "invalid_path",
            format!("{} is already archived", folder.display()),
        ));
    }
    if lock::is_held(&Lock::path_for(&canonical)) {
        return Err(refusal("already_processing", "already processing"));
    }
    let name = folder
        .file_name()
        .expect("folder has a name")
        .to_string_lossy();
    let archive_dir = root.join("Archive");
    let step = |what: &str, e: &dyn Display| refusal("archive_failed", format!("{what}: {e}"));
    fs::create_dir_all(&archive_dir).map_err(|e| {
        step(
            "move",
            &format!("cannot create {}: {e}", archive_dir.display()),
        )
    })?;
    // `rename` onto an empty directory succeeds on macOS, so a free name is chosen first.
    let target = (1..)
        .map(|n| match n {
            1 => archive_dir.join(&*name),
            n => archive_dir.join(format!("{name}-{n}")),
        })
        .find(|t| !t.exists())
        .expect("an unbounded range has a free name");
    fs::rename(folder, &target).map_err(|e| {
        step(
            "move",
            &format!(
                "cannot move {} to {}: {e}",
                folder.display(),
                target.display()
            ),
        )
    })?;
    let new_recording = target.join(canonical.file_name().expect("recording is a file"));
    // Right after the move: whatever fails below, the row points at the archived folder.
    store_moved(store, &canonical, &new_recording);
    let meta_path = target.join("meta.json");
    Meta::load(&meta_path)
        .and_then(|meta| {
            meta.map_or(Ok(()), |mut m| {
                m.recording = new_recording.clone();
                m.save(&meta_path)
            })
        })
        .map_err(|e| step("meta.json", &format!("{}: {e:#}", meta_path.display())))?;
    vault::repoint_note(
        &config.vault_dir,
        &config.vault_folder,
        &canonical,
        &new_recording,
        &target.join("transcript.md"),
    )
    .map_err(|e| step("vault note", &format!("{e:#}")))?;
    jobs.remove_for(recording)
        .map_err(|e| step("jobs", &format!("{}: {e:#}", jobs.dir.display())))?;
    Ok(Some(new_recording))
}

/// Moves a recording's folder to the macOS Trash with `trash`, then removes its store row
/// and its jobs; the vault note stays.
pub fn trash(
    config: &Config,
    jobs: &JobStore,
    store: Option<&Store>,
    trash: fn(&Path) -> io::Result<()>,
    recording: &Path,
) -> Result<(), Refusal> {
    let Some(canonical) = locate(config, jobs, store, recording, "trash_failed")? else {
        return Ok(());
    };
    if lock::is_held(&Lock::path_for(&canonical)) {
        return Err(refusal("already_processing", "already processing"));
    }
    let folder = canonical.parent().expect("recording is inside a folder");
    trash(folder).map_err(|e| refusal("trash_failed", e))?;
    store_removed(store, &canonical);
    jobs.remove_for(recording).map_err(|e| {
        refusal(
            "trash_failed",
            format!("moved to the Trash, but cannot remove its jobs: {e:#}"),
        )
    })
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;
    use crate::Language;
    use crate::jobs::{Job, Stage};
    use crate::store::{RunStart, Source};

    thread_local! {
        static TRASHED: RefCell<Vec<PathBuf>> = const { RefCell::new(Vec::new()) };
    }

    fn record_trash(path: &Path) -> io::Result<()> {
        TRASHED.with(|t| t.borrow_mut().push(path.to_path_buf()));
        Ok(())
    }

    fn fail_trash(_: &Path) -> io::Result<()> {
        Err(io::Error::other("no trash"))
    }

    fn trashed() -> Vec<PathBuf> {
        TRASHED.with(|t| t.take())
    }

    /// A temp dir with a config (`rec/`, `vault/`) and a job store under it.
    fn setup(name: &str) -> (PathBuf, Config, JobStore) {
        let dir = std::env::temp_dir().join(format!(
            "meeting-test-{}-folders-{name}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let config = Config {
            recordings_dir: dir.join("rec"),
            vault_dir: dir.join("vault"),
            vault_folder: "Meetings".into(),
            ffmpeg_path: "/ffmpeg".into(),
            claude_path: "/claude".into(),
            extension_id: "ext".into(),
            meeting_path: None,
        };
        let jobs = JobStore {
            dir: dir.join("jobs"),
        };
        (dir, config, jobs)
    }

    fn job(jobs: &JobStore, id: &str, recording: &Path) {
        jobs.write(&Job {
            job_id: id.into(),
            recording: recording.to_path_buf(),
            title: None,
            stage: Stage::Done,
            error: None,
            started_at: "2026-10-19T14:30:00Z".into(),
            updated_at: "2026-10-19T14:30:00Z".into(),
            summary_path: None,
        })
        .unwrap();
    }

    /// A folder `rec/<name>/recording.webm` plus a job for it.
    fn foldered(dir: &Path, jobs: &JobStore, name: &str, job_id: &str) -> PathBuf {
        foldered_as(dir, jobs, name, "recording.webm", job_id)
    }

    fn foldered_as(dir: &Path, jobs: &JobStore, name: &str, file: &str, job_id: &str) -> PathBuf {
        let folder = dir.join("rec").join(name);
        fs::create_dir_all(&folder).unwrap();
        fs::write(folder.join(file), "x").unwrap();
        let recording = fs::canonicalize(folder.join(file)).unwrap();
        job(jobs, job_id, &recording);
        recording
    }

    /// A flat file in `rec/` and a file outside it, each with a job: both are invalid paths.
    fn flat_and_outside(dir: &Path, jobs: &JobStore) -> [PathBuf; 2] {
        fs::create_dir_all(dir.join("rec")).unwrap();
        [
            ("flat", dir.join("rec/flat.webm")),
            ("out", dir.join("outside.webm")),
        ]
        .map(|(id, path)| {
            fs::write(&path, "x").unwrap();
            let recording = fs::canonicalize(path).unwrap();
            job(jobs, id, &recording);
            recording
        })
    }

    fn job_ids(jobs: &JobStore) -> Vec<String> {
        jobs.list(10)
            .unwrap()
            .into_iter()
            .map(|j| j.job_id)
            .collect()
    }

    fn meta(recording: &Path) -> Meta {
        Meta {
            recording: recording.to_path_buf(),
            language: Language::En,
            recorded_at: "2026-10-19T14:30".into(),
            duration_s: None,
            title: None,
            speakers: vec![],
            runs: vec![],
        }
    }

    fn open_store(dir: &Path) -> Store {
        Store::open(&dir.join("store/notes.sqlite")).unwrap()
    }

    fn seed(store: &Store, recording: &Path) -> i64 {
        store
            .start_run(&RunStart {
                recording,
                source: Source::Browser,
                recorded_at: "2026-10-19T14:30",
                language: None,
            })
            .unwrap()
    }

    #[test]
    fn archive_moves_the_folder_and_rewrites_meta_note_and_jobs() {
        let (dir, config, jobs) = setup("archive");
        let old = foldered(&dir, &jobs, "m", "a");
        foldered(&dir, &jobs, "m", "b");
        foldered(&dir, &jobs, "other", "c");
        let meta = meta(&old);
        meta.save(&Meta::path_for(&old)).unwrap();
        let transcript = old.with_file_name("transcript.md");
        fs::create_dir_all(dir.join("vault")).unwrap();
        let note = vault::write_note(
            &dir.join("vault"),
            "Meetings",
            &vault::Note {
                recorded_at: "2026-10-19T14:30",
                title: "T",
                language: Language::En,
                speakers: &[],
                recording: &old,
                transcript: &transcript,
                body: "body\n",
                meeting_title: Some("Weekly sync"),
                attendees: &["Rahim".into()],
                calendar: Some(vault::CalendarOutcome::NoMatch),
            },
        )
        .unwrap();
        let result = archive(&config, &jobs, None, &old);
        let new = fs::canonicalize(dir.join("rec/Archive/m/recording.webm")).unwrap();
        let moved = Meta::load(&new.with_file_name("meta.json"))
            .unwrap()
            .unwrap();
        let text = fs::read_to_string(&note).unwrap();
        let remaining = job_ids(&jobs);
        let old_gone = !dir.join("rec/m").exists();
        fs::remove_dir_all(&dir).unwrap();
        assert_eq!(result, Ok(Some(new.clone())));
        assert!(old_gone);
        assert_eq!(
            moved,
            Meta {
                recording: new.clone(),
                ..meta
            }
        );
        assert!(text.contains(&format!("recording: {:?}\n", new.to_string_lossy())));
        assert!(text.contains(&format!(
            "transcript: {:?}\n",
            new.with_file_name("transcript.md").to_string_lossy()
        )));
        assert!(text.ends_with(
            "\nmeeting_title: \"Weekly sync\"\nattendees: [\"Rahim\"]\ncalendar: \"no_match\"\n---\n\nbody\n"
        ));
        assert_eq!(remaining, ["c"]);
    }

    #[test]
    fn archive_keeps_the_recording_file_name() {
        let (dir, config, jobs) = setup("archive-m4a");
        let old = foldered_as(&dir, &jobs, "m", "recording.m4a", "a");
        meta(&old).save(&Meta::path_for(&old)).unwrap();
        fs::create_dir_all(dir.join("vault")).unwrap();
        let note = vault::write_note(
            &dir.join("vault"),
            "Meetings",
            &vault::Note {
                recorded_at: "2026-10-19T14:30",
                title: "T",
                language: Language::En,
                speakers: &[],
                recording: &old,
                transcript: &old.with_file_name("transcript.md"),
                body: "body\n",
                meeting_title: None,
                attendees: &[],
                calendar: None,
            },
        )
        .unwrap();
        let result = archive(&config, &jobs, None, &old);
        let new = fs::canonicalize(dir.join("rec/Archive/m/recording.m4a")).unwrap();
        let moved = Meta::load(&new.with_file_name("meta.json"))
            .unwrap()
            .unwrap();
        let text = fs::read_to_string(&note).unwrap();
        let left = job_ids(&jobs);
        fs::remove_dir_all(&dir).unwrap();
        assert_eq!(result, Ok(Some(new.clone())));
        assert_eq!(moved.recording, new);
        assert!(text.contains("Archive/m/recording.m4a"));
        assert!(!text.contains("recording.webm"));
        assert!(left.is_empty());
    }

    #[test]
    fn archive_picks_a_free_name() {
        let (dir, config, jobs) = setup("archive-name");
        let recording = foldered(&dir, &jobs, "m", "a");
        fs::create_dir_all(dir.join("rec/Archive/m")).unwrap();
        fs::create_dir_all(dir.join("rec/Archive/m-2")).unwrap();
        let result = archive(&config, &jobs, None, &recording);
        let landed = dir.join("rec/Archive/m-3/recording.webm").is_file();
        fs::remove_dir_all(&dir).unwrap();
        assert!(result.is_ok());
        assert!(landed);
    }

    #[test]
    fn archive_refuses_already_archived_flat_and_outside_paths() {
        let (dir, config, jobs) = setup("archive-refuse");
        let archived = foldered(&dir, &jobs, "Archive/done", "arch");
        let [flat, outside] = flat_and_outside(&dir, &jobs);
        let refusals: Vec<_> = [archived, flat, outside]
            .iter()
            .map(|r| archive(&config, &jobs, None, r).unwrap_err())
            .collect();
        let kept = job_ids(&jobs).len();
        let still_there = dir.join("rec/Archive/done/recording.webm").is_file();
        fs::remove_dir_all(&dir).unwrap();
        assert!(refusals.iter().all(|r| r.code == "invalid_path"));
        assert!(refusals[0].message.contains("already archived"));
        assert_eq!(kept, 3);
        assert!(still_there);
    }

    #[test]
    fn archive_refuses_a_held_lock() {
        let (dir, config, jobs) = setup("archive-lock");
        let recording = foldered(&dir, &jobs, "m", "a");
        let _lock = Lock::acquire(&Lock::path_for(&recording)).unwrap();
        let result = archive(&config, &jobs, None, &recording);
        let stayed = recording.is_file();
        let kept = jobs.read("a").unwrap().is_some();
        fs::remove_dir_all(&dir).unwrap();
        assert_eq!(
            result,
            Err(refusal("already_processing", "already processing"))
        );
        assert!(stayed && kept);
    }

    #[test]
    fn archive_of_a_missing_recording_clears_its_jobs() {
        let (dir, config, jobs) = setup("archive-missing");
        let recording = foldered(&dir, &jobs, "m", "a");
        fs::remove_file(&recording).unwrap();
        let result = archive(&config, &jobs, None, &recording);
        let left = job_ids(&jobs).len();
        fs::remove_dir_all(&dir).unwrap();
        assert_eq!(result, Ok(None));
        assert_eq!(left, 0);
    }

    #[test]
    fn archive_without_meta_or_note_succeeds() {
        let (dir, config, jobs) = setup("archive-bare");
        let recording = foldered(&dir, &jobs, "m", "a");
        let result = archive(&config, &jobs, None, &recording);
        let landed = dir.join("rec/Archive/m/recording.webm").is_file();
        fs::remove_dir_all(&dir).unwrap();
        assert!(result.is_ok());
        assert!(landed);
    }

    #[test]
    fn archive_failure_after_the_move_keeps_the_jobs() {
        let (dir, config, jobs) = setup("archive-fail");
        let recording = foldered(&dir, &jobs, "m", "a");
        fs::create_dir(Meta::path_for(&recording)).unwrap();
        let refused = archive(&config, &jobs, None, &recording).unwrap_err();
        let landed = dir.join("rec/Archive/m/recording.webm").is_file();
        let kept = jobs.read("a").unwrap().is_some();
        fs::remove_dir_all(&dir).unwrap();
        assert_eq!(refused.code, "archive_failed");
        assert!(refused.message.contains("meta.json"));
        assert!(landed && kept);
    }

    #[test]
    fn archive_works_when_the_recordings_dir_is_named_archive() {
        let (dir, mut config, jobs) = setup("archive-named");
        config.recordings_dir = dir.join("Archive");
        let folder = dir.join("Archive/m");
        fs::create_dir_all(&folder).unwrap();
        fs::write(folder.join("recording.webm"), "x").unwrap();
        let recording = fs::canonicalize(folder.join("recording.webm")).unwrap();
        job(&jobs, "a", &recording);
        let result = archive(&config, &jobs, None, &recording);
        let landed = dir.join("Archive/Archive/m/recording.webm").is_file();
        fs::remove_dir_all(&dir).unwrap();
        assert!(result.is_ok());
        assert!(landed);
    }

    #[test]
    fn archive_moves_context_json_with_the_folder() {
        let (dir, config, jobs) = setup("archive-ctx");
        let old = foldered(&dir, &jobs, "m", "a");
        fs::write(old.with_file_name("context.json"), "{}").unwrap();
        let result = archive(&config, &jobs, None, &old);
        let moved = dir.join("rec/Archive/m/context.json").is_file();
        fs::remove_dir_all(&dir).unwrap();
        assert!(result.is_ok());
        assert!(moved);
    }

    #[test]
    fn trash_moves_the_folder_and_clears_its_jobs() {
        let (dir, config, jobs) = setup("trash");
        let recording = foldered(&dir, &jobs, "m", "a");
        foldered(&dir, &jobs, "m", "b");
        foldered(&dir, &jobs, "other", "c");
        fs::create_dir_all(dir.join("vault")).unwrap();
        let note = dir.join("vault/note.md");
        fs::write(&note, "keep").unwrap();
        trashed();
        let result = trash(&config, &jobs, None, record_trash, &recording);
        let calls = trashed();
        let remaining = job_ids(&jobs);
        let kept_note = fs::read_to_string(&note).unwrap();
        let expected = recording.parent().unwrap().to_path_buf();
        fs::remove_dir_all(&dir).unwrap();
        assert_eq!(result, Ok(()));
        assert_eq!(calls, [expected]);
        assert_eq!(remaining, ["c"]);
        assert_eq!(kept_note, "keep");
    }

    #[test]
    fn trash_accepts_an_archived_recording() {
        let (dir, config, jobs) = setup("trash-archived");
        let recording = foldered(&dir, &jobs, "Archive/done", "a");
        trashed();
        let result = trash(&config, &jobs, None, record_trash, &recording);
        let calls = trashed();
        let expected = recording.parent().unwrap().to_path_buf();
        fs::remove_dir_all(&dir).unwrap();
        assert_eq!(result, Ok(()));
        assert_eq!(calls, [expected]);
    }

    #[test]
    fn trash_refuses_flat_and_outside_paths_and_a_held_lock() {
        let (dir, config, jobs) = setup("trash-refuse");
        let locked = foldered(&dir, &jobs, "m", "lock");
        let _lock = Lock::acquire(&Lock::path_for(&locked)).unwrap();
        let [flat, outside] = flat_and_outside(&dir, &jobs);
        trashed();
        let codes: Vec<_> = [flat, outside, locked]
            .iter()
            .map(|r| {
                trash(&config, &jobs, None, record_trash, r)
                    .unwrap_err()
                    .code
            })
            .collect();
        let calls = trashed();
        let kept = job_ids(&jobs).len();
        fs::remove_dir_all(&dir).unwrap();
        assert_eq!(
            codes,
            ["invalid_path", "invalid_path", "already_processing"]
        );
        assert!(calls.is_empty());
        assert_eq!(kept, 3);
    }

    #[test]
    fn trash_of_a_missing_recording_clears_its_jobs() {
        let (dir, config, jobs) = setup("trash-missing");
        let recording = foldered(&dir, &jobs, "m", "a");
        fs::remove_file(&recording).unwrap();
        trashed();
        let result = trash(&config, &jobs, None, record_trash, &recording);
        let calls = trashed();
        let left = job_ids(&jobs).len();
        fs::remove_dir_all(&dir).unwrap();
        assert_eq!(result, Ok(()));
        assert!(calls.is_empty());
        assert_eq!(left, 0);
    }

    #[test]
    fn trash_failure_returns_trash_failed_and_keeps_the_jobs() {
        let (dir, config, jobs) = setup("trash-fail");
        let recording = foldered(&dir, &jobs, "m", "a");
        let refused = trash(&config, &jobs, None, fail_trash, &recording).unwrap_err();
        let kept = jobs.read("a").unwrap().is_some();
        fs::remove_dir_all(&dir).unwrap();
        assert_eq!(refused.code, "trash_failed");
        assert!(refused.message.contains("no trash"));
        assert!(kept);
    }

    #[test]
    fn a_missing_recording_whose_jobs_cannot_be_cleared_is_refused() {
        let (dir, config, jobs) = setup("missing-jobs-fail");
        // A jobs "directory" that is a file cannot be read.
        fs::write(&jobs.dir, "x").unwrap();
        let store = open_store(&dir);
        let gone = dir.join("rec/m/recording.webm");
        let id = seed(&store, &gone);
        let archived = archive(&config, &jobs, Some(&store), &gone).unwrap_err();
        let trashed = trash(&config, &jobs, Some(&store), record_trash, &gone).unwrap_err();
        let row_kept = store.get(id).unwrap().is_some();
        fs::remove_dir_all(&dir).unwrap();
        assert_eq!(archived.code, "archive_failed");
        assert_eq!(trashed.code, "trash_failed");
        for message in [archived.message, trashed.message] {
            assert!(message.starts_with("cannot read "), "{message}");
        }
        assert!(row_kept);
    }

    #[test]
    fn archive_repoints_the_row_even_when_meta_fails_after_the_move() {
        let (dir, config, jobs) = setup("store-archive");
        let store = open_store(&dir);
        let fine = foldered(&dir, &jobs, "fine", "a");
        let broken = foldered(&dir, &jobs, "broken", "b");
        fs::create_dir(Meta::path_for(&broken)).unwrap();
        let ids = [seed(&store, &fine), seed(&store, &broken)];
        let moved = archive(&config, &jobs, Some(&store), &fine);
        let failed = archive(&config, &jobs, Some(&store), &broken).unwrap_err();
        let paths = ids.map(|id| store.get(id).unwrap().unwrap().recording_path);
        let archived = ["fine", "broken"].map(|n| {
            fs::canonicalize(dir.join(format!("rec/Archive/{n}/recording.webm"))).unwrap()
        });
        fs::remove_dir_all(&dir).unwrap();
        assert_eq!(moved, Ok(Some(archived[0].clone())));
        assert_eq!(failed.code, "archive_failed");
        assert_eq!(paths, archived);
    }

    #[test]
    fn trash_and_missing_recording_remove_the_row() {
        let (dir, config, jobs) = setup("store-trash");
        let store = open_store(&dir);
        let trashable = foldered(&dir, &jobs, "t", "a");
        let gone_a = foldered(&dir, &jobs, "ga", "b");
        let gone_t = foldered(&dir, &jobs, "gt", "c");
        let kept = foldered(&dir, &jobs, "kept", "d");
        for recording in [&trashable, &gone_a, &gone_t, &kept] {
            seed(&store, recording);
        }
        fs::remove_file(&gone_a).unwrap();
        fs::remove_file(&gone_t).unwrap();
        let results = (
            trash(&config, &jobs, Some(&store), record_trash, &trashable),
            archive(&config, &jobs, Some(&store), &gone_a),
            trash(&config, &jobs, Some(&store), record_trash, &gone_t),
        );
        let left = store.paths().unwrap();
        fs::remove_dir_all(&dir).unwrap();
        assert_eq!(results, (Ok(()), Ok(None), Ok(())));
        assert_eq!(left, [kept]);
    }

    #[test]
    fn a_refusal_leaves_the_row_alone() {
        let (dir, config, jobs) = setup("store-refusal");
        let store = open_store(&dir);
        let locked = foldered(&dir, &jobs, "m", "lock");
        let failing = foldered(&dir, &jobs, "f", "fail");
        let _lock = Lock::acquire(&Lock::path_for(&locked)).unwrap();
        let [flat, _] = flat_and_outside(&dir, &jobs);
        for recording in [&locked, &failing, &flat] {
            seed(&store, recording);
        }
        let before = store.paths().unwrap();
        let codes = [
            archive(&config, &jobs, Some(&store), &locked).unwrap_err(),
            archive(&config, &jobs, Some(&store), &flat).unwrap_err(),
            trash(&config, &jobs, Some(&store), record_trash, &locked).unwrap_err(),
            trash(&config, &jobs, Some(&store), record_trash, &flat).unwrap_err(),
            trash(&config, &jobs, Some(&store), fail_trash, &failing).unwrap_err(),
        ]
        .map(|r| r.code);
        let after = store.paths().unwrap();
        fs::remove_dir_all(&dir).unwrap();
        assert_eq!(
            codes,
            [
                "already_processing",
                "invalid_path",
                "already_processing",
                "invalid_path",
                "trash_failed"
            ]
        );
        assert_eq!(before.len(), 3);
        assert_eq!(after, before);
    }

    #[test]
    fn no_row_means_nothing_is_created() {
        let (dir, config, jobs) = setup("store-no-row");
        let store = open_store(&dir);
        let archived = foldered(&dir, &jobs, "a", "a");
        let trashable = foldered(&dir, &jobs, "t", "b");
        let gone = foldered(&dir, &jobs, "g", "c");
        fs::remove_file(&gone).unwrap();
        let results = (
            archive(&config, &jobs, Some(&store), &archived).map(drop),
            trash(&config, &jobs, Some(&store), record_trash, &trashable),
            archive(&config, &jobs, Some(&store), &gone).map(drop),
        );
        let rows = store.paths().unwrap();
        fs::remove_dir_all(&dir).unwrap();
        assert_eq!(results, (Ok(()), Ok(()), Ok(())));
        assert!(rows.is_empty());
    }

    #[test]
    fn a_failing_store_never_fails_the_move() {
        let (dir, config, jobs) = setup("store-broken");
        let store = open_store(&dir);
        let archived = foldered(&dir, &jobs, "a", "a");
        let trashable = foldered(&dir, &jobs, "t", "b");
        let gone = foldered(&dir, &jobs, "g", "c");
        for recording in [&archived, &trashable, &gone] {
            seed(&store, recording);
        }
        fs::remove_file(&gone).unwrap();
        // Every write through `store` fails from here on.
        rusqlite::Connection::open(dir.join("store/notes.sqlite"))
            .unwrap()
            .execute_batch("DROP TABLE meetings")
            .unwrap();
        let broken = (
            store.moved(&archived, &gone).is_err(),
            store.removed(&gone).is_err(),
        );
        trashed();
        let results = (
            archive(&config, &jobs, Some(&store), &archived).map(drop),
            trash(&config, &jobs, Some(&store), record_trash, &trashable),
            trash(&config, &jobs, Some(&store), record_trash, &gone),
        );
        let landed = dir.join("rec/Archive/a/recording.webm").is_file();
        let calls = trashed();
        let left = job_ids(&jobs);
        let expected = trashable.parent().unwrap().to_path_buf();
        fs::remove_dir_all(&dir).unwrap();
        assert_eq!(broken, (true, true));
        assert_eq!(results, (Ok(()), Ok(()), Ok(())));
        assert!(landed);
        assert_eq!(calls, [expected]);
        assert!(left.is_empty());
    }
}
