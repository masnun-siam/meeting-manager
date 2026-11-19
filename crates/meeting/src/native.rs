use std::fmt::Display;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use anyhow::Context;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::Language;
use crate::calendar_fetch::FetchError;
use crate::config::{Config, check_part, check_recording};
use crate::context;
use crate::doctor;
use crate::folders::{self, Refusal};
use crate::jobs::{self, Job, JobStore, Stage, new_job_id, now_utc};
use crate::lock::{self, Lock};
use crate::spawn;
use crate::store::{self, Store};

/// Requests are a few dozen bytes; anything bigger is not from our extension.
const MAX_REQUEST_BYTES: u32 = 1024 * 1024;

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Request {
    ListJobs {
        limit: usize,
    },
    Open {
        job_id: String,
        what: OpenWhat,
    },
    Health,
    Process {
        path: PathBuf,
        language: Language,
        /// A `Value`, so a malformed context never becomes `bad_request`.
        #[serde(default)]
        context: Option<Value>,
    },
    Retry {
        job_id: String,
    },
    Archive {
        job_id: String,
    },
    Trash {
        job_id: String,
    },
    ProcessPart {
        path: PathBuf,
        language: Language,
    },
}

#[derive(Deserialize, Clone, Copy)]
#[serde(rename_all = "lowercase")]
enum OpenWhat {
    Summary,
    Folder,
}

pub struct Host {
    pub config: Result<Config, String>,
    pub jobs: JobStore,
    pub open: fn(&Path) -> io::Result<()>,
    pub trash: fn(&Path) -> io::Result<()>,
    pub key: fn() -> anyhow::Result<String>,
    pub manifest: PathBuf,
    /// The notes database; opened only when `desktop` is on.
    pub store_path: PathBuf,
    pub exe: PathBuf,
    /// Whether `FEATURE_MEETING_CONTEXT` was set at build time; off ignores `context`.
    pub context: bool,
    /// `FEATURE_DESKTOP_APP`: off, no store is touched.
    pub desktop: bool,
    /// `FEATURE_CONTINUE_RECORDING`: off, `process_part` is `bad_request`.
    pub continue_recording: bool,
    pub calendar: fn() -> anyhow::Result<Option<String>>,
    pub fetch: fn(&str) -> Result<String, FetchError>,
}

/// Opens with `/usr/bin/open`: Chrome starts the host with a bare `PATH`.
pub fn open_with_system(path: &Path) -> io::Result<()> {
    let status = std::process::Command::new("/usr/bin/open")
        .arg(path)
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!("open exited with {status}")))
    }
}

/// Moves to the Trash with `/usr/bin/trash -s` (macOS 15+); `-s` makes a failed move exit non-zero.
pub fn trash_with_system(path: &Path) -> io::Result<()> {
    let status = std::process::Command::new("/usr/bin/trash")
        .arg("-s")
        .arg(path)
        .status()
        .map_err(|e| match e.kind() {
            io::ErrorKind::NotFound => {
                io::Error::other("/usr/bin/trash is missing: Trash needs macOS 15 or later")
            }
            _ => e,
        })?;
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!("trash exited with {status}")))
    }
}

/// `Ok(None)` is a clean end of input; `Err(InvalidData)` is a frame over the size limit.
fn read_frame(r: &mut impl Read) -> io::Result<Option<Vec<u8>>> {
    let mut len = [0u8; 4];
    match r.read_exact(&mut len) {
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        r => r?,
    }
    let len = u32::from_ne_bytes(len);
    if len > MAX_REQUEST_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("frame of {len} bytes is too large"),
        ));
    }
    let mut bytes = vec![0; len as usize];
    r.read_exact(&mut bytes)?;
    Ok(Some(bytes))
}

fn write_frame(w: &mut impl Write, reply: &Value) -> io::Result<()> {
    let bytes = serde_json::to_vec(reply)?;
    let len = u32::try_from(bytes.len()).map_err(io::Error::other)?;
    w.write_all(&len.to_ne_bytes())?;
    w.write_all(&bytes)?;
    w.flush()
}

fn error(code: &str, message: impl Display) -> Value {
    json!({"ok": false, "code": code, "message": message.to_string()})
}

/// Answers one request per frame until the input ends.
pub fn serve(
    host: &Host,
    origin: Option<&str>,
    mut input: impl Read,
    mut output: impl Write,
) -> anyhow::Result<()> {
    loop {
        let reply = match read_frame(&mut input) {
            Ok(None) => return Ok(()),
            Ok(Some(bytes)) => handle(host, origin, &bytes)?,
            Err(e) if e.kind() == io::ErrorKind::InvalidData => {
                let reply = error("bad_request", &e);
                tracing::info!("native-host reply {}", reply["code"]);
                write_frame(&mut output, &reply)?;
                return Ok(());
            }
            Err(e) => return Err(e).context("cannot read a request"),
        };
        write_frame(&mut output, &reply).context("cannot write a reply")?;
    }
}

fn handle(host: &Host, origin: Option<&str>, bytes: &[u8]) -> anyhow::Result<Value> {
    let reply = dispatch(host, origin, bytes)?;
    tracing::info!(
        "native-host reply {}",
        reply.get("code").unwrap_or(&json!("ok"))
    );
    Ok(reply)
}

fn dispatch(host: &Host, origin: Option<&str>, bytes: &[u8]) -> anyhow::Result<Value> {
    let Ok(raw) = serde_json::from_slice::<Value>(bytes) else {
        return Ok(error("bad_request", "request is not valid JSON"));
    };
    // `health` answers even when the config is missing: it reports why.
    let is_health = raw["type"] == "health";
    match &host.config {
        Ok(config) => {
            if origin != Some(&format!("chrome-extension://{}/", config.extension_id)) {
                return Ok(error("bad_request", "request is from an unknown origin"));
            }
        }
        Err(message) if !is_health => return Ok(error("not_configured", message)),
        Err(_) => {}
    }
    let request = match serde_json::from_value::<Request>(raw.clone()) {
        Ok(request) => request,
        Err(e) => return Ok(error("bad_request", e)),
    };
    // Only the message type is logged, never its content.
    tracing::info!("native-host request {}", raw["type"]);
    match request {
        Request::ListJobs { limit } => Ok(json!({"ok": true, "jobs": host.jobs.list(limit)?})),
        Request::Open { job_id, what } => open(host, &job_id, what),
        Request::Process {
            path,
            language,
            context,
        } => start_checked(
            host,
            config_of(host),
            &path,
            Some(language),
            context.as_ref(),
        ),
        Request::Retry { job_id } => retry(host, &job_id),
        Request::Archive { job_id } => archive(host, config_of(host), &job_id),
        Request::Trash { job_id } => trash(host, config_of(host), &job_id),
        // Built without the flag, the request is as unknown as any other type.
        Request::ProcessPart { .. } if !host.continue_recording => {
            Ok(error("bad_request", "unknown request type process_part"))
        }
        Request::ProcessPart { path, language } => {
            process_part(host, config_of(host), &path, language)
        }
        Request::Health => Ok(json!({"ok": true, "checks": doctor::checks(&doctor::Env {
            config: &host.config,
            key: host.key,
            manifest: host.manifest.clone(),
            exe: host.exe.clone(),
            context: host.context,
            desktop: host.desktop,
            calendar: host.calendar,
            fetch: host.fetch,
        })})),
    }
}

/// Ids go straight into a file name, so only what `new_job_id` produces is allowed.
fn valid_job_id(id: &str) -> bool {
    !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

/// The job a request names. A reply in `Err` ends the request.
fn lookup_job(host: &Host, job_id: &str) -> anyhow::Result<Result<Job, Value>> {
    if !valid_job_id(job_id) {
        return Ok(Err(error("unknown_job", "no such job")));
    }
    Ok(host
        .jobs
        .read(job_id)?
        .ok_or_else(|| error("unknown_job", "no such job")))
}

fn open(host: &Host, job_id: &str, what: OpenWhat) -> anyhow::Result<Value> {
    let job = match lookup_job(host, job_id)? {
        Ok(job) => job,
        Err(reply) => return Ok(reply),
    };
    let target = match what {
        OpenWhat::Summary => job.summary_path.filter(|p| p.is_file()),
        OpenWhat::Folder => job
            .recording
            .parent()
            .filter(|p| p.is_dir())
            .map(Path::to_path_buf),
    };
    let Some(target) = target else {
        return Ok(error("missing_file", "the file or folder no longer exists"));
    };
    (host.open)(&target).with_context(|| format!("cannot open {}", target.display()))?;
    Ok(json!({"ok": true}))
}

fn retry(host: &Host, job_id: &str) -> anyhow::Result<Value> {
    let job = match lookup_job(host, job_id)? {
        Ok(job) => job,
        Err(reply) => return Ok(reply),
    };
    start_checked(host, config_of(host), &job.recording, None, None)
}

/// The notes database, only in a desktop build. One that cannot be opened is logged by kind
/// and left out: it never fails a request.
fn open_store(host: &Host) -> Option<Store> {
    if !host.desktop {
        return None;
    }
    Store::open(&host.store_path)
        .inspect_err(|e| tracing::warn!("store open failed: {}", store::error_kind(e)))
        .ok()
}

fn moved_reply(result: Result<(), Refusal>) -> Value {
    match result {
        Ok(()) => json!({"ok": true}),
        Err(refused) => error(refused.code, refused.message),
    }
}

/// Archives the job's recording folder: the rules are `folders::archive`'s.
fn archive(host: &Host, config: &Config, job_id: &str) -> anyhow::Result<Value> {
    let job = match lookup_job(host, job_id)? {
        Ok(job) => job,
        Err(reply) => return Ok(reply),
    };
    let store = open_store(host);
    let archived = folders::archive(config, &host.jobs, store.as_ref(), &job.recording);
    Ok(moved_reply(archived.map(drop)))
}

/// Moves the job's recording folder to the Trash: the rules are `folders::trash`'s.
fn trash(host: &Host, config: &Config, job_id: &str) -> anyhow::Result<Value> {
    let job = match lookup_job(host, job_id)? {
        Ok(job) => job,
        Err(reply) => return Ok(reply),
    };
    let store = open_store(host);
    Ok(moved_reply(folders::trash(
        config,
        &host.jobs,
        store.as_ref(),
        host.trash,
        &job.recording,
    )))
}

/// `dispatch` only reaches a request handler once the config has loaded.
fn config_of(host: &Host) -> &Config {
    host.config.as_ref().expect("dispatch checked the config")
}

/// Refuses a bad path or a recording already being processed, otherwise starts a job.
fn start_checked(
    host: &Host,
    config: &Config,
    path: &Path,
    language: Option<Language>,
    context: Option<&Value>,
) -> anyhow::Result<Value> {
    let recording = match check_recording(&config.recordings_dir, path) {
        Ok(recording) => recording,
        Err(reason) => return Ok(error("invalid_path", reason)),
    };
    if lock::is_held(&Lock::path_for(&recording)) {
        return Ok(error("already_processing", "already processing"));
    }
    if let (true, Some(raw)) = (host.context, context) {
        store_context(&recording, raw);
    }
    queue(host, new_job(recording), language, spawn::process_detached)
}

/// Hands a saved part to `meeting process --continue`. A held lock is no refusal: the
/// continuation stops that run. The meeting's latest job is reused, so it stays one job.
fn process_part(
    host: &Host,
    config: &Config,
    path: &Path,
    language: Language,
) -> anyhow::Result<Value> {
    let recording = match check_part(&config.recordings_dir, path) {
        Ok(recording) => recording,
        Err(reason) => return Ok(error("invalid_path", reason)),
    };
    let latest = host
        .jobs
        .list(usize::MAX)?
        .into_iter()
        .find(|job| job.recording == recording);
    let job = match latest {
        Some(mut job) => {
            job.stage = Stage::Queued;
            job.error = None;
            job.updated_at = now_utc();
            job
        }
        None => new_job(recording),
    };
    queue(host, job, Some(language), spawn::continue_detached)
}

fn new_job(recording: PathBuf) -> Job {
    let now = now_utc();
    Job {
        job_id: new_job_id(),
        recording,
        title: None,
        stage: Stage::Queued,
        error: None,
        started_at: now.clone(),
        updated_at: now,
        summary_path: None,
    }
}

/// `spawn::process_detached` or `spawn::continue_detached`.
type Spawn = fn(&Path, &Path, Option<Language>, Option<&str>) -> io::Result<()>;

/// Writes the queued job and starts its run. A run that cannot start is a failed job, and the
/// reply is still `ok`: the job list shows the failure.
fn queue(
    host: &Host,
    mut job: Job,
    language: Option<Language>,
    spawn: Spawn,
) -> anyhow::Result<Value> {
    host.jobs.write(&job)?;
    // The child is neither kept nor waited on: it is reparented when the host exits.
    if let Err(e) = spawn(&host.exe, &job.recording, language, Some(&job.job_id)) {
        let message = format!("queued: cannot start meeting process: {e}");
        tracing::error!("{message}");
        job.stage = Stage::Failed;
        job.error = Some(message);
        job.updated_at = now_utc();
        host.jobs.write(&job)?;
    }
    Ok(json!({"ok": true, "job_id": job.job_id}))
}

/// Writes `context.json` beside the recording. Never fails the request and never logs a value.
fn store_context(recording: &Path, raw: &Value) {
    if raw.is_null() {
        return;
    }
    match context::check(raw) {
        Err(rule) => tracing::warn!("context dropped: {rule}"),
        Ok(ctx) => {
            let written = serde_json::to_vec(&ctx)
                .map_err(anyhow::Error::from)
                .and_then(|bytes| {
                    jobs::write_atomic(&recording.with_file_name("context.json"), &bytes)
                });
            match written {
                Ok(()) => tracing::info!("context written"),
                Err(_) => tracing::warn!("context not written"),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::fs;
    use std::io::Cursor;

    use super::*;
    use crate::jobs::{Job, Stage};
    use crate::store::{RunStart, Source};
    use crate::test_support::stub_meeting;

    const ORIGIN: &str = "chrome-extension://ext/";

    thread_local! {
        static OPENED: RefCell<Vec<PathBuf>> = const { RefCell::new(Vec::new()) };
    }

    fn record(path: &Path) -> io::Result<()> {
        OPENED.with(|o| o.borrow_mut().push(path.to_path_buf()));
        Ok(())
    }

    thread_local! {
        static TRASHED: RefCell<Vec<PathBuf>> = const { RefCell::new(Vec::new()) };
    }

    fn record_trash(path: &Path) -> io::Result<()> {
        TRASHED.with(|t| t.borrow_mut().push(path.to_path_buf()));
        Ok(())
    }

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("meeting-test-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn config(dir: &Path) -> Config {
        Config {
            recordings_dir: dir.join("rec"),
            vault_dir: dir.join("vault"),
            vault_folder: "Meetings".into(),
            ffmpeg_path: "/ffmpeg".into(),
            claude_path: "/claude".into(),
            extension_id: "ext".into(),
            meeting_path: None,
        }
    }

    fn host(dir: &Path) -> Host {
        Host {
            config: Ok(config(dir)),
            jobs: JobStore {
                dir: dir.join("jobs"),
            },
            open: record,
            trash: record_trash,
            key: || Ok("k".into()),
            manifest: dir.join("host.json"),
            store_path: dir.join("store/notes.sqlite"),
            exe: dir.join("meeting"),
            context: true,
            desktop: false,
            continue_recording: true,
            calendar: || Ok(None),
            fetch: |_| panic!("no calendar is stored"),
        }
    }

    fn job(id: &str, started_at: &str, summary: Option<PathBuf>) -> Job {
        Job {
            job_id: id.into(),
            recording: PathBuf::from("/nope/m.webm"),
            title: None,
            stage: Stage::Done,
            error: None,
            started_at: started_at.into(),
            updated_at: started_at.into(),
            summary_path: summary,
        }
    }

    fn frames(requests: &[Value]) -> Vec<u8> {
        let mut out = Vec::new();
        for r in requests {
            write_frame(&mut out, r).unwrap();
        }
        out
    }

    fn raw_frame(bytes: &[u8]) -> Vec<u8> {
        let mut out = (bytes.len() as u32).to_ne_bytes().to_vec();
        out.extend_from_slice(bytes);
        out
    }

    fn replies(output: Vec<u8>) -> Vec<Value> {
        let mut cursor = Cursor::new(output);
        let mut out = Vec::new();
        while let Some(bytes) = read_frame(&mut cursor).unwrap() {
            out.push(serde_json::from_slice(&bytes).unwrap());
        }
        out
    }

    fn ask(host: &Host, origin: Option<&str>, input: Vec<u8>) -> Vec<Value> {
        let mut output = Vec::new();
        serve(host, origin, Cursor::new(input), &mut output).unwrap();
        replies(output)
    }

    fn one(host: &Host, request: Value) -> Value {
        ask(host, Some(ORIGIN), frames(&[request])).remove(0)
    }

    #[test]
    fn list_jobs_is_newest_first_limited_with_exact_keys() {
        let dir = temp("native-list");
        let host = host(&dir);
        for (i, id) in ["a", "b", "c"].iter().enumerate() {
            host.jobs
                .write(&job(id, &format!("2026-10-19T14:30:0{i}Z"), None))
                .unwrap();
        }
        let reply = one(&host, json!({"type": "list_jobs", "limit": 2}));
        let ids: Vec<_> = reply["jobs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|j| j["job_id"].as_str().unwrap())
            .collect();
        let mut keys: Vec<_> = reply["jobs"][0].as_object().unwrap().keys().collect();
        keys.sort();
        let all = one(&host, json!({"type": "list_jobs", "limit": 50}));
        fs::remove_dir_all(&dir).unwrap();
        assert_eq!(reply["ok"], true);
        assert_eq!(ids, ["c", "b"]);
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
        assert_eq!(all["jobs"].as_array().unwrap().len(), 3);
        assert!(reply["jobs"][0]["summary_path"].is_null());
    }

    #[test]
    fn list_jobs_without_a_jobs_dir_is_empty() {
        let dir = temp("native-no-jobs");
        let reply = one(&host(&dir), json!({"type": "list_jobs", "limit": 10}));
        fs::remove_dir_all(&dir).unwrap();
        assert_eq!(reply, json!({"ok": true, "jobs": []}));
    }

    #[test]
    fn open_summary_and_folder_run_open_on_the_path() {
        let dir = temp("native-open");
        let host = host(&dir);
        let summary = dir.join("s.md");
        fs::write(&summary, "x").unwrap();
        let mut j = job("a-1", "2026-10-19T14:30:00Z", Some(summary.clone()));
        j.recording = dir.join("m.webm");
        host.jobs.write(&j).unwrap();
        OPENED.with(|o| o.borrow_mut().clear());
        let summary_reply = one(
            &host,
            json!({"type": "open", "job_id": "a-1", "what": "summary"}),
        );
        let folder_reply = one(
            &host,
            json!({"type": "open", "job_id": "a-1", "what": "folder"}),
        );
        let opened = OPENED.with(|o| o.borrow().clone());
        fs::remove_dir_all(&dir).unwrap();
        assert_eq!(summary_reply, json!({"ok": true}));
        assert_eq!(folder_reply, json!({"ok": true}));
        assert_eq!(opened, [summary, dir]);
    }

    #[test]
    fn open_rejects_missing_and_unsafe_job_ids() {
        let dir = temp("native-unknown");
        let host = host(&dir);
        for id in ["nope", "../x", "a/b", ""] {
            let reply = one(
                &host,
                json!({"type": "open", "job_id": id, "what": "summary"}),
            );
            assert_eq!(reply["code"], "unknown_job", "id {id:?}");
        }
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn open_reports_missing_summary_file() {
        let dir = temp("native-missing");
        let host = host(&dir);
        host.jobs
            .write(&job("none", "2026-10-19T14:30:00Z", None))
            .unwrap();
        host.jobs
            .write(&job(
                "gone",
                "2026-10-19T14:30:01Z",
                Some(dir.join("gone.md")),
            ))
            .unwrap();
        let null = one(
            &host,
            json!({"type": "open", "job_id": "none", "what": "summary"}),
        );
        let deleted = one(
            &host,
            json!({"type": "open", "job_id": "gone", "what": "summary"}),
        );
        let folder = one(
            &host,
            json!({"type": "open", "job_id": "gone", "what": "folder"}),
        );
        fs::remove_dir_all(&dir).unwrap();
        assert_eq!(null["code"], "missing_file");
        assert_eq!(deleted["code"], "missing_file");
        assert_eq!(folder["code"], "missing_file");
    }

    #[test]
    fn bad_requests_are_rejected() {
        let dir = temp("native-bad");
        let host = host(&dir);
        let list = frames(&[json!({"type": "list_jobs", "limit": 1})]);
        let wrong_origin = ask(&host, Some("chrome-extension://evil/"), list.clone());
        let no_origin = ask(&host, None, list);
        let bad_json = ask(&host, Some(ORIGIN), raw_frame(b"{nope"));
        let unknown_type = one(&host, json!({"type": "nope"}));
        fs::remove_dir_all(&dir).unwrap();
        for reply in [&wrong_origin[0], &no_origin[0], &bad_json[0], &unknown_type] {
            assert_eq!(reply["ok"], false);
            assert_eq!(reply["code"], "bad_request");
        }
    }

    #[test]
    fn missing_config_replies_not_configured_naming_install() {
        let dir = temp("native-unconfigured");
        let mut host = host(&dir);
        host.config = Err("meeting is not configured; run `meeting install`".into());
        let reply = one(&host, json!({"type": "list_jobs", "limit": 1}));
        fs::remove_dir_all(&dir).unwrap();
        assert_eq!(reply["code"], "not_configured");
        assert!(
            reply["message"]
                .as_str()
                .unwrap()
                .contains("meeting install")
        );
    }

    #[test]
    fn health_lists_the_checks_with_and_without_a_config() {
        let dir = temp("native-health");
        let mut host = host(&dir);
        let request = json!({"type": "health"});
        let configured = one(&host, request.clone());
        host.config = Err("meeting is not configured; run `meeting install`".into());
        let unconfigured = ask(&host, None, frames(std::slice::from_ref(&request))).remove(0);
        host.config = Ok(config(&dir));
        let evil = ask(&host, Some("chrome-extension://evil/"), frames(&[request])).remove(0);
        fs::remove_dir_all(&dir).unwrap();
        let names = |r: &Value| -> Vec<String> {
            r["checks"]
                .as_array()
                .unwrap()
                .iter()
                .map(|c| c["name"].as_str().unwrap().to_string())
                .collect()
        };
        assert_eq!(configured["ok"], true);
        assert_eq!(names(&configured).len(), 8);
        assert_eq!(names(&configured).last().unwrap(), "calendar");
        assert_eq!(unconfigured["ok"], true);
        assert_eq!(unconfigured["checks"][0]["name"], "config");
        assert_eq!(unconfigured["checks"][0]["ok"], false);
        assert_eq!(unconfigured["checks"][1]["ok"], true);
        assert_eq!(evil["code"], "bad_request");
    }

    #[test]
    fn health_has_no_calendar_check_with_the_gate_off() {
        let dir = temp("native-health-off");
        let host = Host {
            context: false,
            ..host(&dir)
        };
        let reply = one(&host, json!({"type": "health"}));
        fs::remove_dir_all(&dir).unwrap();
        let checks = reply["checks"].as_array().unwrap();
        assert_eq!(checks.len(), 7);
        assert!(checks.iter().all(|c| c["name"] != "calendar"));
    }

    #[test]
    fn health_has_the_meeting_path_check_only_with_the_flag_on() {
        use std::os::unix::fs::PermissionsExt;
        let dir = temp("native-health-desktop");
        let mut host = host(&dir);
        let checks = |host: &Host| -> Vec<(String, bool, String)> {
            one(host, json!({"type": "health"}))["checks"]
                .as_array()
                .unwrap()
                .iter()
                .map(|c| {
                    let text = |key: &str| c[key].as_str().unwrap().to_string();
                    (text("name"), c["ok"] == true, text("fix"))
                })
                .collect()
        };
        let off = checks(&host);
        host.desktop = true;
        let unset = checks(&host);
        let tool = dir.join("meeting");
        fs::write(&tool, "#!/bin/sh\n").unwrap();
        fs::set_permissions(&tool, fs::Permissions::from_mode(0o755)).unwrap();
        let Ok(config) = host.config.as_mut() else {
            unreachable!()
        };
        config.meeting_path = Some(tool);
        let set = checks(&host);
        fs::remove_dir_all(&dir).unwrap();
        assert_eq!(off.len(), 8);
        assert!(off.iter().all(|c| c.0 != "meeting_path"));
        assert_eq!(unset.len(), 9);
        assert_eq!(
            unset[7],
            ("meeting_path".into(), false, "Run `meeting install`".into())
        );
        assert_eq!(unset[8].0, "calendar");
        assert_eq!(set[7], ("meeting_path".into(), true, String::new()));
    }

    #[test]
    fn two_frames_get_two_replies_and_eof_gets_none() {
        let dir = temp("native-two");
        let host = host(&dir);
        let request = json!({"type": "list_jobs", "limit": 1});
        let two = ask(&host, Some(ORIGIN), frames(&[request.clone(), request]));
        let none = ask(&host, Some(ORIGIN), Vec::new());
        fs::remove_dir_all(&dir).unwrap();
        assert_eq!(two.len(), 2);
        assert!(none.is_empty());
    }

    #[test]
    fn oversized_frame_gets_bad_request_and_stops() {
        let dir = temp("native-big");
        let mut input = (MAX_REQUEST_BYTES + 1).to_ne_bytes().to_vec();
        input.extend_from_slice(&frames(&[json!({"type": "list_jobs", "limit": 1})]));
        let out = ask(&host(&dir), Some(ORIGIN), input);
        fs::remove_dir_all(&dir).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["code"], "bad_request");
    }

    #[test]
    fn truncated_frame_is_an_error() {
        let dir = temp("native-truncated");
        let mut input = 10u32.to_ne_bytes().to_vec();
        input.extend_from_slice(b"{}");
        let result = serve(&host(&dir), Some(ORIGIN), Cursor::new(input), Vec::new());
        fs::remove_dir_all(&dir).unwrap();
        assert!(result.is_err());
    }

    fn webm(dir: &Path) -> PathBuf {
        let rec = dir.join("rec");
        fs::create_dir_all(rec.join("m")).unwrap();
        fs::write(rec.join("m/recording.webm"), "x").unwrap();
        fs::canonicalize(rec.join("m/recording.webm")).unwrap()
    }

    #[test]
    fn process_and_retry_need_a_config() {
        let dir = temp("native-proc-unconfigured");
        let mut host = host(&dir);
        host.config = Err("not configured".into());
        let process = one(
            &host,
            json!({"type": "process", "path": "/r/m.webm", "language": "en"}),
        );
        let retry = one(&host, json!({"type": "retry", "job_id": "a"}));
        fs::remove_dir_all(&dir).unwrap();
        assert_eq!(process["code"], "not_configured");
        assert_eq!(retry["code"], "not_configured");
    }

    #[test]
    fn process_rejects_bad_paths_and_a_bad_language() {
        let dir = temp("native-proc-path");
        let host = host(&dir);
        webm(&dir);
        fs::write(dir.join("rec/a.mp4"), "x").unwrap();
        fs::write(dir.join("rec/x.webm"), "x").unwrap();
        fs::write(dir.join("outside.webm"), "x").unwrap();
        let codes: Vec<_> = [
            dir.join("outside.webm"),
            dir.join("rec/a.mp4"),
            dir.join("rec/x.webm"),
            dir.join("rec/m/missing.webm"),
        ]
        .iter()
        .map(|p| {
            one(
                &host,
                json!({"type": "process", "path": p, "language": "en"}),
            )["code"]
                .clone()
        })
        .collect();
        let language = one(
            &host,
            json!({"type": "process", "path": dir.join("rec/m/recording.webm"), "language": "fr"}),
        );
        let wrote_jobs = host.jobs.dir.exists();
        fs::remove_dir_all(&dir).unwrap();
        assert_eq!(codes, ["invalid_path"; 4]);
        assert_eq!(language["code"], "bad_request");
        assert!(!wrote_jobs);
    }

    #[test]
    fn process_refuses_a_held_lock_without_a_job() {
        let dir = temp("native-proc-locked");
        let host = host(&dir);
        let recording = webm(&dir);
        fs::write(Lock::path_for(&recording), std::process::id().to_string()).unwrap();
        let reply = one(
            &host,
            json!({"type": "process", "path": recording, "language": "bn"}),
        );
        let wrote_jobs = host.jobs.dir.exists();
        fs::remove_dir_all(&dir).unwrap();
        assert_eq!(reply["code"], "already_processing");
        assert!(!wrote_jobs);
    }

    fn ctx() -> Value {
        json!({"version": 1, "platform": "other", "started_at_ms": 1, "utc_offset_min": 0})
    }

    fn process_with(host: &Host, recording: &Path, context: Value) -> Value {
        one(
            host,
            json!({"type": "process", "path": recording, "language": "en", "context": context}),
        )
    }

    #[test]
    fn process_writes_context_json_before_the_job() {
        let dir = temp("native-ctx");
        let host = host(&dir);
        let recording = webm(&dir);
        let reply = process_with(&host, &recording, ctx());
        let file = fs::read_to_string(recording.with_file_name("context.json")).unwrap();
        let job_written = host.jobs.dir.exists();
        fs::remove_dir_all(&dir).unwrap();
        assert_eq!(reply["ok"], true);
        assert!(reply["job_id"].is_string());
        assert!(job_written);
        assert_eq!(serde_json::from_str::<Value>(&file).unwrap()["version"], 1);
    }

    #[test]
    fn process_without_a_usable_context_writes_no_file_and_still_replies_ok() {
        let dir = temp("native-ctx-none");
        let host = host(&dir);
        let recording = webm(&dir);
        let target = recording.with_file_name("context.json");
        let mut replies = vec![
            one(
                &host,
                json!({"type": "process", "path": recording, "language": "en"}),
            ),
            process_with(&host, &recording, Value::Null),
            process_with(&host, &recording, json!(5)),
            process_with(&host, &recording, json!({"version": 2})),
        ];
        let off = Host {
            context: false,
            ..host
        };
        replies.push(process_with(&off, &recording, ctx()));
        let wrote = target.exists();
        fs::remove_dir_all(&dir).unwrap();
        assert!(replies.iter().all(|r| r["ok"] == true), "{replies:?}");
        assert!(!wrote);
    }

    #[test]
    fn an_unwritable_folder_still_starts_the_job() {
        use std::os::unix::fs::PermissionsExt;
        let dir = temp("native-ctx-ro");
        let host = host(&dir);
        let recording = webm(&dir);
        let folder = recording.parent().unwrap();
        fs::set_permissions(folder, fs::Permissions::from_mode(0o555)).unwrap();
        let reply = process_with(&host, &recording, ctx());
        fs::set_permissions(folder, fs::Permissions::from_mode(0o755)).unwrap();
        fs::remove_dir_all(&dir).unwrap();
        assert_eq!(reply["ok"], true);
    }

    #[test]
    fn a_context_without_a_new_one_is_kept_by_process_and_retry() {
        let dir = temp("native-ctx-keep");
        let host = host(&dir);
        let recording = webm(&dir);
        process_with(&host, &recording, ctx());
        let target = recording.with_file_name("context.json");
        let before = fs::read(&target).unwrap();
        let reply = one(
            &host,
            json!({"type": "process", "path": recording, "language": "en"}),
        );
        let mut j = job("r", "2026-10-19T14:30:00Z", None);
        j.recording = recording.clone();
        j.stage = Stage::Failed;
        host.jobs.write(&j).unwrap();
        let retried = one(&host, json!({"type": "retry", "job_id": "r"}));
        let after = fs::read(&target).unwrap();
        fs::remove_dir_all(&dir).unwrap();
        assert_eq!(reply["ok"], true);
        assert_eq!(retried["ok"], true);
        assert_eq!(before, after);
    }

    #[test]
    fn process_accepts_a_recording_with_another_listed_extension() {
        let dir = temp("native-proc-m4a");
        let host = host(&dir);
        let recording = webm(&dir).with_file_name("recording.m4a");
        fs::write(&recording, "x").unwrap();
        fs::write(Lock::path_for(&recording), std::process::id().to_string()).unwrap();
        let reply = one(
            &host,
            json!({"type": "process", "path": recording, "language": "en"}),
        );
        fs::remove_dir_all(&dir).unwrap();
        assert_eq!(reply["code"], "already_processing");
    }

    #[test]
    fn retry_rejects_unknown_ids_and_deleted_recordings() {
        let dir = temp("native-retry");
        let host = host(&dir);
        host.jobs
            .write(&job("old", "2026-10-19T14:30:00Z", None))
            .unwrap();
        let unknown: Vec<_> = ["nope", "../x"]
            .iter()
            .map(|id| one(&host, json!({"type": "retry", "job_id": id}))["code"].clone())
            .collect();
        let gone = one(&host, json!({"type": "retry", "job_id": "old"}));
        fs::remove_dir_all(&dir).unwrap();
        assert_eq!(unknown, ["unknown_job"; 2]);
        assert_eq!(gone["code"], "invalid_path");
    }

    #[test]
    fn a_failed_spawn_is_a_failed_job_with_an_ok_reply() {
        let dir = temp("native-spawn-fail");
        let mut host = host(&dir);
        host.exe = dir.join("no-such-exe");
        let recording = webm(&dir);
        let reply = one(
            &host,
            json!({"type": "process", "path": recording, "language": "en"}),
        );
        let job = host
            .jobs
            .read(reply["job_id"].as_str().unwrap())
            .unwrap()
            .unwrap();
        fs::remove_dir_all(&dir).unwrap();
        assert_eq!(reply["ok"], true);
        assert_eq!(job.stage, Stage::Failed);
        assert!(
            job.error
                .unwrap()
                .starts_with("queued: cannot start meeting process: ")
        );
    }

    /// `rec/m/recording.webm` (returned canonical) with `recording-part-2.webm` beside it.
    fn with_part(dir: &Path) -> PathBuf {
        let recording = webm(dir);
        fs::write(dir.join("rec/m/recording-part-2.webm"), "x").unwrap();
        recording
    }

    fn part_req(host: &Host, path: &Path) -> Value {
        one(
            host,
            json!({"type": "process_part", "path": path, "language": "bn"}),
        )
    }

    fn wait_for(path: &Path) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !path.exists() {
            assert!(
                std::time::Instant::now() < deadline,
                "no {}",
                path.display()
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }

    #[test]
    fn process_part_queues_a_job_and_spawns_with_continue() {
        let dir = temp("native-part-spawn");
        let mut host = host(&dir);
        host.exe = stub_meeting(&dir);
        let recording = with_part(&dir);
        let reply = part_req(&host, &dir.join("rec/m/recording-part-2.webm"));
        let id = reply["job_id"].as_str().unwrap().to_string();
        let job = host.jobs.read(&id).unwrap().unwrap();
        wait_for(&dir.join("done"));
        let args = fs::read_to_string(dir.join("args")).unwrap();
        let pid = fs::read_to_string(dir.join("pid")).unwrap();
        let pgid = fs::read_to_string(dir.join("pgid")).unwrap();
        let stdio = fs::read_to_string(dir.join("stdio")).unwrap();
        let null = fs::metadata("/dev/null").unwrap();
        fs::remove_dir_all(&dir).unwrap();

        assert_eq!(reply, json!({"ok": true, "job_id": id}));
        assert_eq!(job.stage, Stage::Queued);
        assert_eq!(job.recording, recording);
        let want = format!(
            "process\n{}\n--continue\n--language\nbn\n--job-id\n{id}\n",
            recording.display()
        );
        assert_eq!(args, want);
        assert_eq!(pid.trim(), pgid.trim(), "own process group");
        let null_dev = std::os::unix::fs::MetadataExt::rdev(&null).to_string();
        assert!(stdio.lines().all(|l| l.trim() == null_dev), "{stdio}");
    }

    #[test]
    fn process_part_reuses_the_latest_job_id() {
        let dir = temp("native-part-reuse");
        let mut host = host(&dir);
        host.exe = stub_meeting(&dir);
        let recording = with_part(&dir);
        let other = Job {
            recording: dir.join("rec/other/recording.webm"),
            ..job("newest-other", "2026-10-20T10:00:00Z", None)
        };
        let older = Job {
            recording: recording.clone(),
            ..job("older", "2026-10-19T09:00:00Z", None)
        };
        let latest = Job {
            recording: recording.clone(),
            title: Some("Standup".into()),
            stage: Stage::Failed,
            error: Some("summarising: boom".into()),
            ..job("latest", "2026-10-19T10:00:00Z", None)
        };
        for j in [&other, &older, &latest] {
            host.jobs.write(j).unwrap();
        }
        let reply = part_req(&host, &dir.join("rec/m/recording-part-2.webm"));
        let updated = host.jobs.read("latest").unwrap().unwrap();
        wait_for(&dir.join("done"));
        let count = fs::read_dir(&host.jobs.dir).unwrap().count();
        fs::remove_dir_all(&dir).unwrap();

        assert_eq!(reply, json!({"ok": true, "job_id": "latest"}));
        assert_eq!(updated.stage, Stage::Queued);
        assert_eq!(updated.error, None);
        assert_eq!(updated.started_at, "2026-10-19T10:00:00Z");
        assert_ne!(updated.updated_at, "2026-10-19T10:00:00Z");
        assert_eq!(updated.title.as_deref(), Some("Standup"));
        assert_eq!(count, 3, "no second job file");
    }

    #[test]
    fn process_part_ignores_a_held_lock() {
        let dir = temp("native-part-locked");
        let mut host = host(&dir);
        host.exe = stub_meeting(&dir);
        let recording = with_part(&dir);
        fs::write(Lock::path_for(&recording), std::process::id().to_string()).unwrap();
        let reply = part_req(&host, &dir.join("rec/m/recording-part-2.webm"));
        let job = host.jobs.read(reply["job_id"].as_str().unwrap()).unwrap();
        wait_for(&dir.join("done"));
        fs::remove_dir_all(&dir).unwrap();
        assert_eq!(reply["ok"], true);
        assert_eq!(job.unwrap().stage, Stage::Queued);
    }

    #[test]
    fn process_part_rejects_bad_paths() {
        use std::os::unix::fs::symlink;
        let dir = temp("native-part-paths");
        let mut host = host(&dir);
        host.exe = stub_meeting(&dir);
        let rec = dir.join("rec");
        with_part(&dir);
        for sub in ["Archive/m", "x/y", "only", "dir/recording-part-2.webm"] {
            fs::create_dir_all(rec.join(sub)).unwrap();
        }
        for file in [
            "m/recording-part-1.webm",
            "m/recording-part-0.webm",
            "m/recording-part-x.webm",
            "m/recording-part-2.webm.part",
            "m/recording-part-2.partial.wav",
            "m/recording-part-2.txt",
            "m/recording-part-3.m4a",
            "m/recording-part-4.WEBM",
            "recording.webm",
            "recording-part-2.webm",
            "x/y/recording.webm",
            "x/y/recording-part-2.webm",
            "Archive/m/recording.webm",
            "Archive/m/recording-part-2.webm",
            "only/recording-part-2.webm",
            "dir/recording.webm",
        ] {
            fs::write(rec.join(file), "x").unwrap();
        }
        fs::write(dir.join("outside.webm"), "x").unwrap();
        fs::create_dir_all(dir.join("elsewhere")).unwrap();
        for file in ["recording.webm", "recording-part-2.webm"] {
            fs::write(dir.join("elsewhere").join(file), "x").unwrap();
        }
        symlink(
            dir.join("outside.webm"),
            rec.join("m/recording-part-5.webm"),
        )
        .unwrap();
        symlink(
            rec.join("m/recording-part-2.webm"),
            rec.join("m/recording-part-6.webm"),
        )
        .unwrap();
        symlink(dir.join("elsewhere"), rec.join("out")).unwrap();
        symlink(rec.join("m"), rec.join("in")).unwrap();

        let paths = [
            PathBuf::from("rec/m/recording-part-2.webm"),
            rec.join("m/recording-part-7.webm"),
            rec.join("dir/recording-part-2.webm"),
            dir.join("elsewhere/recording-part-2.webm"),
            rec.join("m/recording-part-1.webm"),
            rec.join("m/recording-part-0.webm"),
            rec.join("m/recording-part-x.webm"),
            rec.join("m/recording-part-2.webm.part"),
            rec.join("m/recording-part-2.partial.wav"),
            rec.join("m/recording-part-2.txt"),
            rec.join("only/recording-part-2.webm"),
            rec.join("m/recording-part-3.m4a"),
            rec.join("recording-part-2.webm"),
            rec.join("x/y/recording-part-2.webm"),
            rec.join("Archive/m/recording-part-2.webm"),
            rec.join("m/../m/recording-part-2.webm"),
            rec.join("m/recording-part-5.webm"),
            rec.join("m/recording-part-6.webm"),
            rec.join("out/recording-part-2.webm"),
            rec.join("in/recording-part-2.webm"),
            rec.join("m/recording-part-4.WEBM"),
        ];
        let codes: Vec<_> = paths
            .iter()
            .map(|p| part_req(&host, p)["code"].clone())
            .collect();
        let wrote_jobs = host.jobs.dir.exists();
        let spawned = dir.join("args").exists();
        fs::remove_dir_all(&dir).unwrap();
        for (path, code) in paths.iter().zip(&codes) {
            assert_eq!(code, "invalid_path", "{}", path.display());
        }
        assert!(!wrote_jobs);
        assert!(!spawned);
    }

    #[test]
    fn process_part_needs_a_config_and_the_extension_origin() {
        let dir = temp("native-part-config");
        let mut host = host(&dir);
        with_part(&dir);
        let part = dir.join("rec/m/recording-part-2.webm");
        let request = json!({"type": "process_part", "path": part, "language": "en"});
        let wrong_origin = ask(
            &host,
            Some("chrome-extension://other/"),
            frames(std::slice::from_ref(&request)),
        );
        let no_origin = ask(&host, None, frames(std::slice::from_ref(&request)));
        host.config = Err("not configured".into());
        let unconfigured = one(&host, request);
        let wrote_jobs = host.jobs.dir.exists();
        fs::remove_dir_all(&dir).unwrap();
        assert_eq!(wrong_origin[0]["code"], "bad_request");
        assert_eq!(no_origin[0]["code"], "bad_request");
        assert_eq!(unconfigured["code"], "not_configured");
        assert!(!wrote_jobs);
    }

    #[test]
    fn process_part_is_bad_request_with_the_flag_off() {
        let dir = temp("native-part-off");
        let host = Host {
            continue_recording: false,
            exe: stub_meeting(&dir),
            ..host(&dir)
        };
        with_part(&dir);
        let valid = dir.join("rec/m/recording-part-2.webm");
        let invalid = dir.join("rec/m/recording-part-1.webm");
        let replies = [part_req(&host, &valid), part_req(&host, &invalid)];
        let wrote_jobs = host.jobs.dir.exists();
        let spawned = dir.join("args").exists();
        fs::remove_dir_all(&dir).unwrap();
        for reply in &replies {
            assert_eq!(reply["code"], "bad_request");
            let message = reply["message"].as_str().unwrap();
            assert!(!message.contains("recording-part"), "{message}");
            assert!(!message.contains(dir.to_str().unwrap()), "{message}");
        }
        assert!(!wrote_jobs);
        assert!(!spawned);
    }

    #[test]
    fn process_part_with_a_bad_language_or_a_missing_field_is_bad_request() {
        let dir = temp("native-part-lang");
        let host = host(&dir);
        with_part(&dir);
        let part = dir.join("rec/m/recording-part-2.webm");
        let codes = [
            json!({"type": "process_part", "path": part, "language": "fr"}),
            json!({"type": "process_part", "path": part}),
            json!({"type": "process_part", "language": "en"}),
        ]
        .map(|r| one(&host, r)["code"].clone());
        let wrote_jobs = host.jobs.dir.exists();
        fs::remove_dir_all(&dir).unwrap();
        assert_eq!(codes, ["bad_request"; 3]);
        assert!(!wrote_jobs);
    }

    #[test]
    fn a_failed_part_spawn_is_a_failed_job_with_an_ok_reply() {
        let dir = temp("native-part-spawn-fail");
        let mut host = host(&dir);
        host.exe = dir.join("no-such-exe");
        with_part(&dir);
        let reply = part_req(&host, &dir.join("rec/m/recording-part-2.webm"));
        let job = host
            .jobs
            .read(reply["job_id"].as_str().unwrap())
            .unwrap()
            .unwrap();
        fs::remove_dir_all(&dir).unwrap();
        assert_eq!(reply["ok"], true);
        assert_eq!(job.stage, Stage::Failed);
        assert!(
            job.error
                .unwrap()
                .starts_with("queued: cannot start meeting process: ")
        );
    }

    /// A folder `rec/<name>/recording.webm` plus a job for it.
    fn archivable(host: &Host, dir: &Path, name: &str, job_id: &str) -> PathBuf {
        let folder = dir.join("rec").join(name);
        fs::create_dir_all(&folder).unwrap();
        fs::write(folder.join("recording.webm"), "x").unwrap();
        let recording = fs::canonicalize(folder.join("recording.webm")).unwrap();
        let mut j = job(job_id, "2026-10-19T14:30:00Z", None);
        j.recording = recording.clone();
        host.jobs.write(&j).unwrap();
        recording
    }

    fn archive_req(host: &Host, id: &str) -> Value {
        one(host, json!({"type": "archive", "job_id": id}))
    }

    fn trash_req(host: &Host, id: &str) -> Value {
        one(host, json!({"type": "trash", "job_id": id}))
    }

    fn trashed() -> Vec<PathBuf> {
        TRASHED.with(|t| t.take())
    }

    /// A host built with the desktop flag, and its store.
    fn desktop(dir: &Path) -> (Host, Store) {
        let host = Host {
            desktop: true,
            ..host(dir)
        };
        let store = Store::open(&host.store_path).unwrap();
        (host, store)
    }

    fn seed(store: &Store, recording: &Path) -> i64 {
        store
            .start_run(&RunStart {
                recording,
                source: Source::Browser,
                recorded_at: "2026-10-19T14:30",
                language: None,
                attendees: None,
            })
            .unwrap()
    }

    #[test]
    fn archive_rejects_unknown_ids() {
        let dir = temp("native-archive-unknown");
        let host = host(&dir);
        let codes: Vec<_> = ["nope", "../x"]
            .map(|id| archive_req(&host, id)["code"].clone())
            .into();
        fs::remove_dir_all(&dir).unwrap();
        assert_eq!(codes, ["unknown_job"; 2]);
    }

    #[test]
    fn trash_rejects_unknown_ids() {
        let dir = temp("native-trash-unknown");
        let host = host(&dir);
        trashed();
        let replies = ["none", "../x", ""].map(|id| trash_req(&host, id));
        let calls = trashed();
        fs::remove_dir_all(&dir).unwrap();
        assert!(replies.iter().all(|r| r["code"] == "unknown_job"));
        assert!(calls.is_empty());
    }

    #[test]
    fn archive_by_job_id_replies_ok_and_moves_the_folder() {
        let dir = temp("native-archive");
        let host = host(&dir);
        archivable(&host, &dir, "m", "a");
        let reply = archive_req(&host, "a");
        let landed = dir.join("rec/Archive/m/recording.webm").is_file();
        let old_gone = !dir.join("rec/m").exists();
        let left = host.jobs.list(10).unwrap().len();
        fs::remove_dir_all(&dir).unwrap();
        assert_eq!(reply, json!({"ok": true}));
        assert!(landed && old_gone);
        assert_eq!(left, 0);
    }

    #[test]
    fn trash_by_job_id_replies_ok_and_calls_the_trash_fn() {
        let dir = temp("native-trash");
        let host = host(&dir);
        let recording = archivable(&host, &dir, "m", "a");
        trashed();
        let reply = trash_req(&host, "a");
        let calls = trashed();
        let left = host.jobs.list(10).unwrap().len();
        let expected = recording.parent().unwrap().to_path_buf();
        fs::remove_dir_all(&dir).unwrap();
        assert_eq!(reply, json!({"ok": true}));
        assert_eq!(calls, [expected]);
        assert_eq!(left, 0);
    }

    #[test]
    fn archive_and_trash_pass_refusal_codes_through() {
        let dir = temp("native-refusals");
        let host = host(&dir);
        let locked = archivable(&host, &dir, "m", "lock");
        let _lock = Lock::acquire(&Lock::path_for(&locked)).unwrap();
        let flat = dir.join("rec/flat.webm");
        fs::write(&flat, "x").unwrap();
        let mut j = job("flat", "2026-10-19T14:30:00Z", None);
        j.recording = fs::canonicalize(&flat).unwrap();
        host.jobs.write(&j).unwrap();
        trashed();
        let busy =
            json!({"ok": false, "code": "already_processing", "message": "already processing"});
        let replies = [
            archive_req(&host, "lock"),
            trash_req(&host, "lock"),
            archive_req(&host, "flat"),
            trash_req(&host, "flat"),
        ];
        let calls = trashed();
        let kept = host.jobs.list(10).unwrap().len();
        fs::remove_dir_all(&dir).unwrap();
        assert_eq!(replies[0], busy);
        assert_eq!(replies[1], busy);
        for reply in &replies[2..] {
            assert_eq!(reply["ok"], false);
            assert_eq!(reply["code"], "invalid_path");
            let message = reply["message"].as_str().unwrap();
            assert!(
                message.contains("is not in its own folder; move it to "),
                "{message}"
            );
        }
        assert!(calls.is_empty());
        assert_eq!(kept, 2);
    }

    #[test]
    fn archive_and_trash_leave_no_store_with_the_flag_off() {
        let dir = temp("native-no-store");
        let host = host(&dir);
        archivable(&host, &dir, "a", "a");
        archivable(&host, &dir, "t", "t");
        let gone = archivable(&host, &dir, "g", "g");
        fs::remove_file(&gone).unwrap();
        let replies = [
            archive_req(&host, "a"),
            trash_req(&host, "t"),
            archive_req(&host, "g"),
        ];
        let store_dir = host.store_path.parent().unwrap().exists();
        fs::remove_dir_all(&dir).unwrap();
        assert!(replies.iter().all(|r| r == &json!({"ok": true})));
        assert!(!store_dir);
    }

    #[test]
    fn archive_updates_the_store_with_the_flag_on() {
        let dir = temp("native-store-archive");
        let (host, store) = desktop(&dir);
        let old = archivable(&host, &dir, "m", "a");
        let id = seed(&store, &old);
        let reply = archive_req(&host, "a");
        let new = fs::canonicalize(dir.join("rec/Archive/m/recording.webm")).unwrap();
        let row = store.get_by_path(&new).unwrap().map(|r| r.id);
        let stale = store.get_by_path(&old).unwrap();
        fs::remove_dir_all(&dir).unwrap();
        assert_eq!(reply, json!({"ok": true}));
        assert_eq!(row, Some(id));
        assert_eq!(stale, None);
    }

    #[test]
    fn trash_removes_the_row_with_the_flag_on() {
        let dir = temp("native-store-trash");
        let (host, store) = desktop(&dir);
        let recording = archivable(&host, &dir, "m", "a");
        let other = archivable(&host, &dir, "other", "b");
        seed(&store, &recording);
        seed(&store, &other);
        let reply = trash_req(&host, "a");
        let left = store.paths().unwrap();
        fs::remove_dir_all(&dir).unwrap();
        assert_eq!(reply, json!({"ok": true}));
        assert_eq!(left, [other]);
    }

    #[test]
    fn a_store_that_cannot_open_never_fails_the_move() {
        let dir = temp("native-store-blocked");
        let mut host = host(&dir);
        host.desktop = true;
        // The store's parent is a regular file, so it can be neither created nor opened.
        fs::write(dir.join("blocked"), "x").unwrap();
        host.store_path = dir.join("blocked/notes.sqlite");
        archivable(&host, &dir, "a", "a");
        archivable(&host, &dir, "t", "t");
        trashed();
        let replies = [archive_req(&host, "a"), trash_req(&host, "t")];
        let landed = dir.join("rec/Archive/a/recording.webm").is_file();
        let calls = trashed().len();
        fs::remove_dir_all(&dir).unwrap();
        assert!(replies.iter().all(|r| r == &json!({"ok": true})));
        assert!(landed);
        assert_eq!(calls, 1);
    }
}
