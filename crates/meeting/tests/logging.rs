use std::fs;
use std::process::Command;

fn run(name: &str, args: &[&str]) -> (std::process::Output, String) {
    let home = std::env::temp_dir().join(format!("meeting-test-{}-{name}", std::process::id()));
    let output = Command::new(env!("CARGO_BIN_EXE_meeting"))
        .args(args)
        .env("HOME", &home)
        .output()
        .unwrap();
    let log = fs::read_to_string(home.join("Library/Logs/meeting-manager/meeting.log"))
        .unwrap_or_default();
    fs::remove_dir_all(&home).unwrap();
    (output, log)
}

#[test]
fn run_logs_to_file_and_not_stdout() {
    let (output, log) = run("native-host", &["native-host", "chrome-extension://abc/"]);
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(log.contains("INFO meeting native-host"));
    assert!(log.contains("not implemented yet"));
}

#[test]
fn rejected_arguments_are_logged() {
    let (output, log) = run("bad-args", &["process", "x.webm", "--language", "fr"]);
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(log.contains("ERROR"));
}
