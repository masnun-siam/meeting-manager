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
    let mut command = Command::new(exe);
    command.arg("process").arg(recording);
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
    #[cfg(target_os = "macos")]
    use std::os::unix::fs::MetadataExt;
    use std::os::unix::fs::symlink;
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("meeting-test-{}-spawn-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A stand-in `meeting` that records its arguments, pid, process group and the devices
    /// behind its stdio, then `done` last. The stdio descriptors are copied first because a
    /// redirect would replace them.
    fn stub(dir: &Path) -> PathBuf {
        let path = dir.join("meeting");
        let script = r#"#!/bin/sh
d=$(/usr/bin/dirname "$0")
exec 3<&0 4>&1 5>&2
printf '%s\n' "$@" > "$d/args"
echo $$ > "$d/pid"
/bin/ps -o pgid= -p $$ > "$d/pgid"
/usr/bin/stat -f %r /dev/fd/3 /dev/fd/4 /dev/fd/5 > "$d/stdio"
/usr/bin/touch "$d/done"
"#;
        crate::test_support::write_script(&path, script);
        path
    }

    struct Seen {
        args: Vec<String>,
        // Read only by the macOS-only process group test.
        #[cfg(target_os = "macos")]
        pid: String,
        #[cfg(target_os = "macos")]
        pgid: String,
        #[cfg(target_os = "macos")]
        stdio: Vec<String>,
    }

    fn spawn_and_wait(name: &str, language: Option<Language>, job_id: Option<&str>) -> Seen {
        let dir = temp(name);
        let exe = stub(&dir);
        process_detached(
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
            #[cfg(target_os = "macos")]
            pid: read("pid").trim().to_string(),
            #[cfg(target_os = "macos")]
            pgid: read("pgid").trim().to_string(),
            #[cfg(target_os = "macos")]
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

    // Reads the child's group and stdio the macOS way; the Linux hook image skips it, macOS CI runs it.
    #[cfg(target_os = "macos")]
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
    fn check_tool_accepts_only_an_absolute_executable_file() {
        let dir = temp("check");
        let tool = stub(&dir);
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
