use std::fmt::Display;
use std::io::{self, Read, Write};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::Context;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::Language;
use crate::config::{Config, check_recording};
use crate::doctor;
use crate::jobs::{Job, JobStore, Stage, new_job_id, now_utc};
use crate::lock::{self, Lock};

/// Requests are a few dozen bytes; anything bigger is not from our extension.
const MAX_REQUEST_BYTES: u32 = 1024 * 1024;

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Request {
    ListJobs { limit: usize },
    Open { job_id: String, what: OpenWhat },
    Health,
    Process { path: PathBuf, language: Language },
    Retry { job_id: String },
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
    pub key: fn() -> anyhow::Result<String>,
    pub manifest: PathBuf,
    pub exe: PathBuf,
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
        Request::Process { path, language } => {
            start_checked(host, config_of(host), &path, Some(language))
        }
        Request::Retry { job_id } => retry(host, &job_id),
        Request::Health => Ok(json!({"ok": true, "checks": doctor::checks(&doctor::Env {
            config: &host.config,
            key: host.key,
            manifest: host.manifest.clone(),
            exe: host.exe.clone(),
        })})),
    }
}

/// Ids go straight into a file name, so only what `new_job_id` produces is allowed.
fn valid_job_id(id: &str) -> bool {
    !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

fn open(host: &Host, job_id: &str, what: OpenWhat) -> anyhow::Result<Value> {
    if !valid_job_id(job_id) {
        return Ok(error("unknown_job", "no such job"));
    }
    let Some(job) = host.jobs.read(job_id)? else {
        return Ok(error("unknown_job", "no such job"));
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
    if !valid_job_id(job_id) {
        return Ok(error("unknown_job", "no such job"));
    }
    let Some(job) = host.jobs.read(job_id)? else {
        return Ok(error("unknown_job", "no such job"));
    };
    start_checked(host, config_of(host), &job.recording, None)
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
) -> anyhow::Result<Value> {
    let recording = match check_recording(&config.recordings_dir, path) {
        Ok(recording) => recording,
        Err(reason) => return Ok(error("invalid_path", reason)),
    };
    if lock::is_held(&Lock::path_for(&recording)) {
        return Ok(error("already_processing", "already processing"));
    }
    let now = now_utc();
    let mut job = Job {
        job_id: new_job_id(),
        recording,
        title: None,
        stage: Stage::Queued,
        error: None,
        started_at: now.clone(),
        updated_at: now,
        summary_path: None,
    };
    host.jobs.write(&job)?;
    // The child is neither kept nor waited on: it is reparented when the host exits.
    if let Err(e) = spawn_process(&host.exe, &job, language) {
        let message = format!("queued: cannot start meeting process: {e}");
        tracing::error!("{message}");
        job.stage = Stage::Failed;
        job.error = Some(message);
        job.updated_at = now_utc();
        host.jobs.write(&job)?;
    }
    Ok(json!({"ok": true, "job_id": job.job_id}))
}

/// Own process group and no stdio, so the job outlives Chrome closing the pipe.
fn spawn_process(exe: &Path, job: &Job, language: Option<Language>) -> io::Result<()> {
    let mut command = Command::new(exe);
    command.arg("process").arg(&job.recording);
    match language {
        Some(Language::Bn) => command.args(["--language", "bn"]),
        Some(Language::En) => command.args(["--language", "en"]),
        None => &mut command,
    };
    command
        .args(["--job-id", &job.job_id])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .map(drop)
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::fs;
    use std::io::Cursor;

    use super::*;
    use crate::jobs::{Job, Stage};

    const ORIGIN: &str = "chrome-extension://ext/";

    thread_local! {
        static OPENED: RefCell<Vec<PathBuf>> = const { RefCell::new(Vec::new()) };
    }

    fn record(path: &Path) -> io::Result<()> {
        OPENED.with(|o| o.borrow_mut().push(path.to_path_buf()));
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
        }
    }

    fn host(dir: &Path) -> Host {
        Host {
            config: Ok(config(dir)),
            jobs: JobStore {
                dir: dir.join("jobs"),
            },
            open: record,
            key: || Ok("k".into()),
            manifest: dir.join("host.json"),
            exe: dir.join("meeting"),
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
        assert_eq!(names(&configured).len(), 7);
        assert_eq!(unconfigured["ok"], true);
        assert_eq!(unconfigured["checks"][0]["name"], "config");
        assert_eq!(unconfigured["checks"][0]["ok"], false);
        assert_eq!(unconfigured["checks"][1]["ok"], true);
        assert_eq!(evil["code"], "bad_request");
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
}
