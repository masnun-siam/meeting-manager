//! Starting `meeting process` in the background. The native host and the desktop app both
//! call this, each with an absolute path of its own: `PATH` is never consulted.

use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};

use crate::Language;

/// Runs `<exe> process <recording> [--language bn|en] [--job-id <id>]` in its own process
/// group with no stdio, so the job outlives whoever started it. The child is neither kept
/// nor waited on.
pub fn process_detached(
    exe: &Path,
    recording: &Path,
    language: Option<Language>,
    job_id: Option<&str>,
) -> io::Result<()> {
    spawn(exe, recording, language, job_id, false)
}

/// As `process_detached`, with `--continue` right after the recording: the run stops an
/// earlier run of the meeting and reuses its job id. Same shape as `process_detached`, so a
/// caller can inject either.
pub fn continue_detached(
    exe: &Path,
    recording: &Path,
    language: Option<Language>,
    job_id: Option<&str>,
) -> io::Result<()> {
    spawn(exe, recording, language, job_id, true)
}

fn spawn(
    exe: &Path,
    recording: &Path,
    language: Option<Language>,
    job_id: Option<&str>,
    continuing: bool,
) -> io::Result<()> {
    let mut command = Command::new(exe);
    command.arg("process").arg(recording);
    if continuing {
        command.arg("--continue");
    }
    match language {
        Some(Language::Bn) => command.args(["--language", "bn"]),
        Some(Language::En) => command.args(["--language", "en"]),
        None => &mut command,
    };
    if let Some(id) = job_id {
        command.args(["--job-id", id]);
    }
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .map(drop)
}

/// Whether `path` can be spawned as the tool: absolute, and an executable regular file
/// (a symlink to one counts). The `Err` says which rule failed.
pub fn check_tool(path: &Path) -> Result<(), &'static str> {
    if !path.is_absolute() {
        return Err("not an absolute path");
    }
    let Some(file) = fs::metadata(path).ok().filter(fs::Metadata::is_file) else {
        return Err("not a file");
    };
    if file.permissions().mode() & 0o111 == 0 {
        return Err("not executable");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::{MetadataExt, symlink};
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    use super::*;
    use crate::test_support::stub_meeting;

    fn temp(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("meeting-test-{}-spawn-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    struct Seen {
        args: Vec<String>,
        pid: String,
        pgid: String,
        stdio: Vec<String>,
    }

    type SpawnFn = fn(&Path, &Path, Option<Language>, Option<&str>) -> io::Result<()>;

    fn spawn_and_wait(name: &str, language: Option<Language>, job_id: Option<&str>) -> Seen {
        spawn_with(process_detached, name, language, job_id)
    }

    fn spawn_with(
        spawn: SpawnFn,
        name: &str,
        language: Option<Language>,
        job_id: Option<&str>,
    ) -> Seen {
        let dir = temp(name);
        let exe = stub_meeting(&dir);
        spawn(
            &exe,
            Path::new("/r/my meeting/recording.m4a"),
            language,
            job_id,
        )
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !dir.join("done").exists() {
            assert!(Instant::now() < deadline, "the child never finished");
            std::thread::sleep(Duration::from_millis(20));
        }
        let read = |file: &str| fs::read_to_string(dir.join(file)).unwrap();
        let lines = |file: &str| read(file).lines().map(str::to_string).collect();
        let seen = Seen {
            args: lines("args"),
            pid: read("pid").trim().to_string(),
            pgid: read("pgid").trim().to_string(),
            stdio: lines("stdio"),
        };
        fs::remove_dir_all(&dir).unwrap();
        seen
    }

    #[test]
    fn process_detached_passes_the_arguments_in_order() {
        const REC: &str = "/r/my meeting/recording.m4a";
        let args = |name, language, job_id| spawn_and_wait(name, language, job_id).args;
        assert_eq!(
            args("bn-id", Some(Language::Bn), Some("j-1")),
            ["process", REC, "--language", "bn", "--job-id", "j-1"]
        );
        assert_eq!(
            args("en", Some(Language::En), None),
            ["process", REC, "--language", "en"]
        );
        assert_eq!(
            args("id", None, Some("j-2")),
            ["process", REC, "--job-id", "j-2"]
        );
        assert_eq!(args("bare", None, None), ["process", REC]);
    }

    #[test]
    fn process_detached_gives_the_child_its_own_group_and_null_stdio() {
        let seen = spawn_and_wait("group", None, None);
        let null = fs::metadata("/dev/null").unwrap().rdev().to_string();
        assert!(!seen.pid.is_empty());
        assert_eq!(seen.pgid, seen.pid);
        assert_ne!(seen.pid, std::process::id().to_string());
        assert_eq!(seen.stdio, [null.clone(), null.clone(), null]);
    }

    #[test]
    fn process_detached_with_a_missing_exe_is_an_error() {
        let dir = temp("missing");
        let result = process_detached(
            &dir.join("nope"),
            Path::new("/r/m/recording.m4a"),
            None,
            None,
        );
        fs::remove_dir_all(&dir).unwrap();
        assert!(result.is_err());
    }

    #[test]
    fn continue_detached_passes_the_arguments_in_order() {
        const REC: &str = "/r/my meeting/recording.m4a";
        let seen = |name, language, job_id| spawn_with(continue_detached, name, language, job_id);
        let all = seen("c-bn-id", Some(Language::Bn), Some("j-1"));
        assert_eq!(
            all.args,
            [
                "process",
                REC,
                "--continue",
                "--language",
                "bn",
                "--job-id",
                "j-1"
            ]
        );
        assert_eq!(all.pgid, all.pid);
        let null = fs::metadata("/dev/null").unwrap().rdev().to_string();
        assert_eq!(all.stdio, [null.clone(), null.clone(), null]);
        assert_eq!(
            seen("c-en", Some(Language::En), None).args,
            ["process", REC, "--continue", "--language", "en"]
        );
        assert_eq!(
            seen("c-bare", None, None).args,
            ["process", REC, "--continue"]
        );
    }

    #[test]
    fn continue_detached_with_a_missing_exe_is_an_error() {
        let dir = temp("c-missing");
        let result = continue_detached(
            &dir.join("nope"),
            Path::new("/r/m/recording.m4a"),
            None,
            None,
        );
        fs::remove_dir_all(&dir).unwrap();
        assert!(result.is_err());
    }

    #[test]
    fn check_tool_accepts_only_an_absolute_executable_file() {
        let dir = temp("check");
        let tool = stub_meeting(&dir);
        let plain = dir.join("plain");
        fs::write(&plain, "x").unwrap();
        fs::set_permissions(&plain, fs::Permissions::from_mode(0o644)).unwrap();
        let link = dir.join("link");
        symlink(&tool, &link).unwrap();
        let results = (
            check_tool(Path::new("bin/meeting")),
            check_tool(&dir.join("nope")),
            check_tool(&dir),
            check_tool(&plain),
            check_tool(&tool),
            check_tool(&link),
        );
        fs::remove_dir_all(&dir).unwrap();
        assert_eq!(
            results,
            (
                Err("not an absolute path"),
                Err("not a file"),
                Err("not a file"),
                Err("not executable"),
                Ok(()),
                Ok(()),
            )
        );
    }
}
