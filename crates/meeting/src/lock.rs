use std::fs::{self, OpenOptions};
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, bail};

const BUSY: &str = "already processing";
/// How often `stop_holder` checks whether the earlier run has ended.
const POLL: Duration = Duration::from_millis(100);

/// `lock` in a recording's folder, holding the PID of the job that runs it.
/// Removed on drop; one left by a crashed process is taken over.
#[derive(Debug)]
pub struct Lock {
    path: PathBuf,
}

impl Lock {
    pub fn path_for(recording: &Path) -> PathBuf {
        recording.with_file_name("lock")
    }

    pub fn acquire(path: &Path) -> anyhow::Result<Lock> {
        match create(path) {
            Err(e) if e.kind() == ErrorKind::AlreadyExists => {}
            r => {
                return r
                    .map(|()| Lock { path: path.into() })
                    .with_context(|| cannot(path));
            }
        }
        if is_held(path) {
            bail!(BUSY);
        }
        remove(path).with_context(|| cannot(path))?;
        match create(path) {
            Err(e) if e.kind() == ErrorKind::AlreadyExists => bail!(BUSY),
            r => r
                .map(|()| Lock { path: path.into() })
                .with_context(|| cannot(path)),
        }
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        if let Err(e) = remove(&self.path) {
            tracing::warn!("cannot remove {}: {e}", self.path.display());
        }
    }
}

/// True when the file exists and holds the PID of a running process.
pub fn is_held(path: &Path) -> bool {
    fs::read_to_string(path)
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .is_some_and(pid_running)
}

/// For `meeting process --continue`: stops the run holding `lock` when it is a
/// `meeting process` run of `recording` (canonical), then removes the lock. SIGTERM first,
/// SIGKILL after `grace`; to its whole process group when it leads one (spawned runs do, so
/// ffmpeg and `claude` end too), else to the PID alone. A lock that is missing, unreadable,
/// dead or held by any other process is left to `Lock::acquire`'s stale path, or removed as
/// stale, and nothing is signalled.
pub fn stop_holder(lock: &Path, recording: &Path, grace: Duration) -> anyhow::Result<()> {
    let Some(pid) = fs::read_to_string(lock)
        .ok()
        .and_then(|s| s.trim().parse::<u32>().ok())
        .filter(|&pid| pid != std::process::id() && alive(pid))
    else {
        return Ok(());
    };
    if !command_of(pid).is_some_and(|c| is_process_run(&c, recording)) {
        tracing::info!("lock was stale");
        return remove(lock).with_context(|| cannot(lock));
    }
    let own_group = pgid_of(std::process::id());
    let target = match pgid_of(pid) {
        Some(pgid) if pgid == pid && Some(pgid) != own_group => Target::Group(pgid),
        _ => Target::Pid(pid),
    };
    for signal in ["TERM", "KILL"] {
        tracing::info!("stopping earlier run: {signal}");
        send(signal, target);
        if ended(pid, grace) {
            tracing::info!("stopped earlier run");
            return remove(lock).with_context(|| cannot(lock));
        }
    }
    bail!("cannot stop the earlier run (process {pid}); stop it and run again")
}

/// Whether `command` (a `ps` command line) is `meeting process <recording> [--…]`: ` process `
/// right after a word ending in `meeting`, then an absolute path, cut at the first ` --`,
/// that canonicalises to `recording`. Spaces in the paths are fine; anything else is not a
/// run of this recording.
fn is_process_run(command: &str, recording: &Path) -> bool {
    const SEP: &str = " process ";
    command.match_indices(SEP).any(|(at, _)| {
        if !command[..at].ends_with("meeting") {
            return false;
        }
        let rest = &command[at + SEP.len()..];
        let path = Path::new(rest.find(" --").map_or(rest, |end| &rest[..end]));
        path.is_absolute() && fs::canonicalize(path).is_ok_and(|p| p == recording)
    })
}

#[derive(Clone, Copy)]
enum Target {
    Group(u32),
    Pid(u32),
}

/// `/bin/kill -<signal>` to the group or the PID. A failure (it already ended) is ignored;
/// `ended` decides.
fn send(signal: &str, target: Target) {
    let target = match target {
        Target::Group(pgid) => format!("-{pgid}"),
        Target::Pid(pid) => pid.to_string(),
    };
    let _ = Command::new("/bin/kill")
        .args([&format!("-{signal}"), "--", &target])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// Waits up to `grace` for `pid` to end.
fn ended(pid: u32, grace: Duration) -> bool {
    let deadline = Instant::now() + grace;
    loop {
        if !alive(pid) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(POLL);
    }
}

/// Running and not a zombie: a run killed while its starter (the host, the desktop app)
/// lives on stays listed by `ps` until that starter reaps it.
fn alive(pid: u32) -> bool {
    ps(&["-o", "stat=", "-p", &pid.to_string()]).is_some_and(|stat| !stat.starts_with('Z'))
}

/// `pid`'s full command line, through `ps` (absolute path, as `pid_running`).
fn command_of(pid: u32) -> Option<String> {
    ps(&["-o", "command=", "-p", &pid.to_string()])
}

fn pgid_of(pid: u32) -> Option<u32> {
    ps(&["-o", "pgid=", "-p", &pid.to_string()])?.parse().ok()
}

fn ps(args: &[&str]) -> Option<String> {
    let out = Command::new("/bin/ps")
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()
        .filter(|o| o.status.success())?;
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn create(path: &Path) -> std::io::Result<()> {
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    write!(file, "{}", std::process::id())
}

fn remove(path: &Path) -> std::io::Result<()> {
    match fs::remove_file(path) {
        Err(e) if e.kind() != ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

fn cannot(path: &Path) -> String {
    format!("cannot lock {}", path.display())
}

// Absolute path: Chrome starts the host with a bare `PATH`. `ps` sees any user's
// processes, unlike `kill -0`, and needs no `libc`.
// ponytail: a reused PID makes a stale lock look held until that process exits;
// also store and compare the process start time to close it.
fn pid_running(pid: u32) -> bool {
    Command::new("/bin/ps")
        .args(["-p", &pid.to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("meeting-test-{}-{name}", std::process::id()))
    }

    fn dead_pid() -> u32 {
        let mut child = Command::new("/usr/bin/true").spawn().unwrap();
        let pid = child.id();
        child.wait().unwrap();
        pid
    }

    #[test]
    fn acquire_writes_this_pid() {
        let path = temp("acquire.lock");
        let lock = Lock::acquire(&path).unwrap();
        let held = fs::read_to_string(&path).unwrap();
        drop(lock);
        assert_eq!(held, std::process::id().to_string());
    }

    #[test]
    fn second_acquire_is_refused_while_held() {
        let path = temp("second.lock");
        let _lock = Lock::acquire(&path).unwrap();
        let err = Lock::acquire(&path).unwrap_err();
        assert_eq!(err.to_string(), "already processing");
        assert!(is_held(&path));
    }

    #[test]
    fn drop_unlocks() {
        let path = temp("release.lock");
        drop(Lock::acquire(&path).unwrap());
        assert!(!path.exists());
        drop(Lock::acquire(&path).unwrap());
        assert!(!path.exists());
        Lock::acquire(&path).unwrap();
    }

    #[test]
    fn dead_pid_lock_is_taken_over() {
        let path = temp("stale.lock");
        fs::write(&path, dead_pid().to_string()).unwrap();
        assert!(!is_held(&path));
        let _lock = Lock::acquire(&path).unwrap();
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            std::process::id().to_string()
        );
    }

    #[test]
    fn garbage_lock_is_taken_over() {
        let path = temp("garbage.lock");
        fs::write(&path, "garbage").unwrap();
        assert!(!is_held(&path));
        let _lock = Lock::acquire(&path).unwrap();
        assert!(is_held(&path));
    }

    #[test]
    fn missing_lock_is_not_held() {
        assert!(!is_held(&temp("missing.lock")));
    }

    const GRACE: Duration = Duration::from_millis(300);

    /// A temp dir with `m/recording.webm` and a stub `meeting` script running `body`.
    fn meeting_stub(name: &str, body: &str) -> (PathBuf, PathBuf) {
        use std::os::unix::fs::PermissionsExt;
        let dir = temp(name);
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("m")).unwrap();
        let dir = dir.canonicalize().unwrap();
        let rec = dir.join("m/recording.webm");
        fs::write(&rec, "x").unwrap();
        let exe = dir.join("meeting");
        fs::write(&exe, format!("#!/bin/sh\n{body}\n")).unwrap();
        fs::set_permissions(&exe, fs::Permissions::from_mode(0o755)).unwrap();
        (dir, rec)
    }

    /// Starts `<dir>/meeting process <rec>`, in its own group when `group`, and reaps it
    /// in a thread so it does not linger as a zombie. Returns its pid once its `sleep`
    /// child has written its own pid to `<dir>/child`.
    fn start(dir: &Path, rec: &Path, group: bool) -> (u32, u32) {
        use std::os::unix::process::CommandExt;
        let mut command = Command::new(dir.join("meeting"));
        command.arg("process").arg(rec).arg("--language").arg("en");
        if group {
            command.process_group(0);
        }
        let mut child = command.spawn().unwrap();
        let pid = child.id();
        std::thread::spawn(move || child.wait());
        let deadline = Instant::now() + Duration::from_secs(5);
        let child = loop {
            if let Some(child) = fs::read_to_string(dir.join("child"))
                .ok()
                .and_then(|s| s.trim().parse().ok())
            {
                break child;
            }
            assert!(Instant::now() < deadline, "the stub never started");
            std::thread::sleep(Duration::from_millis(20));
        };
        (pid, child)
    }

    const SLEEPS: &str = "/bin/sleep 60 &\necho $! > \"$(/usr/bin/dirname \"$0\")/child\"\nwait";

    fn sleeper() -> std::process::Child {
        Command::new("/bin/sleep").arg("60").spawn().unwrap()
    }

    #[test]
    fn continue_stops_a_live_meeting_process_and_its_group() {
        let (dir, rec) = meeting_stub("continue-stop", SLEEPS);
        let (pid, child) = start(&dir, &rec, true);
        let lock = dir.join("m/lock");
        fs::write(&lock, pid.to_string()).unwrap();
        let started = Instant::now();
        stop_holder(&lock, &rec, GRACE).unwrap();
        assert!(started.elapsed() < GRACE, "{:?}", started.elapsed());
        assert!(!alive(pid));
        assert!(ended(child, GRACE), "the group's child survived");
        assert!(!lock.exists());
    }

    #[test]
    fn continue_kills_a_term_ignoring_run_after_the_grace_time() {
        let body = format!("trap '' TERM\n{SLEEPS}");
        let (dir, rec) = meeting_stub("continue-kill", &body);
        let (pid, child) = start(&dir, &rec, true);
        let lock = dir.join("m/lock");
        fs::write(&lock, pid.to_string()).unwrap();
        let started = Instant::now();
        stop_holder(&lock, &rec, GRACE).unwrap();
        assert!(started.elapsed() >= GRACE, "{:?}", started.elapsed());
        assert!(!alive(pid));
        assert!(ended(child, GRACE), "the group's child survived");
        assert!(!lock.exists());
    }

    #[test]
    fn continue_signals_only_the_pid_when_it_leads_no_group() {
        let (dir, rec) = meeting_stub("continue-pid", SLEEPS);
        let (pid, child) = start(&dir, &rec, false);
        let lock = dir.join("m/lock");
        fs::write(&lock, pid.to_string()).unwrap();
        stop_holder(&lock, &rec, GRACE).unwrap();
        assert!(!alive(pid));
        let child_alive = alive(child);
        send("KILL", Target::Pid(child));
        assert!(child_alive, "the shared group was signalled");
        assert!(!lock.exists());
    }

    #[test]
    fn continue_leaves_an_unrelated_pid_alone() {
        let (dir, rec) = meeting_stub("continue-unrelated", "");
        let mut sleep = sleeper();
        let lock = dir.join("m/lock");
        fs::write(&lock, sleep.id().to_string()).unwrap();
        stop_holder(&lock, &rec, GRACE).unwrap();
        let survived = alive(sleep.id());
        sleep.kill().unwrap();
        sleep.wait().unwrap();
        assert!(survived);
        assert!(!lock.exists());
    }

    #[test]
    fn continue_ignores_a_run_of_another_recording() {
        let (dir, rec) = meeting_stub("continue-other", SLEEPS);
        let other = dir.join("o/recording.webm");
        fs::create_dir_all(dir.join("o")).unwrap();
        fs::write(&other, "x").unwrap();
        let (pid, child) = start(&dir, &other, true);
        let lock = dir.join("m/lock");
        fs::write(&lock, pid.to_string()).unwrap();
        stop_holder(&lock, &rec, GRACE).unwrap();
        let survived = alive(pid);
        send("KILL", Target::Group(pid));
        assert!(survived);
        assert!(ended(child, GRACE));
        assert!(!lock.exists());
    }

    #[test]
    fn continue_with_a_dead_or_garbage_lock_does_nothing() {
        let (dir, rec) = meeting_stub("continue-dead", "");
        let lock = dir.join("m/lock");
        for held in [dead_pid().to_string(), "garbage".into()] {
            fs::write(&lock, &held).unwrap();
            stop_holder(&lock, &rec, GRACE).unwrap();
            assert_eq!(fs::read_to_string(&lock).unwrap(), held);
            drop(Lock::acquire(&lock).unwrap());
        }
        stop_holder(&lock, &rec, GRACE).unwrap();
        assert!(!lock.exists());
    }

    #[test]
    fn continue_never_signals_this_process() {
        let (dir, rec) = meeting_stub("continue-self", "");
        let lock = dir.join("m/lock");
        let _held = Lock::acquire(&lock).unwrap();
        stop_holder(&lock, &rec, GRACE).unwrap();
        assert!(is_held(&lock));
    }

    #[test]
    fn continue_identifies_a_meeting_process_run_of_the_recording() {
        let (dir, rec) = meeting_stub("continue-ident", "");
        let spaced = dir.join("my meeting");
        fs::create_dir_all(&spaced).unwrap();
        let spaced_rec = spaced.join("recording.webm");
        fs::write(&spaced_rec, "x").unwrap();
        let link = dir.join("link");
        std::os::unix::fs::symlink(dir.join("m"), &link).unwrap();
        let r = rec.display();
        let yes = [
            format!("/Applications/x/meeting process {r}"),
            format!("/bin/sh /a b/meeting process {r} --language en --job-id j"),
            format!("meeting process {r} --continue"),
            format!("/x/meeting process {}/recording.webm", link.display()),
        ];
        for command in &yes {
            assert!(is_process_run(command, &rec), "{command}");
        }
        assert!(is_process_run(
            &format!("/x/meeting process {} --retranscribe", spaced_rec.display()),
            &spaced_rec
        ));
        let no = [
            format!("/x/meeting import {r} --language en"),
            format!("/x/meeting processing {r}"),
            format!("/x/notmeetingx process {r}"),
            format!("/bin/sleep process {r}"),
            "/x/meeting process m/recording.webm".to_string(),
            format!("/x/meeting process {}", dir.join("m/other.webm").display()),
            "/bin/sleep 60".to_string(),
        ];
        for command in &no {
            assert!(!is_process_run(command, &rec), "{command}");
        }
    }

    #[test]
    fn path_sits_in_the_recording_folder() {
        assert_eq!(
            Lock::path_for(Path::new("/r/a/recording.webm")),
            PathBuf::from("/r/a/lock")
        );
    }
}
