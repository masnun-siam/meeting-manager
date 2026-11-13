//! Notes: the list, one note, edits, reruns and opening a note outside the app. Every command
//! reads the store through `Env`; none takes a path or a command line from the UI.

mod markdown;

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};

use meeting::Language;
use meeting::jobs::Stage;
use meeting::lock::{self, Lock};
use meeting::store::{self, Edit, EditError, ListRow, Row, Status, Store};
use meeting::{native, spawn, vault};
use serde::Deserialize;
use serde_json::{Value, json};
use tauri::State;

use crate::App;
use crate::env::Env;
use crate::reply;

use markdown::{render_html, shown_transcript};

/// What a command answers: the `ok` reply, or the error reply it stopped at.
type Reply = Result<Value, Value>;

/// The notes commands' outside effects, swapped for fakes in tests.
pub struct Notes {
    /// Opens a file or folder (`/usr/bin/open`).
    pub open: fn(&Path) -> io::Result<()>,
    /// Starts `<exe> process <recording>` detached.
    pub spawn: fn(&Path, &Path, Option<Language>, Option<&str>) -> io::Result<()>,
}

impl Notes {
    pub fn new(_env: &Env) -> Notes {
        Notes {
            open: native::open_with_system,
            spawn: spawn::process_detached,
        }
    }
}

/// The fields `note_update` may change, applied in this order.
#[derive(Default)]
pub struct Update {
    pub title: Option<String>,
    pub body_md: Option<String>,
    pub my_notes: Option<String>,
    pub speaker: Option<SpeakerName>,
}

#[derive(Deserialize)]
pub struct SpeakerName {
    pub label: String,
    pub name: String,
}

impl Update {
    fn edits(self) -> impl Iterator<Item = Edit> {
        [
            self.title.map(Edit::Title),
            self.body_md.map(Edit::Body),
            self.my_notes.map(Edit::MyNotes),
            self.speaker.map(|s| Edit::Speaker {
                label: s.label,
                name: s.name,
            }),
        ]
        .into_iter()
        .flatten()
    }
}

#[derive(Deserialize, Clone, Copy, Debug, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum OpenWhat {
    Obsidian,
    Folder,
}

#[tauri::command(rename_all = "snake_case")]
pub fn notes_list(app: State<'_, App>, query: Option<String>) -> Value {
    answer(list(&app.env, query.as_deref()))
}

#[tauri::command(rename_all = "snake_case")]
pub fn note_get(app: State<'_, App>, id: i64) -> Value {
    answer(get(&app.env, id))
}

#[tauri::command(rename_all = "snake_case")]
pub fn note_update(
    app: State<'_, App>,
    id: i64,
    title: Option<String>,
    body_md: Option<String>,
    my_notes: Option<String>,
    speaker: Option<SpeakerName>,
) -> Value {
    let fields = Update {
        title,
        body_md,
        my_notes,
        speaker,
    };
    answer(update(&app.env, id, fields))
}

#[tauri::command(rename_all = "snake_case")]
pub fn note_rerun(app: State<'_, App>, id: i64, replace_edits: bool) -> Value {
    answer(rerun(&app.env, &app.notes, id, replace_edits))
}

#[tauri::command(rename_all = "snake_case")]
pub fn note_open(app: State<'_, App>, id: i64, what: OpenWhat) -> Value {
    answer(open(&app.env, &app.notes, id, what))
}

fn answer(reply: Reply) -> Value {
    reply.unwrap_or_else(|error| error)
}

/// Every row (or the rows matching `query`), with the job stage of the ones being processed.
pub fn list(env: &Env, query: Option<&str>) -> Reply {
    let store = open_store(env)?;
    let rows = store.list(query).map_err(store_failed)?;
    let stages = if rows.iter().any(|r| r.status == Status::Processing) {
        job_stages(env)
    } else {
        HashMap::new()
    };
    let notes: Vec<Value> = rows.iter().map(|row| list_json(row, &stages)).collect();
    Ok(reply::ok(json!({ "notes": notes })))
}

pub fn get(env: &Env, id: i64) -> Reply {
    let store = open_store(env)?;
    let row = find(&store, id)?;
    Ok(reply::ok(json!({ "note": note_json(&row) })))
}

/// Applies the edits, then rewrites the vault note of a `done` row. A vault failure is part of
/// the `ok` reply: the store edit stands and the UI offers a Retry, which sends only `id`.
pub fn update(env: &Env, id: i64, fields: Update) -> Reply {
    let store = open_store(env)?;
    find(&store, id)?;
    for edit in fields.edits() {
        store.edit(id, edit).map_err(|e| edit_failed(id, e))?;
    }
    let row = find(&store, id)?;
    let (vault, vault_error) = if row.status == Status::Done {
        match rewrite_vault(env, &store, &row) {
            Ok(()) => ("ok", None),
            Err(e) => {
                tracing::warn!(id, kind = store::error_kind(&e), "vault note not updated");
                ("failed", Some(vault_reason(&e)))
            }
        }
    } else {
        // D39: "My notes" of a note that is not done lives in the store only.
        ("ok", None)
    };
    let row = find(&store, id)?;
    Ok(reply::ok(json!({
        "note": note_json(&row),
        "vault": vault,
        "vault_error": vault_error,
    })))
}

/// Runs `meeting process` on the note's recording again. Every refusal comes before any edit
/// mark is cleared.
pub fn rerun(env: &Env, notes: &Notes, id: i64, replace_edits: bool) -> Reply {
    let store = open_store(env)?;
    let row = find(&store, id)?;
    let recording = &row.recording_path;
    if !recording.is_file() {
        return Err(reply::err(
            "recording_missing",
            "the recording file no longer exists",
        ));
    }
    if lock::is_held(&Lock::path_for(recording)) {
        return Err(reply::err(
            "already_processing",
            "this recording is already being processed",
        ));
    }
    let exe = meeting_path(env).map_err(|reason| {
        reply::err(
            "tool_missing",
            format!("Processing tool not found ({reason}); run `meeting install`"),
        )
    })?;
    if replace_edits {
        store.clear_edits(id).map_err(store_failed)?;
    }
    (notes.spawn)(&exe, recording, row.language, None).map_err(|e| {
        tracing::warn!(id, kind = ?e.kind(), "cannot start meeting process");
        reply::err(
            "tool_missing",
            format!("Processing tool could not start ({e}); run `meeting install`"),
        )
    })?;
    tracing::info!(id, replace_edits, "rerun started");
    Ok(reply::ok(json!({})))
}

/// Opens the vault note (D37) or the meeting folder.
pub fn open(env: &Env, notes: &Notes, id: i64, what: OpenWhat) -> Reply {
    let store = open_store(env)?;
    let row = find(&store, id)?;
    let target = match what {
        OpenWhat::Folder => row
            .recording_path
            .parent()
            .filter(|dir| dir.is_dir())
            .map(Path::to_path_buf),
        OpenWhat::Obsidian => vault_note(env, &store, &row),
    };
    let Some(target) = target else {
        return Err(reply::err(
            "missing_file",
            "the file or folder no longer exists",
        ));
    };
    (notes.open)(&target).map_err(|e| {
        tracing::warn!(id, kind = ?e.kind(), "cannot open");
        reply::err("open_failed", format!("cannot open it: {e}"))
    })?;
    Ok(reply::ok(json!({})))
}

fn open_store(env: &Env) -> Result<Store, Value> {
    env.store().map_err(store_failed)
}

fn store_failed(e: anyhow::Error) -> Value {
    tracing::warn!(kind = store::error_kind(&e), "notes database failed");
    reply::err("store_unavailable", format!("{e:#}"))
}

fn find(store: &Store, id: i64) -> Result<Row, Value> {
    store
        .get(id)
        .map_err(store_failed)?
        .ok_or_else(|| reply::err("unknown_note", format!("no note with id {id}")))
}

fn edit_failed(id: i64, e: EditError) -> Value {
    match e {
        EditError::UnknownNote => reply::err("unknown_note", format!("no note with id {id}")),
        EditError::NotEditable | EditError::UnknownSpeaker => reply::err(
            "not_editable",
            "this field can't be changed: the note is not done, the value is empty or the \
             speaker is unknown",
        ),
        EditError::TooLarge => reply::err("too_large", "My notes is limited to 1 MB"),
        EditError::Store(e) => store_failed(e),
    }
}

/// The stage name of the newest job of each recording. A job directory that can't be read
/// counts as no jobs: the list still shows.
fn job_stages(env: &Env) -> HashMap<PathBuf, Stage> {
    let jobs = env.jobs.list(usize::MAX).unwrap_or_else(|e| {
        tracing::warn!(kind = store::error_kind(&e), "cannot read the jobs");
        Vec::new()
    });
    let mut stages = HashMap::new();
    // `list` is newest first, so the first job of a recording wins.
    for job in jobs {
        stages.entry(job.recording).or_insert(job.stage);
    }
    stages
}

fn list_json(row: &ListRow, stages: &HashMap<PathBuf, Stage>) -> Value {
    let (stage, error) = match row.status {
        Status::Processing => (
            stages
                .get(&row.recording_path)
                .map(|s| s.name().to_string()),
            None,
        ),
        Status::Failed => (row.failed_stage.clone(), row.error.clone()),
        Status::Done | Status::NoSpeech => (None, None),
    };
    let folder_name = row
        .recording_path
        .parent()
        .and_then(Path::file_name)
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    json!({
        "id": row.id,
        "title": row.title,
        "folder_name": folder_name,
        "recorded_at": row.recorded_at,
        "duration_s": row.duration_s,
        "language": row.language,
        "source": row.source,
        "status": row.status,
        "stage": stage,
        "error": error,
        "recording_missing": !row.recording_path.is_file(),
    })
}

/// The row as the note view needs it: its fields, the transcript with current names, the
/// summary as safe HTML, and whether the recording is still there.
fn note_json(row: &Row) -> Value {
    // Store paths are read from TEXT columns, so they are UTF-8 and always serialise.
    let mut note = serde_json::to_value(row).expect("a store row serialises");
    note["transcript"] = json!(
        row.transcript
            .as_deref()
            .map(|t| shown_transcript(t, &row.speakers))
    );
    note["body_html"] = json!(row.body_md.as_deref().map(render_html).unwrap_or_default());
    note["recording_missing"] = json!(!row.recording_path.is_file());
    note
}

fn rewrite_vault(env: &Env, store: &Store, row: &Row) -> anyhow::Result<()> {
    let config = env.config().map_err(anyhow::Error::msg)?;
    let path = vault::rewrite_from_row(&config.vault_dir, &config.vault_folder, row)?;
    store.set_vault_note_path(row.id, &path)
}

/// Why the vault note was not written, without the note's file name: that holds the title. A
/// file system error is the root cause of a context naming that file, so only the step is said.
fn vault_reason(e: &anyhow::Error) -> String {
    let root = e.root_cause();
    if root.downcast_ref::<io::Error>().is_some() {
        format!("cannot write the vault note: {root}")
    } else {
        root.to_string()
    }
}

/// The stored vault note if it is still there, else the note whose `recording` frontmatter is
/// this recording (saved for next time).
fn vault_note(env: &Env, store: &Store, row: &Row) -> Option<PathBuf> {
    if let Some(stored) = row.vault_note_path.as_ref().filter(|p| p.is_file()) {
        return Some(stored.clone());
    }
    let config = env.config().ok()?;
    let found = vault::find_existing(
        &config.vault_dir.join(&config.vault_folder),
        &row.recording_path,
    )?;
    if let Err(e) = store.set_vault_note_path(row.id, &found) {
        tracing::warn!(
            id = row.id,
            kind = store::error_kind(&e),
            "cannot save the vault note path"
        );
    }
    Some(found)
}

pub(crate) fn meeting_path(env: &Env) -> Result<PathBuf, String> {
    let exe = env
        .config()?
        .meeting_path
        .ok_or("meeting_path is not set in config.toml")?;
    spawn::check_tool(&exe).map_err(|rule| format!("meeting_path is {rule}"))?;
    Ok(exe)
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use meeting::config::Config;
    use meeting::jobs::Job;
    use meeting::store::{RunResult, RunStart, Source};

    use super::*;

    const TOO_LARGE: usize = 1_048_577;

    /// A temp home with a store, a vault and a recordings folder; removed on drop.
    struct Fixture {
        env: Env,
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.env.home);
        }
    }

    impl Fixture {
        fn new(name: &str) -> Fixture {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let home = std::env::temp_dir().join(format!(
                "meeting-desktop-notes-{name}-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(home.join("Vault")).unwrap();
            let fixture = Fixture { env: Env::at(home) };
            fixture.write_config(None);
            fixture
        }

        fn write_config(&self, meeting_path: Option<PathBuf>) {
            let home = &self.env.home;
            let config = Config {
                recordings_dir: home.join("Recordings"),
                vault_dir: home.join("Vault"),
                vault_folder: "Meetings".into(),
                ffmpeg_path: "/opt/homebrew/bin/ffmpeg".into(),
                claude_path: "/usr/local/bin/claude".into(),
                extension_id: "abcdefghijklmnopabcdefghijklmnop".into(),
                meeting_path,
            };
            config
                .save_to(&home.join(".config/meeting-manager/config.toml"))
                .unwrap();
        }

        fn store(&self) -> Store {
            self.env.store().unwrap()
        }

        fn vault_folder(&self) -> PathBuf {
            self.env.home.join("Vault/Meetings")
        }

        fn vault_notes(&self) -> Vec<PathBuf> {
            let Ok(entries) = fs::read_dir(self.vault_folder()) else {
                return Vec::new();
            };
            let mut notes: Vec<PathBuf> = entries.map(|e| e.unwrap().path()).collect();
            notes.sort();
            notes
        }

        /// A recording file in its own meeting folder.
        fn recording(&self, folder: &str) -> PathBuf {
            let dir = self.env.home.join("Recordings").join(folder);
            fs::create_dir_all(&dir).unwrap();
            let path = dir.join("recording.webm");
            fs::write(&path, b"audio").unwrap();
            path
        }

        /// A row for a new recording in `folder`, merged from `result`.
        fn add(&self, folder: &str, result: RunResult) -> (i64, PathBuf) {
            let recording = self.recording(folder);
            let id = self.store().merge_run(&recording, &result).unwrap().id;
            (id, recording)
        }

        fn add_done(&self, folder: &str) -> (i64, PathBuf) {
            self.add(folder, done("Standup", "2026-10-19T14:30"))
        }

        fn row(&self, id: i64) -> Row {
            self.store().get(id).unwrap().unwrap()
        }

        fn corrupt_store(&self) {
            fs::create_dir_all(self.env.store_path.parent().unwrap()).unwrap();
            fs::write(&self.env.store_path, "this is not a database, not at all").unwrap();
        }

        /// An executable stand-in for the `meeting` binary, named in the config.
        fn install_tool(&self) -> PathBuf {
            let tool = self.env.home.join("bin/meeting");
            fs::create_dir_all(tool.parent().unwrap()).unwrap();
            fs::write(&tool, "#!/bin/sh\n").unwrap();
            fs::set_permissions(&tool, fs::Permissions::from_mode(0o755)).unwrap();
            self.write_config(Some(tool.clone()));
            tool
        }
    }

    fn done(title: &str, recorded_at: &str) -> RunResult {
        let speaker =
            |label: &str, name: Option<&str>| (label.into(), name.map(Into::into), "e".into());
        RunResult {
            status: Status::Done,
            failed_stage: None,
            error: None,
            title: Some(title.into()),
            body_md: Some(
                "## Action items\n\n- Rahim (inferred) ships it.\n- Speaker 10 reviews.\n".into(),
            ),
            speakers: vec![
                speaker("Speaker 1", Some("Rahim")),
                speaker("Speaker 10", None),
            ],
            language: Some(Language::En),
            duration_s: Some(60),
            transcript: Some(
                "[00:00:01] Speaker 1: I am Speaker 1.\n[00:00:02] Speaker 10: Thanks.\n".into(),
            ),
            event: None,
            source: Source::Browser,
            recorded_at: recorded_at.into(),
        }
    }

    fn failed(recorded_at: &str) -> RunResult {
        RunResult {
            status: Status::Failed,
            failed_stage: Some("summarising".into()),
            error: Some("claude failed".into()),
            title: None,
            body_md: None,
            speakers: vec![],
            transcript: None,
            ..done("", recorded_at)
        }
    }

    thread_local! {
        static OPENED: RefCell<Vec<PathBuf>> = const { RefCell::new(Vec::new()) };
        static SPAWNED: RefCell<Vec<(PathBuf, PathBuf, Option<Language>)>> =
            const { RefCell::new(Vec::new()) };
    }

    fn record_open(path: &Path) -> io::Result<()> {
        OPENED.with(|o| o.borrow_mut().push(path.to_path_buf()));
        Ok(())
    }

    fn fail_open(_: &Path) -> io::Result<()> {
        Err(io::Error::other("open exited with 1"))
    }

    fn record_spawn(
        exe: &Path,
        recording: &Path,
        language: Option<Language>,
        job_id: Option<&str>,
    ) -> io::Result<()> {
        assert_eq!(job_id, None);
        SPAWNED.with(|s| {
            s.borrow_mut()
                .push((exe.to_path_buf(), recording.to_path_buf(), language))
        });
        Ok(())
    }

    fn fail_spawn(_: &Path, _: &Path, _: Option<Language>, _: Option<&str>) -> io::Result<()> {
        Err(io::Error::from(io::ErrorKind::PermissionDenied))
    }

    fn fakes() -> Notes {
        Notes {
            open: record_open,
            spawn: record_spawn,
        }
    }

    fn opened() -> Vec<PathBuf> {
        OPENED.with(|o| o.take())
    }

    fn spawned() -> Vec<(PathBuf, PathBuf, Option<Language>)> {
        SPAWNED.with(|s| s.take())
    }

    fn code(reply: Reply) -> String {
        let error = reply.expect_err("an error reply");
        assert_eq!(error["ok"], false, "{error}");
        error["code"].as_str().unwrap().to_string()
    }

    fn title(text: &str) -> Update {
        Update {
            title: Some(text.into()),
            ..Update::default()
        }
    }

    fn my_notes(text: String) -> Update {
        Update {
            my_notes: Some(text),
            ..Update::default()
        }
    }

    fn rename(label: &str, name: &str) -> Update {
        Update {
            speaker: Some(SpeakerName {
                label: label.into(),
                name: name.into(),
            }),
            ..Update::default()
        }
    }

    fn ids(reply: &Value) -> Vec<i64> {
        reply["notes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|n| n["id"].as_i64().unwrap())
            .collect()
    }

    #[test]
    fn the_list_is_newest_first_and_a_query_filters_it() {
        let f = Fixture::new("list");
        let (older, _) = f.add(
            "2026-10-18 0900",
            done("Alpha planning", "2026-10-18T09:00"),
        );
        let (newer, _) = f.add("2026-10-19 1430", done("Beta review", "2026-10-19T14:30"));

        let all = list(&f.env, None).unwrap();
        assert_eq!(all["ok"], true);
        assert_eq!(ids(&all), [newer, older]);
        assert_eq!(
            all["notes"][0],
            json!({
                "id": newer,
                "title": "Beta review",
                "folder_name": "2026-10-19 1430",
                "recorded_at": "2026-10-19T14:30",
                "duration_s": 60,
                "language": "en",
                "source": "browser",
                "status": "done",
                "stage": null,
                "error": null,
                "recording_missing": false,
            })
        );

        let found = list(&f.env, Some("beta")).unwrap();
        assert_eq!(ids(&found), [newer]);
        let blank = list(&f.env, Some("  ")).unwrap();
        assert_eq!(ids(&blank), [newer, older]);
    }

    #[test]
    fn the_list_marks_a_missing_recording_per_row() {
        let f = Fixture::new("missing");
        let (gone, recording) = f.add("2026-10-18 0900", done("A", "2026-10-18T09:00"));
        let (kept, _) = f.add("2026-10-19 1430", done("B", "2026-10-19T14:30"));
        fs::remove_file(&recording).unwrap();

        let reply = list(&f.env, None).unwrap();
        let missing: Vec<(i64, bool)> = reply["notes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|n| (n["id"].as_i64().unwrap(), n["recording_missing"] == true))
            .collect();
        assert_eq!(missing, [(kept, false), (gone, true)]);
    }

    #[test]
    fn a_processing_row_shows_the_newest_jobs_stage_and_a_failed_row_its_reason() {
        let f = Fixture::new("stage");
        let store = f.store();
        let start = |folder: &str, at: &str| {
            let recording = f.recording(folder);
            let run = RunStart {
                recording: &recording,
                source: Source::Desktop,
                recorded_at: at,
                language: None,
            };
            (store.start_run(&run).unwrap(), recording)
        };
        let (with_job, recording) = start("2026-10-19 1430", "2026-10-19T14:30");
        let (without_job, _) = start("2026-10-19 1500", "2026-10-19T15:00");
        let (failed_id, _) = f.add("2026-10-18 0900", failed("2026-10-18T09:00"));
        let job = |id: &str, stage, started_at: &str| Job {
            job_id: id.into(),
            recording: recording.clone(),
            title: None,
            stage,
            error: None,
            started_at: started_at.into(),
            updated_at: started_at.into(),
            summary_path: None,
        };
        f.env
            .jobs
            .write(&job("old", Stage::Failed, "2026-10-19T08:00:00Z"))
            .unwrap();
        f.env
            .jobs
            .write(&job("new", Stage::Summarising, "2026-10-19T08:30:00Z"))
            .unwrap();

        let reply = list(&f.env, None).unwrap();
        let row = |id: i64| {
            reply["notes"]
                .as_array()
                .unwrap()
                .iter()
                .find(|n| n["id"] == id)
                .unwrap()
                .clone()
        };
        assert_eq!(row(with_job)["status"], "processing");
        assert_eq!(row(with_job)["stage"], "summarising");
        assert_eq!(row(with_job)["error"], Value::Null);
        assert_eq!(row(with_job)["source"], "desktop");
        assert_eq!(row(with_job)["title"], Value::Null);
        assert_eq!(row(without_job)["stage"], Value::Null);
        assert_eq!(row(failed_id)["status"], "failed");
        assert_eq!(row(failed_id)["stage"], "summarising");
        assert_eq!(row(failed_id)["error"], "claude failed");
    }

    #[test]
    fn an_unopenable_store_is_store_unavailable_on_every_command() {
        let f = Fixture::new("corrupt");
        f.corrupt_store();
        let notes = fakes();
        let replies = [
            list(&f.env, None),
            get(&f.env, 1),
            update(&f.env, 1, Update::default()),
            rerun(&f.env, &notes, 1, false),
            open(&f.env, &notes, 1, OpenWhat::Folder),
        ];
        for reply in replies {
            let error = reply.unwrap_err();
            assert_eq!(error["code"], "store_unavailable", "{error}");
            let message = error["message"].as_str().unwrap();
            assert!(
                message.contains("Notes database can't be opened"),
                "{message}"
            );
        }
        assert!(opened().is_empty());
        assert!(spawned().is_empty());
    }

    #[test]
    fn get_returns_the_note_with_safe_html_and_current_names() {
        let f = Fixture::new("get");
        let mut result = done("Standup", "2026-10-19T14:30");
        result.body_md = Some("Hello <img src=x onerror=alert(1)> **team**".into());
        let (id, recording) = f.add("2026-10-19 1430", result);

        let note = get(&f.env, id).unwrap()["note"].clone();
        assert_eq!(note["id"], id);
        assert_eq!(note["title"], "Standup");
        assert_eq!(note["recording_path"], recording.to_str().unwrap());
        assert_eq!(note["status"], "done");
        assert_eq!(note["my_notes"], "");
        assert_eq!(note["vault_note_path"], Value::Null);
        assert_eq!(note["speakers"][0]["label"], "Speaker 1");
        assert_eq!(note["speakers"][0]["name"], "Rahim");
        assert_eq!(note["speakers"][0]["name_edited"], false);
        assert_eq!(
            note["body_html"],
            "<p>Hello &lt;img src=x onerror=alert(1)&gt; <strong>team</strong></p>\n"
        );
        assert_eq!(
            note["transcript"],
            "[00:00:01] Rahim (inferred): I am Speaker 1.\n[00:00:02] Speaker 10: Thanks.\n"
        );
        assert_eq!(note["recording_missing"], false);

        fs::remove_file(&recording).unwrap();
        assert_eq!(get(&f.env, id).unwrap()["note"]["recording_missing"], true);
        assert_eq!(code(get(&f.env, id + 1)), "unknown_note");
    }

    #[test]
    fn a_note_without_a_body_has_empty_html() {
        let f = Fixture::new("nobody");
        let (id, _) = f.add("2026-10-18 0900", failed("2026-10-18T09:00"));
        let note = get(&f.env, id).unwrap()["note"].clone();
        assert_eq!(note["body_html"], "");
        assert_eq!(note["transcript"], Value::Null);
    }

    #[test]
    fn a_title_edit_renames_the_vault_note() {
        let f = Fixture::new("title");
        let (id, _) = f.add_done("2026-10-19 1430");
        let first = update(&f.env, id, Update::default()).unwrap();
        assert_eq!(first["vault"], "ok");
        let before = f.vault_folder().join("2026-10-19 1430 Standup.md");
        assert_eq!(f.vault_notes(), vec![before.clone()]);
        assert_eq!(first["note"]["vault_note_path"], before.to_str().unwrap());

        let reply = update(&f.env, id, title("Weekly sync")).unwrap();
        let after = f.vault_folder().join("2026-10-19 1430 Weekly sync.md");
        assert_eq!(reply["ok"], true);
        assert_eq!(reply["vault"], "ok");
        assert_eq!(reply["vault_error"], Value::Null);
        assert_eq!(reply["note"]["title"], "Weekly sync");
        assert_eq!(reply["note"]["title_edited"], true);
        assert_eq!(reply["note"]["vault_note_path"], after.to_str().unwrap());
        assert_eq!(f.vault_notes(), vec![after.clone()]);
        assert!(
            fs::read_to_string(&after)
                .unwrap()
                .contains("# Weekly sync")
        );
    }

    #[test]
    fn renaming_a_speaker_updates_the_body_transcript_and_vault_note() {
        let f = Fixture::new("rename");
        let (id, _) = f.add_done("2026-10-19 1430");
        let reply = update(&f.env, id, rename("Speaker 1", "Rahim Uddin")).unwrap();
        assert_eq!(reply["vault"], "ok");
        let note = &reply["note"];
        let body = note["body_md"].as_str().unwrap();
        assert!(body.contains("- Rahim Uddin ships it."), "{body}");
        assert!(body.contains("- Speaker 10 reviews."), "{body}");
        assert_eq!(
            note["transcript"],
            "[00:00:01] Rahim Uddin: I am Speaker 1.\n[00:00:02] Speaker 10: Thanks.\n"
        );
        assert_eq!(note["speakers"][1]["name"], Value::Null);

        let text = fs::read_to_string(f.row(id).vault_note_path.unwrap()).unwrap();
        assert!(
            text.contains("speakers: [\"Rahim Uddin\", \"Speaker 10\"]"),
            "{text}"
        );
        assert!(text.contains("- Rahim Uddin ships it."), "{text}");
    }

    #[test]
    fn a_note_that_is_not_done_takes_only_my_notes_and_writes_no_vault_note() {
        let f = Fixture::new("notdone");
        let (id, _) = f.add("2026-10-18 0900", failed("2026-10-18T09:00"));
        assert_eq!(code(update(&f.env, id, title("New"))), "not_editable");
        let body = Update {
            body_md: Some("x".into()),
            ..Update::default()
        };
        assert_eq!(code(update(&f.env, id, body)), "not_editable");
        assert_eq!(
            code(update(&f.env, id, rename("Speaker 1", "R"))),
            "not_editable"
        );
        assert_eq!(f.row(id).title, None);

        let reply = update(&f.env, id, my_notes("call back".into())).unwrap();
        assert_eq!(reply["vault"], "ok");
        assert_eq!(reply["vault_error"], Value::Null);
        assert_eq!(reply["note"]["my_notes"], "call back");
        assert_eq!(f.row(id).my_notes, "call back");
        assert!(f.vault_notes().is_empty());
    }

    #[test]
    fn bad_edits_are_refused_and_change_nothing() {
        let f = Fixture::new("refused");
        let (id, _) = f.add_done("2026-10-19 1430");
        assert_eq!(code(update(&f.env, id, title("  "))), "not_editable");
        assert_eq!(
            code(update(&f.env, id, rename("Speaker 9", "X"))),
            "not_editable"
        );
        assert_eq!(
            code(update(&f.env, id, rename("Speaker 1", ""))),
            "not_editable"
        );
        assert_eq!(
            code(update(&f.env, id, my_notes("a".repeat(TOO_LARGE)))),
            "too_large"
        );
        assert_eq!(code(update(&f.env, id + 1, title("X"))), "unknown_note");
        let row = f.row(id);
        assert_eq!(row.title.as_deref(), Some("Standup"));
        assert_eq!(row.my_notes, "");
        assert!(!row.speakers[0].name_edited);
        assert!(f.vault_notes().is_empty());
    }

    #[test]
    fn my_notes_at_the_cap_is_accepted() {
        let f = Fixture::new("cap");
        let (id, _) = f.add_done("2026-10-19 1430");
        let reply = update(&f.env, id, my_notes("a".repeat(TOO_LARGE - 1))).unwrap();
        assert_eq!(reply["vault"], "ok");
        let text = fs::read_to_string(f.row(id).vault_note_path.unwrap()).unwrap();
        assert!(text.contains("## My notes"));
    }

    #[test]
    fn a_vault_failure_keeps_the_edit_and_a_plain_resend_retries() {
        let f = Fixture::new("vaultfail");
        let (id, _) = f.add_done("2026-10-19 1430");
        let vault = f.env.home.join("Vault");
        fs::remove_dir_all(&vault).unwrap();

        let reply = update(&f.env, id, title("Secret plans")).unwrap();
        assert_eq!(reply["ok"], true);
        assert_eq!(reply["vault"], "failed");
        let reason = reply["vault_error"].as_str().unwrap();
        assert!(reason.contains("does not exist"), "{reason}");
        assert!(!reason.contains("Secret plans"), "{reason}");
        assert_eq!(reply["note"]["title"], "Secret plans");
        assert_eq!(f.row(id).title.as_deref(), Some("Secret plans"));

        fs::create_dir_all(&vault).unwrap();
        let retry = update(&f.env, id, Update::default()).unwrap();
        assert_eq!(retry["vault"], "ok");
        assert_eq!(retry["vault_error"], Value::Null);
        assert_eq!(
            f.vault_notes(),
            [f.vault_folder().join("2026-10-19 1430 Secret plans.md")]
        );
    }

    #[test]
    fn an_unwritable_vault_folder_names_no_title() {
        let f = Fixture::new("readonly");
        let (id, _) = f.add_done("2026-10-19 1430");
        update(&f.env, id, Update::default()).unwrap();
        fs::set_permissions(f.vault_folder(), fs::Permissions::from_mode(0o555)).unwrap();
        let reply = update(&f.env, id, title("Secret plans")).unwrap();
        fs::set_permissions(f.vault_folder(), fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(reply["vault"], "failed");
        let reason = reply["vault_error"].as_str().unwrap();
        assert!(
            reason.starts_with("cannot write the vault note: Permission denied"),
            "{reason}"
        );
        assert!(!reason.contains("Secret plans"), "{reason}");
        assert!(!reason.contains("Standup"), "{reason}");
    }

    #[test]
    fn an_unreadable_config_is_a_vault_failure() {
        let f = Fixture::new("noconfig");
        let (id, _) = f.add_done("2026-10-19 1430");
        fs::remove_dir_all(f.env.home.join(".config")).unwrap();
        let reply = update(&f.env, id, Update::default()).unwrap();
        assert_eq!(reply["vault"], "failed");
        assert_eq!(
            reply["vault_error"],
            "meeting is not configured; run `meeting install`"
        );
    }

    /// A done note with an edited title, for the rerun tests.
    fn edited(f: &Fixture) -> (i64, PathBuf) {
        let (id, recording) = f.add_done("2026-10-19 1430");
        update(&f.env, id, title("Mine")).unwrap();
        update(&f.env, id, my_notes("keep".into())).unwrap();
        (id, recording)
    }

    #[test]
    fn rerun_refusals_come_in_order_and_clear_nothing() {
        let f = Fixture::new("refusals");
        let (id, recording) = edited(&f);
        let notes = fakes();
        let refuse = |expected: &str| {
            assert_eq!(code(rerun(&f.env, &notes, id, true)), expected);
            assert!(f.row(id).title_edited, "{expected}");
            assert!(spawned().is_empty(), "{expected}");
        };
        assert_eq!(code(rerun(&f.env, &notes, id + 1, true)), "unknown_note");

        // The tool is missing too, so each check must win over the ones after it.
        fs::remove_file(&recording).unwrap();
        let lock = Lock::path_for(&recording);
        fs::write(&lock, std::process::id().to_string()).unwrap();
        refuse("recording_missing");
        fs::write(&recording, b"audio").unwrap();
        refuse("already_processing");
        fs::remove_file(&lock).unwrap();
        refuse("tool_missing");

        let tool = f.install_tool();
        fs::set_permissions(&tool, fs::Permissions::from_mode(0o644)).unwrap();
        let error = rerun(&f.env, &notes, id, true).unwrap_err();
        assert_eq!(error["code"], "tool_missing");
        assert_eq!(
            error["message"],
            "Processing tool not found (meeting_path is not executable); run `meeting install`"
        );
        assert!(f.row(id).title_edited);
    }

    #[test]
    fn a_failed_spawn_is_tool_missing() {
        let f = Fixture::new("spawnfail");
        let (id, _) = edited(&f);
        f.install_tool();
        let notes = Notes {
            spawn: fail_spawn,
            ..fakes()
        };
        let error = rerun(&f.env, &notes, id, false).unwrap_err();
        assert_eq!(error["code"], "tool_missing");
        assert!(
            error["message"]
                .as_str()
                .unwrap()
                .ends_with("run `meeting install`"),
            "{error}"
        );
    }

    #[test]
    fn rerun_replacing_edits_clears_the_marks_but_not_my_notes() {
        let f = Fixture::new("replace");
        let (id, recording) = edited(&f);
        let tool = f.install_tool();
        let reply = rerun(&f.env, &fakes(), id, true).unwrap();
        assert_eq!(reply, json!({"ok": true}));
        assert_eq!(spawned(), [(tool, recording, Some(Language::En))]);
        let row = f.row(id);
        assert!(!row.title_edited);
        assert_eq!(row.my_notes, "keep");
    }

    #[test]
    fn rerun_keeping_edits_clears_nothing() {
        let f = Fixture::new("keep");
        let (id, recording) = edited(&f);
        let tool = f.install_tool();
        rerun(&f.env, &fakes(), id, false).unwrap();
        assert_eq!(spawned(), [(tool, recording, Some(Language::En))]);
        assert!(f.row(id).title_edited);
    }

    #[test]
    fn open_folder_opens_the_meeting_folder() {
        let f = Fixture::new("folder");
        let (id, recording) = f.add_done("2026-10-19 1430");
        let reply = open(&f.env, &fakes(), id, OpenWhat::Folder).unwrap();
        assert_eq!(reply, json!({"ok": true}));
        assert_eq!(opened(), [recording.parent().unwrap()]);

        fs::remove_dir_all(recording.parent().unwrap()).unwrap();
        assert_eq!(
            code(open(&f.env, &fakes(), id, OpenWhat::Folder)),
            "missing_file"
        );
        assert_eq!(
            code(open(&f.env, &fakes(), id + 1, OpenWhat::Folder)),
            "unknown_note"
        );
        assert!(opened().is_empty());
    }

    #[test]
    fn open_obsidian_uses_the_stored_note() {
        let f = Fixture::new("stored");
        let (id, _) = f.add_done("2026-10-19 1430");
        update(&f.env, id, Update::default()).unwrap();
        let note = f.row(id).vault_note_path.unwrap();
        open(&f.env, &fakes(), id, OpenWhat::Obsidian).unwrap();
        assert_eq!(opened(), [note]);
    }

    #[test]
    fn open_obsidian_finds_the_note_by_recording_and_saves_it() {
        let f = Fixture::new("lookup");
        let (id, _) = f.add_done("2026-10-19 1430");
        let vault = f.env.home.join("Vault");
        let note = vault::rewrite_from_row(&vault, "Meetings", &f.row(id)).unwrap();
        assert_eq!(f.row(id).vault_note_path, None);

        open(&f.env, &fakes(), id, OpenWhat::Obsidian).unwrap();
        assert_eq!(opened(), vec![note.clone()]);
        assert_eq!(f.row(id).vault_note_path, Some(note.clone()));

        // A stored path that went stale falls back to the lookup.
        let moved = note.with_file_name("2026-10-19 1430 Renamed.md");
        fs::rename(&note, &moved).unwrap();
        open(&f.env, &fakes(), id, OpenWhat::Obsidian).unwrap();
        assert_eq!(opened(), [moved]);
    }

    #[test]
    fn open_obsidian_without_a_note_is_missing_file() {
        let f = Fixture::new("nonote");
        let (id, _) = f.add_done("2026-10-19 1430");
        assert_eq!(
            code(open(&f.env, &fakes(), id, OpenWhat::Obsidian)),
            "missing_file"
        );
        assert!(opened().is_empty());
    }

    #[test]
    fn a_failed_open_is_open_failed() {
        let f = Fixture::new("openfail");
        let (id, _) = f.add_done("2026-10-19 1430");
        let notes = Notes {
            open: fail_open,
            ..fakes()
        };
        let error = open(&f.env, &notes, id, OpenWhat::Folder).unwrap_err();
        assert_eq!(error["code"], "open_failed");
        assert_eq!(error["message"], "cannot open it: open exited with 1");
    }

    #[test]
    fn the_real_dependencies_are_the_tools() {
        let f = Fixture::new("real");
        let notes = Notes::new(&f.env);
        assert_eq!(notes.open as usize, native::open_with_system as usize);
        assert_eq!(notes.spawn as usize, spawn::process_detached as usize);
    }

    #[test]
    fn edits_apply_in_order_and_the_request_shapes_deserialise() {
        let speaker: SpeakerName =
            serde_json::from_value(json!({"label": "Speaker 1", "name": "R"})).unwrap();
        let fields = Update {
            my_notes: Some("m".into()),
            speaker: Some(speaker),
            title: Some("t".into()),
            ..Update::default()
        };
        let edits: Vec<Edit> = fields.edits().collect();
        assert_eq!(
            edits,
            [
                Edit::Title("t".into()),
                Edit::MyNotes("m".into()),
                Edit::Speaker {
                    label: "Speaker 1".into(),
                    name: "R".into()
                }
            ]
        );
        let what: OpenWhat = serde_json::from_value(json!("obsidian")).unwrap();
        assert_eq!(what, OpenWhat::Obsidian);
        assert!(serde_json::from_value::<OpenWhat>(json!("shell")).is_err());
    }
}
