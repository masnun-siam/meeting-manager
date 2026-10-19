use std::fs;
use std::io::{Read, Write};
use std::process::{Command, Stdio};

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
        .args([
            "native-host",
            &format!("chrome-extension://{EXTENSION_ID}/"),
        ])
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
