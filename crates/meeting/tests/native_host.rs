use serde_json::{Value, json};
use std::fs;
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const EXTENSION_ID: &str = "obbaikcmcdooakogedoiiflflbbhpgdh";

#[test]
fn list_jobs_round_trip_over_stdio() {
    let home =
        std::env::temp_dir().join(format!("meeting-test-{}-native-host", std::process::id()));
    let _ = fs::remove_dir_all(&home);
    let config_dir = home.join(".config/meeting-manager");
    let jobs_dir = home.join("Library/Application Support/meeting-manager/jobs");
    fs::create_dir_all(&config_dir).unwrap();
    fs::create_dir_all(&jobs_dir).unwrap();
    fs::write(
        config_dir.join("config.toml"),
        format!(
            "recordings_dir = \"/r\"\nvault_dir = \"/v\"\nffmpeg_path = \"/f\"\nclaude_path = \"/c\"\nextension_id = \"{EXTENSION_ID}\"\n"
        ),
    )
    .unwrap();
    fs::write(
        jobs_dir.join("1-2-3.json"),
        r#"{"job_id":"1-2-3","recording":"/r/m.webm","title":null,"stage":"queued","error":null,"started_at":"2026-10-19T14:30:05Z","updated_at":"2026-10-19T14:30:05Z","summary_path":null}"#,
    )
    .unwrap();

    let mut child = Command::new(env!("CARGO_BIN_EXE_meeting"))
        // Exactly how Chrome launches the host: the origin is the only argument.
        .arg(format!("chrome-extension://{EXTENSION_ID}/"))
        .env("HOME", &home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let request = br#"{"type":"list_jobs","limit":10}"#;
    let mut stdin = child.stdin.take().unwrap();
    stdin
        .write_all(&(request.len() as u32).to_ne_bytes())
        .unwrap();
    stdin.write_all(request).unwrap();
    drop(stdin);
    let mut out = Vec::new();
    child.stdout.take().unwrap().read_to_end(&mut out).unwrap();
    let status = child.wait().unwrap();
    fs::remove_dir_all(&home).unwrap();

    assert!(status.success());
    let len = u32::from_ne_bytes(out[..4].try_into().unwrap()) as usize;
    assert_eq!(out.len(), 4 + len);
    let reply: serde_json::Value = serde_json::from_slice(&out[4..]).unwrap();
    assert_eq!(reply["ok"], true);
    assert_eq!(reply["jobs"][0]["job_id"], "1-2-3");
}

fn frame(request: &Value) -> Vec<u8> {
    let bytes = serde_json::to_vec(request).unwrap();
    let mut out = (bytes.len() as u32).to_ne_bytes().to_vec();
    out.extend(bytes);
    out
}

/// Sends one request to a real host process; returns the reply and how long it took.
fn ask(home: &Path, request: &Value) -> (Value, Duration) {
    let start = Instant::now();
    let mut child = Command::new(env!("CARGO_BIN_EXE_meeting"))
        // Exactly how Chrome launches the host: the origin is the only argument.
        .arg(format!("chrome-extension://{EXTENSION_ID}/"))
        .env("HOME", home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    stdin.write_all(&frame(request)).unwrap();
    drop(stdin);
    let mut out = Vec::new();
    child.stdout.take().unwrap().read_to_end(&mut out).unwrap();
    assert!(child.wait().unwrap().success());
    (serde_json::from_slice(&out[4..]).unwrap(), start.elapsed())
}

fn wait_for_failure(jobs_dir: &Path, job_id: &str) -> Value {
    let path = jobs_dir.join(format!("{job_id}.json"));
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let job: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        if job["stage"] == "failed" || Instant::now() > deadline {
            return job;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn process_and_retry_outlive_the_host() {
    let home = std::env::temp_dir().join(format!("meeting-test-{}-detach", std::process::id()));
    let _ = fs::remove_dir_all(&home);
    let config_dir = home.join(".config/meeting-manager");
    let jobs_dir = home.join("Library/Application Support/meeting-manager/jobs");
    let rec_dir = home.join("rec");
    fs::create_dir_all(&config_dir).unwrap();
    fs::create_dir_all(&rec_dir).unwrap();
    let ffmpeg = home.join("ffmpeg");
    fs::write(&ffmpeg, "#!/bin/sh\nsleep 2\nexit 1\n").unwrap();
    fs::set_permissions(&ffmpeg, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let folder = rec_dir.join("meeting-2026-10-19_1430");
    fs::create_dir_all(&folder).unwrap();
    let recording = folder.join("recording.webm");
    fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/short.webm"),
        &recording,
    )
    .unwrap();
    fs::write(
        config_dir.join("config.toml"),
        format!(
            "recordings_dir = \"{}\"\nvault_dir = \"{}\"\nffmpeg_path = \"{}\"\nclaude_path = \"/c\"\nextension_id = \"{EXTENSION_ID}\"\n",
            rec_dir.display(),
            home.join("vault").display(),
            ffmpeg.display()
        ),
    )
    .unwrap();

    let (reply, took) = ask(
        &home,
        &json!({"type": "process", "path": recording, "language": "en"}),
    );
    assert_eq!(reply["ok"], true);
    assert!(took < Duration::from_secs(1), "replied after {took:?}");
    let job = wait_for_failure(&jobs_dir, reply["job_id"].as_str().unwrap());

    let (retried, _) = ask(&home, &json!({"type": "retry", "job_id": reply["job_id"]}));
    let retry_job = wait_for_failure(&jobs_dir, retried["job_id"].as_str().unwrap());
    fs::remove_dir_all(&home).unwrap();

    for j in [&job, &retry_job] {
        assert_eq!(j["stage"], "failed");
        assert!(
            j["error"]
                .as_str()
                .unwrap()
                .starts_with("extracting: ffmpeg failed"),
            "{j}"
        );
    }
    assert_ne!(retried["job_id"], reply["job_id"]);
}
