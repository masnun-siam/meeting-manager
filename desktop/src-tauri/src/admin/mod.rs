//! Housekeeping: rescan and backfill, health, archive, trash and the settings page's facts.
//! No command takes a path from the UI: recordings come from the store, the tool from
//! `config.toml`.

mod backfill;

use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use meeting::config::Config;
use meeting::folders::{self, Refusal};
use meeting::jobs::JobStore;
use meeting::native;
use meeting::store::{self, Store};
use serde::Serialize;
use serde_json::{Value, json};
use tauri::{AppHandle, Manager, Runtime, State};

use crate::App;
use crate::env::Env;
use crate::notes::meeting_path;
use crate::reply;

use backfill::{BACKFILL_DONE, Progress, Running};

/// D38: how long `meeting doctor` may take.
const DOCTOR_TIMEOUT: Duration = Duration::from_secs(10);

/// The fix `meeting doctor` itself gives for its `meeting_path` check.
const INSTALL: &str = "Run `meeting install`";

/// The admin commands' state and outside effects, swapped for fakes in tests.
pub struct Admin {
    progress: Arc<Progress>,
    /// Moves a folder to the macOS Trash.
    pub trash: fn(&Path) -> io::Result<()>,
    /// Runs `<exe> doctor` and returns its stdout; `Err` is a fixed reason, safe to log.
    pub doctor: fn(&Path, Duration) -> Result<String, &'static str>,
    pub doctor_timeout: Duration,
}

impl Admin {
    pub fn new(_env: &Env) -> Admin {
        Admin {
            progress: Arc::default(),
            trash: native::trash_with_system,
            doctor: run_doctor,
            doctor_timeout: DOCTOR_TIMEOUT,
        }
    }
}

/// One line of `meeting doctor`.
#[derive(Serialize, Debug, PartialEq)]
pub struct Check {
    pub name: String,
    pub ok: bool,
    pub fix: String,
}

/// Called once at startup: the first backfill, in the background, when it has never finished.
pub fn on_launch<R: Runtime>(app: &AppHandle<R>) {
    let state = app.state::<App>();
    launch_backfill(&state.env, &state.admin);
}

#[tauri::command(rename_all = "snake_case")]
pub fn rescan(app: State<'_, App>) -> Value {
    match start_rescan(&app.env, &app.admin) {
        Ok(_) => reply::ok(json!({ "started": true })),
        Err(error) => error,
    }
}

#[tauri::command(rename_all = "snake_case")]
pub fn rescan_status(app: State<'_, App>) -> Value {
    status(&app.admin)
}

#[tauri::command(rename_all = "snake_case")]
pub fn health(app: State<'_, App>) -> Value {
    health_of(&app.env, &app.admin)
}

#[tauri::command(rename_all = "snake_case")]
pub fn note_archive(app: State<'_, App>, id: i64) -> Value {
    archive(&app.env, id)
}

#[tauri::command(rename_all = "snake_case")]
pub fn note_trash(app: State<'_, App>, id: i64) -> Value {
    trash(&app.env, &app.admin, id)
}

#[tauri::command(rename_all = "snake_case")]
pub fn settings_info(app: State<'_, App>) -> Value {
    settings(&app.env)
}

/// Starts a backfill only when the store opens and has no `backfill_done`. Anything else is
/// logged by kind and left for the user's Rescan folders.
pub fn launch_backfill(env: &Env, admin: &Admin) -> Option<JoinHandle<()>> {
    let config = env
        .config()
        .inspect_err(|_| tracing::info!("no first backfill: not configured"))
        .ok()?;
    let done = env
        .store()
        .and_then(|store| store.setting(BACKFILL_DONE))
        .inspect_err(|e| tracing::warn!(kind = store::error_kind(e), "no first backfill"))
        .ok()?;
    if done.is_some() {
        return None;
    }
    start(env, admin, config, false)
}

/// `rescan`: `Ok` once a run is going (this one or one already running). Never
/// `store_unavailable`: the store is opened, and rebuilt if broken, on the run's thread.
pub fn start_rescan(env: &Env, admin: &Admin) -> Result<Option<JoinHandle<()>>, Value> {
    let config = env
        .config()
        .map_err(|message| reply::err("not_configured", message))?;
    Ok(start(env, admin, config, true))
}

/// Runs a backfill on its own thread with its own store connection; `None` when one is
/// already running or the thread could not start.
fn start(env: &Env, admin: &Admin, config: Config, rebuild: bool) -> Option<JoinHandle<()>> {
    if !admin.progress.start() {
        return None;
    }
    let running = Running(admin.progress.clone());
    let store_path = env.store_path.clone();
    let jobs = JobStore {
        dir: env.jobs.dir.clone(),
    };
    thread::Builder::new()
        .name("backfill".into())
        .spawn(move || {
            let progress = &running.0;
            let opened = if rebuild {
                backfill::open_or_rebuild(&store_path)
            } else {
                Store::open(&store_path)
            };
            if let Err(e) = opened.and_then(|store| backfill::run(&config, &jobs, &store, progress))
            {
                tracing::warn!(kind = store::error_kind(&e), "backfill stopped");
            }
        })
        .inspect_err(|e| tracing::warn!(kind = ?e.kind(), "cannot start the backfill"))
        .ok()
}

pub fn status(admin: &Admin) -> Value {
    let progress = &admin.progress;
    reply::ok(json!({
        "running": progress.running.load(Ordering::SeqCst),
        "done": progress.done.load(Ordering::SeqCst),
        "total": progress.total.load(Ordering::SeqCst),
    }))
}

/// `meeting doctor`'s checks, run by the tool so only the tool reads the Keychain (D38).
pub fn health_of(env: &Env, admin: &Admin) -> Value {
    let checks = match meeting_path(env) {
        Err(_) => vec![failing("meeting_path", INSTALL)],
        Ok(exe) => {
            let parsed = (admin.doctor)(&exe, admin.doctor_timeout).map(|out| parse_checks(&out));
            match parsed {
                Ok(checks) if !checks.is_empty() => checks,
                Ok(_) => doctor_failed("no checks in its output"),
                Err(reason) => doctor_failed(reason),
            }
        }
    };
    reply::ok(json!({ "checks": checks }))
}

fn failing(name: &str, fix: &str) -> Check {
    Check {
        name: name.into(),
        ok: false,
        fix: fix.into(),
    }
}

fn doctor_failed(reason: &'static str) -> Vec<Check> {
    tracing::warn!(reason, "meeting doctor failed");
    vec![failing(
        "doctor",
        "`meeting doctor` did not answer; run it in Terminal to see why, or run `meeting install`",
    )]
}

/// `✓ <name>` and `✗ <name>: <fix>` lines, as `meeting doctor` prints them; other lines are
/// ignored.
pub fn parse_checks(out: &str) -> Vec<Check> {
    out.lines()
        .filter_map(|line| {
            if let Some(name) = line.strip_prefix("✓ ") {
                return Some(Check {
                    name: name.trim().into(),
                    ok: true,
                    fix: String::new(),
                });
            }
            let rest = line.strip_prefix("✗ ")?;
            let (name, fix) = rest.split_once(": ").unwrap_or((rest, ""));
            Some(failing(name.trim(), fix.trim()))
        })
        .filter(|check| !check.name.is_empty())
        .collect()
}

/// `<exe> doctor`: argument list, no shell, null stdin and stderr. Killed after `timeout`.
/// A non-zero exit is normal (a check failed); only its stdout matters.
pub fn run_doctor(exe: &Path, timeout: Duration) -> Result<String, &'static str> {
    let mut child = Command::new(exe)
        .arg("doctor")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| "cannot start")?;
    let mut stdout = child.stdout.take().ok_or("no stdout")?;
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let mut out = Vec::new();
        let read = stdout.read_to_end(&mut out).map(|_| out);
        let _ = sender.send(read);
    });
    let read = receiver.recv_timeout(timeout);
    if read.is_err() {
        let _ = child.kill();
    }
    let _ = child.wait();
    match read {
        Ok(Ok(out)) => Ok(String::from_utf8_lossy(&out).into_owned()),
        Ok(Err(_)) => Err("unreadable output"),
        Err(_) => Err("timed out"),
    }
}

/// D33: the configured folders and whether the store opens. Never moves the store.
pub fn settings(env: &Env) -> Value {
    let config = match env.config() {
        Ok(config) => config,
        Err(message) => return reply::err("not_configured", message),
    };
    let store = match env.store() {
        Ok(_) => json!({ "ok": true }),
        Err(e) => json!({ "ok": false, "message": format!("{e:#}") }),
    };
    reply::ok(json!({
        "recordings_dir": config.recordings_dir,
        "vault_dir": config.vault_dir,
        "vault_folder": config.vault_folder,
        "store": store,
    }))
}

pub fn archive(env: &Env, id: i64) -> Value {
    answer(by_note(env, id, |config, store, recording| {
        folders::archive(config, &env.jobs, Some(store), recording).map(drop)
    }))
}

pub fn trash(env: &Env, admin: &Admin, id: i64) -> Value {
    answer(by_note(env, id, |config, store, recording| {
        folders::trash(config, &env.jobs, Some(store), admin.trash, recording)
    }))
}

fn answer(reply: Result<Value, Value>) -> Value {
    reply.unwrap_or_else(|error| error)
}

/// Looks up the note's recording and applies the library's by-path action (the same rules as
/// the native host). A refusal passes through unchanged (D41).
fn by_note(
    env: &Env,
    id: i64,
    action: impl FnOnce(&Config, &Store, &PathBuf) -> Result<(), Refusal>,
) -> Result<Value, Value> {
    let store = env.store().map_err(|e| {
        tracing::warn!(kind = store::error_kind(&e), "notes database failed");
        reply::err("store_unavailable", format!("{e:#}"))
    })?;
    let row = store
        .get(id)
        .map_err(|e| reply::err("store_unavailable", format!("{e:#}")))?
        .ok_or_else(|| reply::err("unknown_note", format!("no note with id {id}")))?;
    let config = env
        .config()
        .map_err(|message| reply::err("not_configured", message))?;
    action(&config, &store, &row.recording_path)
        .map_err(|refusal| reply::err(refusal.code, refusal.message))?;
    Ok(reply::ok(json!({})))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::atomic::AtomicUsize;

    use meeting::Language;
    use meeting::jobs::{Job, Stage};
    use meeting::lock::Lock;
    use meeting::meta::{Meta, Run, Speaker};
    use meeting::store::{Edit, Row, Source, Status};

    use super::*;

    /// A temp home with a config, a recordings folder and a vault; removed on drop.
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
                "meeting-desktop-admin-{name}-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(home.join("Recordings")).unwrap();
            fs::create_dir_all(home.join("Vault/Meetings")).unwrap();
            // Canonical, so paths compare equal to the ones backfill stores.
            let home = fs::canonicalize(home).unwrap();
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

        fn remove_config(&self) {
            fs::remove_file(self.env.home.join(".config/meeting-manager/config.toml")).unwrap();
        }

        fn recordings(&self) -> PathBuf {
            self.env.home.join("Recordings")
        }

        /// `recording.<ext>` in `<Recordings>/<folder>`, with nothing else.
        fn recording(&self, folder: &str, ext: &str) -> PathBuf {
            let dir = self.recordings().join(folder);
            fs::create_dir_all(&dir).unwrap();
            let path = dir.join(format!("recording.{ext}"));
            fs::write(&path, b"audio").unwrap();
            path
        }

        /// A processed meeting: recording, `meta.json`, `summary.md` and `transcript.md`.
        fn meeting(&self, folder: &str, title: &str) -> PathBuf {
            let recording = self.recording(folder, "webm");
            save_meta(&recording, Some(title), vec![]);
            write_beside(&recording, "summary.md", &format!("# {title}\n\nWe met.\n"));
            write_beside(&recording, "transcript.md", SPEECH);
            recording
        }

        fn store(&self) -> Store {
            self.env.store().unwrap()
        }

        fn row(&self, recording: &Path) -> Row {
            self.store().get_by_path(recording).unwrap().unwrap()
        }

        fn rows(&self) -> Vec<PathBuf> {
            self.store().paths().unwrap()
        }

        fn backfill(&self) -> anyhow::Result<()> {
            let config = self.env.config().unwrap();
            backfill::run(&config, &self.env.jobs, &self.store(), &Progress::default())
        }

        fn corrupt_store(&self) {
            fs::create_dir_all(self.env.store_path.parent().unwrap()).unwrap();
            fs::write(&self.env.store_path, "this is not a database, not at all").unwrap();
        }

        /// The files beside the store whose name starts with `notes.sqlite.broken-`.
        fn broken(&self) -> Vec<String> {
            let mut names: Vec<String> = fs::read_dir(self.env.store_path.parent().unwrap())
                .unwrap()
                .map(|e| e.unwrap().file_name().into_string().unwrap())
                .filter(|n| n.starts_with("notes.sqlite.broken-"))
                .collect();
            names.sort();
            names
        }

        /// An executable `meeting` stand-in running `script`, named in the config.
        fn install_tool(&self, script: &str) -> PathBuf {
            let tool = self.env.home.join("bin/meeting");
            fs::create_dir_all(tool.parent().unwrap()).unwrap();
            fs::write(&tool, format!("#!/bin/sh\n{script}\n")).unwrap();
            fs::set_permissions(&tool, fs::Permissions::from_mode(0o755)).unwrap();
            self.write_config(Some(tool.clone()));
            tool
        }
    }

    const SPEECH: &str = "# Transcript\n\n- Language: en\n- Speakers: 1\n\n\
                          [00:00:01] Speaker 0: Hello there.\n";
    const SILENT: &str = "# Transcript\n\n- Language: en\n- Speakers: 0\n\nNo speech detected\n";

    fn write_beside(recording: &Path, name: &str, text: &str) {
        fs::write(recording.with_file_name(name), text).unwrap();
    }

    fn save_meta(recording: &Path, title: Option<&str>, runs: Vec<Run>) {
        Meta {
            recording: recording.into(),
            language: Language::En,
            recorded_at: "2026-10-19T14:30".into(),
            duration_s: Some(61.6),
            title: title.map(Into::into),
            speakers: vec![Speaker {
                label: "Speaker 0".into(),
                name: Some("Rahim".into()),
                evidence: "Thanks, Rahim.".into(),
            }],
            runs,
        }
        .save(&Meta::path_for(recording))
        .unwrap();
    }

    fn run_with(error: Option<&str>) -> Run {
        Run {
            job_id: "1".into(),
            started_at: "2026-10-19T14:31:00Z".into(),
            ended_at: None,
            stage: Stage::Failed,
            error: error.map(Into::into),
        }
    }

    fn context_json(platform: &str, event: Option<Value>) -> String {
        let mut context = json!({"version": 1, "platform": platform, "url": null,
            "meeting_id": null, "tab_title": null, "meeting_title": null, "started_at_ms": 1,
            "utc_offset_min": 360, "time_zone": "Asia/Dhaka", "participants": [],
            "speaking": [], "truncated": []});
        if let Some(event) = event {
            context["calendar_event"] = event;
        }
        context.to_string()
    }

    fn event() -> Value {
        json!({"occurrence_id": "occ-1", "series_id": "ser-1", "title": "Weekly sync",
            "start": "2026-11-13T10:00:00+06:00", "end": "2026-11-13T11:00:00+06:00",
            "organizer": "Karim", "calendar": "Work", "attendees": [], "description": null})
    }

    fn admin() -> Admin {
        Admin {
            progress: Arc::default(),
            trash: record_trash,
            doctor: fake_doctor,
            doctor_timeout: Duration::from_secs(10),
        }
    }

    fn record_trash(path: &Path) -> io::Result<()> {
        // Stands in for the Trash: the folder leaves the recordings directory.
        fs::remove_dir_all(path)
    }

    fn fail_trash(_: &Path) -> io::Result<()> {
        Err(io::Error::other("trash exited with 1"))
    }

    const DOCTOR_OUT: &str = "✓ config\n✗ claude: Install Claude Code: see https://x.y\njunk\n";

    fn fake_doctor(_: &Path, _: Duration) -> Result<String, &'static str> {
        Ok(DOCTOR_OUT.into())
    }

    fn silent_doctor(_: &Path, _: Duration) -> Result<String, &'static str> {
        Ok("nothing to see\n".into())
    }

    fn slow_doctor(_: &Path, _: Duration) -> Result<String, &'static str> {
        Err("timed out")
    }

    fn unreachable_doctor(_: &Path, _: Duration) -> Result<String, &'static str> {
        panic!("the doctor must not run")
    }

    /// Runs `sql` on the store with the system's `sqlite3`: the app has no SQLite of its own.
    fn sqlite(store: &Path, sql: &str) {
        let ran = Command::new("/usr/bin/sqlite3")
            .arg(store)
            .arg(sql)
            .status()
            .unwrap();
        assert!(ran.success());
    }

    fn code(reply: &Value) -> &str {
        assert_eq!(reply["ok"], false, "{reply}");
        reply["code"].as_str().unwrap()
    }

    fn join(handle: Option<JoinHandle<()>>) {
        handle.expect("a run started").join().unwrap();
    }

    #[test]
    fn backfill_reads_every_meeting_folder_and_counts_progress() {
        let f = Fixture::new("all");
        // 30 in the recordings folder, 5 in Archive/.
        let titled: Vec<PathBuf> = (0..30)
            .map(|i| f.meeting(&format!("m{i:02}"), &format!("Meeting {i}")))
            .collect();
        let archived: Vec<PathBuf> = (0..5)
            .map(|i| f.meeting(&format!("Archive/a{i}"), &format!("Archived {i}")))
            .collect();
        // Skipped: a flat file, a recording directly in Archive/, a hidden folder, a folder
        // with no recording, and a file of another kind.
        fs::write(f.recordings().join("old.webm"), b"audio").unwrap();
        fs::write(f.recordings().join("Archive/recording.webm"), b"audio").unwrap();
        f.recording(".hidden", "webm");
        fs::create_dir_all(f.recordings().join("empty")).unwrap();
        f.recording("notes", "txt");

        let admin = admin();
        join(start_rescan(&f.env, &admin).unwrap());

        assert_eq!(f.rows().len(), 35);
        for (i, recording) in titled.iter().enumerate() {
            let row = f.row(recording);
            assert_eq!(row.title.as_deref(), Some(format!("Meeting {i}").as_str()));
            assert_eq!(row.status, Status::Done);
        }
        assert_eq!(f.row(&archived[4]).title.as_deref(), Some("Archived 4"));
        assert_eq!(
            status(&admin),
            json!({"ok": true, "running": false, "done": 35, "total": 35})
        );
        assert_eq!(
            f.store().setting(BACKFILL_DONE).unwrap().as_deref(),
            Some("1")
        );
    }

    #[test]
    fn status_is_zero_before_the_first_run() {
        assert_eq!(
            status(&admin()),
            json!({"ok": true, "running": false, "done": 0, "total": 0})
        );
    }

    #[test]
    fn a_done_row_takes_every_field_from_the_folder() {
        let f = Fixture::new("done");
        let recording = f.recording("m", "m4a");
        save_meta(&recording, None, vec![]);
        write_beside(&recording, "summary.md", "# From the heading\n\nWe met.\n");
        write_beside(&recording, "transcript.md", SPEECH);
        write_beside(
            &recording,
            "context.json",
            &context_json("desktop", Some(event())),
        );
        f.backfill().unwrap();

        let row = f.row(&recording);
        assert_eq!(row.status, Status::Done);
        assert_eq!(row.title.as_deref(), Some("From the heading"));
        assert_eq!(row.body_md.as_deref(), Some("We met.\n"));
        assert_eq!(row.speakers.len(), 1);
        assert_eq!(row.speakers[0].label, "Speaker 0");
        assert_eq!(row.speakers[0].name.as_deref(), Some("Rahim"));
        assert_eq!(row.speakers[0].evidence, "Thanks, Rahim.");
        assert_eq!(row.language, Some(Language::En));
        assert_eq!(row.duration_s, Some(62));
        assert_eq!(row.recorded_at, "2026-10-19T14:30");
        assert_eq!(
            row.transcript.as_deref(),
            Some("[00:00:01] Speaker 0: Hello there.\n")
        );
        assert_eq!(row.source, Source::Desktop);
        let link = row.event.unwrap();
        assert_eq!(
            (link.occurrence_id.as_str(), link.series_id.as_str()),
            ("occ-1", "ser-1")
        );
        assert_eq!(link.title, "Weekly sync");
        assert_eq!(link.start, "2026-11-13T10:00:00+06:00");
        assert_eq!(link.end, "2026-11-13T11:00:00+06:00");
        assert_eq!(link.calendar.as_deref(), Some("Work"));
    }

    #[test]
    fn source_and_event_follow_the_prds_rule() {
        let f = Fixture::new("source");
        let webm = f.meeting("browser", "B");
        write_beside(&webm, "context.json", &context_json("desktop", None));
        let imported = f.recording("import", "mp3");
        write_beside(&imported, "summary.md", "# I\n");
        let meet = f.recording("meet", "mp4");
        write_beside(&meet, "summary.md", "# M\n");
        write_beside(&meet, "context.json", &context_json("meet", Some(event())));
        let invalid = f.recording("invalid", "wav");
        write_beside(&invalid, "summary.md", "# V\n");
        let mut bad = event();
        bad["occurrence_id"] = json!("");
        write_beside(
            &invalid,
            "context.json",
            &context_json("desktop", Some(bad)),
        );
        let garbled = f.recording("garbled", "ogg");
        write_beside(&garbled, "summary.md", "# G\n");
        write_beside(&garbled, "context.json", "{not json");
        f.backfill().unwrap();

        assert_eq!(f.row(&webm).source, Source::Browser);
        assert_eq!(f.row(&webm).event, None);
        assert_eq!(f.row(&imported).source, Source::Import);
        assert_eq!(f.row(&meet).source, Source::Import);
        assert!(f.row(&meet).event.is_some());
        // An event failing its checks is dropped; the context still counts.
        assert_eq!(f.row(&invalid).source, Source::Desktop);
        assert_eq!(f.row(&invalid).event, None);
        // A context that fails `context::load` counts as none at all.
        assert_eq!(f.row(&garbled).source, Source::Import);
        assert_eq!(f.row(&garbled).event, None);
    }

    #[test]
    fn a_folder_with_no_speech_is_no_speech() {
        let f = Fixture::new("silent");
        let recording = f.recording("m", "webm");
        write_beside(&recording, "transcript.md", SILENT);
        f.backfill().unwrap();
        let row = f.row(&recording);
        assert_eq!(row.status, Status::NoSpeech);
        assert_eq!(row.title.as_deref(), Some("No speech detected"));
        assert_eq!(row.transcript.as_deref(), Some(""));
    }

    #[test]
    fn a_failed_folder_says_why_from_meta_then_jobs_then_a_fixed_message() {
        let f = Fixture::new("failed");
        let from_meta = f.recording("meta", "webm");
        save_meta(
            &from_meta,
            None,
            vec![
                run_with(Some("extracting: old")),
                run_with(Some("transcribing: boom")),
                run_with(None),
            ],
        );
        let from_job = f.recording("job", "webm");
        let job = |id: &str, error: Option<&str>, at: &str| Job {
            job_id: id.into(),
            recording: from_job.clone(),
            title: None,
            stage: Stage::Failed,
            error: error.map(Into::into),
            started_at: at.into(),
            updated_at: at.into(),
            summary_path: None,
        };
        f.env
            .jobs
            .write(&job(
                "1",
                Some("summarising: older"),
                "2026-10-19T14:00:00Z",
            ))
            .unwrap();
        f.env
            .jobs
            .write(&job("2", Some("writing: newest"), "2026-10-19T15:00:00Z"))
            .unwrap();
        let unknown = f.recording("nothing", "webm");
        let plain = f.recording("plain", "webm");
        save_meta(&plain, None, vec![run_with(Some("no stage here"))]);
        f.backfill().unwrap();

        let reason = |r: &Path| {
            let row = f.row(r);
            assert_eq!(row.status, Status::Failed);
            (row.failed_stage, row.error)
        };
        assert_eq!(
            reason(&from_meta),
            (Some("transcribing".into()), Some("boom".into()))
        );
        assert_eq!(
            reason(&from_job),
            (Some("writing".into()), Some("newest".into()))
        );
        let (stage, error) = reason(&unknown);
        assert_eq!(stage, None);
        assert!(error.unwrap().contains("rerun the summary"));
        assert_eq!(reason(&plain), (None, Some("no stage here".into())));
    }

    #[test]
    fn a_broken_file_does_not_stop_the_other_folders() {
        let f = Fixture::new("malformed");
        let broken = f.recording("broken", "webm");
        write_beside(&broken, "meta.json", "{not json");
        write_beside(&broken, "summary.md", "# Still here\n\nBody\n");
        let unreadable = f.meeting("unreadable", "Unreadable");
        fs::set_permissions(
            unreadable.with_file_name("summary.md"),
            fs::Permissions::from_mode(0o000),
        )
        .unwrap();
        fs::remove_file(unreadable.with_file_name("transcript.md")).unwrap();
        let fine = f.meeting("fine", "Fine");
        f.backfill().unwrap();

        let row = f.row(&broken);
        assert_eq!(
            (row.status, row.title.as_deref()),
            (Status::Done, Some("Still here"))
        );
        assert_eq!(row.language, None);
        // No summary to read and no transcript: failed, with the fixed reason.
        assert_eq!(f.row(&unreadable).status, Status::Failed);
        assert_eq!(f.row(&fine).status, Status::Done);
    }

    #[test]
    fn a_rescan_keeps_edits_and_changes_nothing_the_second_time() {
        let f = Fixture::new("edits");
        let edited = f.meeting("edited", "Original");
        let other = f.meeting("other", "Other");
        f.backfill().unwrap();
        let id = f.row(&edited).id;
        let store = f.store();
        store.edit(id, Edit::Title("Mine".into())).unwrap();
        store.edit(id, Edit::Body("My body\n".into())).unwrap();
        store
            .edit(
                id,
                Edit::Speaker {
                    label: "Speaker 0".into(),
                    name: "Karim".into(),
                },
            )
            .unwrap();
        store.edit(id, Edit::MyNotes("notes".into())).unwrap();
        // An unedited field changes on disk.
        write_beside(&other, "summary.md", "# Other\n\nNew body\n");

        f.backfill().unwrap();
        let first = f.row(&edited);
        assert_eq!(first.title.as_deref(), Some("Mine"));
        assert_eq!(first.body_md.as_deref(), Some("My body\n"));
        assert_eq!(first.speakers[0].name.as_deref(), Some("Karim"));
        assert_eq!(first.my_notes, "notes");
        assert_eq!(f.row(&other).body_md.as_deref(), Some("New body\n"));

        f.backfill().unwrap();
        let second = f.row(&edited);
        assert_eq!(f.rows().len(), 2);
        assert_eq!(
            (second.title, second.body_md, second.my_notes),
            (first.title, first.body_md, first.my_notes)
        );
    }

    #[test]
    fn a_recording_being_processed_is_left_to_the_pipeline() {
        let f = Fixture::new("locked");
        let recording = f.meeting("m", "Busy");
        let lock = Lock::acquire(&Lock::path_for(&recording)).unwrap();
        f.backfill().unwrap();
        assert!(f.store().get_by_path(&recording).unwrap().is_none());
        drop(lock);
        f.backfill().unwrap();
        assert!(f.store().get_by_path(&recording).unwrap().is_some());
    }

    #[test]
    fn rows_of_gone_recordings_are_removed_and_a_moved_folder_is_one_row() {
        let f = Fixture::new("removed");
        let kept = f.meeting("kept", "Kept");
        let gone = f.meeting("gone", "Gone");
        let moved = f.meeting("moved", "Moved");
        f.backfill().unwrap();
        fs::remove_dir_all(gone.parent().unwrap()).unwrap();
        fs::create_dir_all(f.recordings().join("Archive")).unwrap();
        fs::rename(
            moved.parent().unwrap(),
            f.recordings().join("Archive/moved"),
        )
        .unwrap();
        f.backfill().unwrap();

        let archived = f.recordings().join("Archive/moved/recording.webm");
        let mut expected = vec![kept, archived.clone()];
        expected.sort();
        let mut rows = f.rows();
        rows.sort();
        assert_eq!(rows, expected);
        assert_eq!(f.row(&archived).title.as_deref(), Some("Moved"));
    }

    #[test]
    fn the_first_backfill_runs_once_and_only_when_it_can() {
        let f = Fixture::new("launch");
        f.meeting("m", "M");
        let admin = admin();
        join(launch_backfill(&f.env, &admin));
        assert_eq!(f.rows().len(), 1);
        // Done: a later launch does not run it again.
        f.meeting("n", "N");
        assert!(launch_backfill(&f.env, &admin).is_none());
        assert_eq!(f.rows().len(), 1);

        let unconfigured = Fixture::new("launch-unconfigured");
        unconfigured.remove_config();
        assert!(launch_backfill(&unconfigured.env, &admin).is_none());

        let corrupt = Fixture::new("launch-corrupt");
        corrupt.corrupt_store();
        assert!(launch_backfill(&corrupt.env, &admin).is_none());
        assert!(corrupt.broken().is_empty());
    }

    #[test]
    fn a_store_error_leaves_backfill_done_unset() {
        let f = Fixture::new("store-error");
        f.meeting("m", "M");
        let store = f.store();
        let config = f.env.config().unwrap();
        f.meeting("refused", "R");
        // The database refuses this one folder's row: the run reports it, and the other
        // folders are still stored.
        drop(store);
        sqlite(
            &f.env.store_path,
            "CREATE TRIGGER refuse BEFORE INSERT ON meetings \
             WHEN NEW.recording_path LIKE '%/refused/%' BEGIN SELECT RAISE(ABORT, 'no'); END;",
        );
        let store = f.store();
        let result = backfill::run(&config, &f.env.jobs, &store, &Progress::default());
        assert!(result.is_err());
        assert_eq!(store.paths().unwrap().len(), 1);
        assert_eq!(store.setting(BACKFILL_DONE).unwrap(), None);
    }

    #[test]
    fn rescan_rebuilds_a_corrupt_store_after_moving_it_aside() {
        let f = Fixture::new("corrupt");
        f.meeting("m", "M");
        f.corrupt_store();
        join(start_rescan(&f.env, &admin()).unwrap());

        let broken = f.broken();
        assert_eq!(broken.len(), 1, "{broken:?}");
        assert_eq!(
            broken[0].len(),
            "notes.sqlite.broken-20261114T013205Z".len()
        );
        let dir = f.env.store_path.parent().unwrap();
        assert_eq!(
            fs::read_to_string(dir.join(&broken[0])).unwrap(),
            "this is not a database, not at all"
        );
        assert_eq!(f.rows().len(), 1);
        let mode = fs::metadata(&f.env.store_path)
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn rescan_rebuilds_a_store_from_a_newer_version() {
        let f = Fixture::new("newer");
        f.meeting("m", "M");
        drop(f.store());
        sqlite(&f.env.store_path, "PRAGMA user_version = 999;");
        assert!(f.env.store().is_err());
        join(start_rescan(&f.env, &admin()).unwrap());
        assert_eq!(f.broken().len(), 1);
        assert_eq!(f.rows().len(), 1);
    }

    #[test]
    fn another_open_error_moves_nothing() {
        let f = Fixture::new("io");
        f.meeting("m", "M");
        // A directory where the file should be: an I/O error, not `OpenError`.
        fs::create_dir_all(&f.env.store_path).unwrap();
        let admin = admin();
        join(start_rescan(&f.env, &admin).unwrap());
        assert!(f.env.store_path.is_dir());
        assert!(f.broken().is_empty());
        assert_eq!(status(&admin)["running"], false);
    }

    #[test]
    fn rescan_needs_a_config_and_never_runs_twice_at_once() {
        let f = Fixture::new("rescan");
        let admin = admin();
        f.remove_config();
        let error = start_rescan(&f.env, &admin).unwrap_err();
        assert_eq!(code(&error), "not_configured");
        assert_eq!(
            error["message"],
            "meeting is not configured; run `meeting install`"
        );
        assert_eq!(status(&admin)["running"], false);

        f.write_config(None);
        assert!(admin.progress.start());
        // A run is going: `Ok` (the command replies `started: true`) but no second thread.
        assert!(start_rescan(&f.env, &admin).unwrap().is_none());
        drop(Running(admin.progress.clone()));
        join(start_rescan(&f.env, &admin).unwrap());
    }

    #[test]
    fn doctor_lines_parse_and_junk_is_ignored() {
        let out = "✓ config\n✗ claude: Install it: brew install claude\n\nWARN noise\n✗ bare\n✓ \n";
        assert_eq!(
            parse_checks(out),
            [
                Check {
                    name: "config".into(),
                    ok: true,
                    fix: String::new()
                },
                Check {
                    name: "claude".into(),
                    ok: false,
                    fix: "Install it: brew install claude".into()
                },
                Check {
                    name: "bare".into(),
                    ok: false,
                    fix: String::new()
                },
            ]
        );
    }

    #[test]
    fn health_reports_the_doctors_checks() {
        let f = Fixture::new("health");
        f.install_tool("");
        let reply = health_of(&f.env, &admin());
        assert_eq!(
            reply,
            json!({"ok": true, "checks": [
                {"name": "config", "ok": true, "fix": ""},
                {"name": "claude", "ok": false, "fix": "Install Claude Code: see https://x.y"},
            ]})
        );
    }

    #[test]
    fn health_without_a_usable_tool_fails_meeting_path_without_spawning() {
        let f = Fixture::new("health-tool");
        let admin = Admin {
            doctor: unreachable_doctor,
            ..admin()
        };
        let expected = json!({"ok": true, "checks": [
            {"name": "meeting_path", "ok": false, "fix": "Run `meeting install`"},
        ]});
        assert_eq!(health_of(&f.env, &admin), expected);
        let tool = f.install_tool("");
        fs::set_permissions(&tool, fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(health_of(&f.env, &admin), expected);
        f.write_config(Some("relative/meeting".into()));
        assert_eq!(health_of(&f.env, &admin), expected);
        f.remove_config();
        assert_eq!(health_of(&f.env, &admin), expected);
    }

    #[test]
    fn a_doctor_that_fails_or_says_nothing_is_the_doctor_check() {
        let f = Fixture::new("health-doctor");
        f.install_tool("");
        for doctor in [silent_doctor, slow_doctor] {
            let reply = health_of(&f.env, &Admin { doctor, ..admin() });
            let checks = reply["checks"].as_array().unwrap();
            assert_eq!(checks.len(), 1);
            assert_eq!(checks[0]["name"], "doctor");
            assert_eq!(checks[0]["ok"], false);
            assert!(
                checks[0]["fix"]
                    .as_str()
                    .unwrap()
                    .contains("meeting doctor")
            );
        }
    }

    #[test]
    fn the_real_doctor_runner_reads_stdout_and_kills_on_timeout() {
        let f = Fixture::new("run-doctor");
        let tool = f.install_tool(
            "[ \"$1\" = doctor ] || exit 9\necho '✓ config'\necho '✗ claude: fix it'\n\
             echo noise >&2\nexit 1",
        );
        let real = Admin {
            doctor: run_doctor,
            ..admin()
        };
        let reply = health_of(&f.env, &real);
        assert_eq!(
            reply["checks"][1],
            json!({"name": "claude", "ok": false, "fix": "fix it"})
        );

        fs::write(&tool, "#!/bin/sh\nexec sleep 5\n").unwrap();
        let started = std::time::Instant::now();
        assert_eq!(
            run_doctor(&tool, Duration::from_millis(200)),
            Err("timed out")
        );
        assert!(started.elapsed() < Duration::from_secs(3));
        assert_eq!(
            run_doctor(Path::new("/no/such/meeting"), DOCTOR_TIMEOUT),
            Err("cannot start")
        );
    }

    #[test]
    fn settings_info_reports_the_config_and_the_store() {
        let f = Fixture::new("settings");
        let home = &f.env.home;
        assert_eq!(
            settings(&f.env),
            json!({"ok": true,
                "recordings_dir": home.join("Recordings"),
                "vault_dir": home.join("Vault"),
                "vault_folder": "Meetings",
                "store": {"ok": true}})
        );
        f.corrupt_store();
        let reply = settings(&f.env);
        assert_eq!(reply["ok"], true);
        assert_eq!(reply["store"]["ok"], false);
        assert!(
            reply["store"]["message"]
                .as_str()
                .unwrap()
                .contains("Rescan folders")
        );
        assert_eq!(reply["vault_folder"], "Meetings");
        assert!(f.broken().is_empty());
        f.remove_config();
        assert_eq!(code(&settings(&f.env)), "not_configured");
    }

    /// A processed meeting already in the store.
    fn stored(f: &Fixture, folder: &str) -> (i64, PathBuf) {
        let recording = f.meeting(folder, "Standup");
        f.backfill().unwrap();
        (f.row(&recording).id, recording)
    }

    #[test]
    fn archive_moves_the_folder_and_repoints_the_row() {
        let f = Fixture::new("archive");
        let (id, recording) = stored(&f, "m");
        assert_eq!(archive(&f.env, id), json!({"ok": true}));
        let moved = f.recordings().join("Archive/m/recording.webm");
        assert!(moved.is_file());
        assert!(!recording.exists());
        assert_eq!(f.store().get(id).unwrap().unwrap().recording_path, moved);
        // Already in Archive/.
        assert_eq!(code(&archive(&f.env, id)), "invalid_path");
    }

    #[test]
    fn archive_while_processing_moves_nothing() {
        let f = Fixture::new("archive-busy");
        let (id, recording) = stored(&f, "m");
        let _lock = Lock::acquire(&Lock::path_for(&recording)).unwrap();
        let reply = archive(&f.env, id);
        assert_eq!(code(&reply), "already_processing");
        assert!(recording.is_file());
        assert_eq!(code(&trash(&f.env, &admin(), id)), "already_processing");
    }

    #[test]
    fn trash_removes_the_folder_and_the_row() {
        let f = Fixture::new("trash");
        let (id, recording) = stored(&f, "m");
        let failing = Admin {
            trash: fail_trash,
            ..admin()
        };
        assert_eq!(code(&trash(&f.env, &failing, id)), "trash_failed");
        assert!(recording.is_file());
        assert_eq!(trash(&f.env, &admin(), id), json!({"ok": true}));
        assert!(!recording.exists());
        assert!(f.store().get(id).unwrap().is_none());
    }

    #[test]
    fn a_missing_recording_removes_the_row() {
        let f = Fixture::new("missing");
        let (archived, first) = stored(&f, "a");
        let (trashed, second) = stored(&f, "t");
        fs::remove_dir_all(first.parent().unwrap()).unwrap();
        fs::remove_dir_all(second.parent().unwrap()).unwrap();
        assert_eq!(archive(&f.env, archived), json!({"ok": true}));
        assert_eq!(trash(&f.env, &admin(), trashed), json!({"ok": true}));
        assert!(f.rows().is_empty());
    }

    #[test]
    fn unknown_notes_and_a_broken_store_are_refused() {
        let f = Fixture::new("refused");
        assert_eq!(code(&archive(&f.env, 42)), "unknown_note");
        assert_eq!(code(&trash(&f.env, &admin(), 42)), "unknown_note");
        f.corrupt_store();
        assert_eq!(code(&archive(&f.env, 1)), "store_unavailable");
        assert_eq!(code(&trash(&f.env, &admin(), 1)), "store_unavailable");
        assert!(f.broken().is_empty());
    }

    #[test]
    fn the_real_dependencies_are_the_tools() {
        let f = Fixture::new("real");
        let admin = Admin::new(&f.env);
        assert_eq!(admin.trash as usize, native::trash_with_system as usize);
        assert_eq!(admin.doctor as usize, run_doctor as usize);
        assert_eq!(admin.doctor_timeout, Duration::from_secs(10));
    }
}
