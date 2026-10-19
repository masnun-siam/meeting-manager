use std::fs::{self, OpenOptions};
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, bail};

const BUSY: &str = "already processing";

/// `<name>.lock` next to a recording, holding the PID of the job that runs it.
/// Removed on drop; one left by a crashed process is taken over.
#[derive(Debug)]
pub struct Lock {
    path: PathBuf,
}

impl Lock {
    pub fn path_for(recording: &Path) -> PathBuf {
        recording.with_extension("lock")
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

    #[test]
    fn path_sits_next_to_the_recording() {
        assert_eq!(
            Lock::path_for(Path::new("/r/a.webm")),
            PathBuf::from("/r/a.lock")
        );
    }
}
