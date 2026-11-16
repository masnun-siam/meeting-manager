//! FR-27: the `meeting` tool follows the app. The binary bundled in `Meeting.app` replaces the
//! file at `meeting_path` when they differ, by a write to a temporary file in the same directory
//! and a rename, so a reader sees the old or the new file, never a partial one (D47). A job
//! already running keeps the old file open until it exits.

use std::fs::{self, File, OpenOptions, Permissions};
use std::io::{self, BufReader, Read};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

/// What the swap did; an `Err` from `swap` is the message shown in Settings › Updates.
#[derive(Debug, PartialEq)]
pub enum Swap {
    Replaced,
    Same,
    /// No `meeting_path`, or nothing there: FR-6's message stands.
    NoTarget,
    /// A local build carries no tool (D50).
    NoBundled,
}

impl Swap {
    pub fn as_str(&self) -> &'static str {
        match self {
            Swap::Replaced => "replaced",
            Swap::Same => "same",
            Swap::NoTarget => "no target",
            Swap::NoBundled => "no bundled tool",
        }
    }
}

/// Replaces `target` with `bundled` when their bytes differ. A failure leaves `target` as it
/// was and no temporary file behind.
pub fn swap(bundled: &Path, target: Option<&Path>) -> Result<Swap, String> {
    if !bundled.is_file() {
        return Ok(Swap::NoBundled);
    }
    let Some(target) = target.filter(|t| t.is_file()) else {
        return Ok(Swap::NoTarget);
    };
    // A target that can't be read is replaced rather than trusted.
    if same_file(bundled, target).unwrap_or(false) {
        return Ok(Swap::Same);
    }
    let dir = target.parent().unwrap_or(Path::new("/"));
    replace(bundled, target, dir).map_err(|e| {
        tracing::warn!(kind = ?e.kind(), "meeting tool not replaced");
        format!(
            "The meeting tool wasn't updated: {e}. Make {} writable or run `meeting install` \
             from a writable location",
            dir.display()
        )
    })?;
    Ok(Swap::Replaced)
}

/// The same path, or the same length and bytes.
fn same_file(a: &Path, b: &Path) -> io::Result<bool> {
    if fs::canonicalize(a)? == fs::canonicalize(b)? {
        return Ok(true);
    }
    if fs::metadata(a)?.len() != fs::metadata(b)?.len() {
        return Ok(false);
    }
    let (mut a, mut b) = (
        BufReader::new(File::open(a)?),
        BufReader::new(File::open(b)?),
    );
    let (mut chunk_a, mut chunk_b) = (vec![0; 64 * 1024], vec![0; 64 * 1024]);
    loop {
        let read = a.read(&mut chunk_a)?;
        if read == 0 {
            return Ok(true);
        }
        b.read_exact(&mut chunk_b[..read])?;
        if chunk_a[..read] != chunk_b[..read] {
            return Ok(false);
        }
    }
}

/// `bundled` copied to `<dir>/.meeting.update-<pid>` with mode 0755, then renamed over `target`.
fn replace(bundled: &Path, target: &Path, dir: &Path) -> io::Result<()> {
    let temp = temp_path(dir);
    let written = write_temp(bundled, &temp).and_then(|()| fs::rename(&temp, target));
    if written.is_err() {
        let _ = fs::remove_file(&temp);
    }
    written
}

fn temp_path(dir: &Path) -> PathBuf {
    dir.join(format!(".meeting.update-{}", std::process::id()))
}

fn write_temp(bundled: &Path, temp: &Path) -> io::Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o755)
        .open(temp)?;
    // The mode given above is narrowed by the umask.
    file.set_permissions(Permissions::from_mode(0o755))?;
    io::copy(&mut File::open(bundled)?, &mut file)?;
    file.sync_all()
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    /// A real temp directory, removed on drop.
    struct Dir(PathBuf);

    impl Dir {
        fn new(name: &str) -> Dir {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let dir = std::env::temp_dir().join(format!(
                "meeting-desktop-tool-{name}-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(dir.join("app")).unwrap();
            fs::create_dir_all(dir.join("bin")).unwrap();
            Dir(dir)
        }

        /// `app/meeting`, the bundled tool.
        fn bundled(&self, bytes: &[u8]) -> PathBuf {
            let path = self.0.join("app/meeting");
            fs::write(&path, bytes).unwrap();
            path
        }

        /// `bin/meeting`, the installed tool, mode 0700.
        fn target(&self, bytes: &[u8]) -> PathBuf {
            let path = self.bin().join("meeting");
            fs::write(&path, bytes).unwrap();
            fs::set_permissions(&path, Permissions::from_mode(0o700)).unwrap();
            path
        }

        fn bin(&self) -> PathBuf {
            self.0.join("bin")
        }

        fn listing(&self) -> Vec<String> {
            let mut names: Vec<String> = fs::read_dir(self.bin())
                .unwrap()
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect();
            names.sort();
            names
        }
    }

    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = fs::set_permissions(self.bin(), Permissions::from_mode(0o755));
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn mode(path: &Path) -> u32 {
        fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn a_different_tool_is_replaced_with_mode_0755_and_no_temp_left() {
        let dir = Dir::new("differs");
        let bundled = dir.bundled(b"meeting 0.2.0");
        let target = dir.target(b"meeting 0.1.0");
        assert_eq!(swap(&bundled, Some(&target)), Ok(Swap::Replaced));
        assert_eq!(fs::read(&target).unwrap(), b"meeting 0.2.0");
        assert_eq!(mode(&target), 0o755);
        assert_eq!(dir.listing(), ["meeting"]);
    }

    #[test]
    fn a_same_length_tool_with_other_bytes_is_replaced() {
        let dir = Dir::new("same-length");
        let bundled = dir.bundled(b"meeting 0.2.0");
        let target = dir.target(b"meeting 0.1.0");
        assert_eq!(swap(&bundled, Some(&target)), Ok(Swap::Replaced));
        assert_eq!(fs::read(&target).unwrap(), b"meeting 0.2.0");
    }

    #[test]
    fn an_equal_tool_is_not_written() {
        let dir = Dir::new("equal");
        let bundled = dir.bundled(b"meeting 0.2.0");
        let target = dir.target(b"meeting 0.2.0");
        let before = fs::metadata(&target).unwrap().modified().unwrap();
        assert_eq!(swap(&bundled, Some(&target)), Ok(Swap::Same));
        assert_eq!(fs::metadata(&target).unwrap().modified().unwrap(), before);
        assert_eq!(mode(&target), 0o700);
    }

    #[test]
    fn a_target_that_is_the_bundled_file_is_the_same() {
        let dir = Dir::new("itself");
        let bundled = dir.bundled(b"meeting 0.2.0");
        assert_eq!(swap(&bundled, Some(&bundled)), Ok(Swap::Same));
    }

    #[test]
    fn no_target_or_a_missing_one_writes_nothing() {
        let dir = Dir::new("no-target");
        let bundled = dir.bundled(b"meeting 0.2.0");
        assert_eq!(swap(&bundled, None), Ok(Swap::NoTarget));
        let missing = dir.bin().join("meeting");
        assert_eq!(swap(&bundled, Some(&missing)), Ok(Swap::NoTarget));
        // A directory is not a regular file either.
        assert_eq!(swap(&bundled, Some(&dir.bin())), Ok(Swap::NoTarget));
        assert!(dir.listing().is_empty());
    }

    #[test]
    fn no_bundled_tool_writes_nothing() {
        let dir = Dir::new("no-bundled");
        let target = dir.target(b"meeting 0.1.0");
        let bundled = dir.0.join("app/meeting");
        assert_eq!(swap(&bundled, Some(&target)), Ok(Swap::NoBundled));
        assert_eq!(fs::read(&target).unwrap(), b"meeting 0.1.0");
    }

    #[test]
    fn a_read_only_directory_keeps_the_old_tool_and_says_how_to_fix_it() {
        let dir = Dir::new("read-only");
        let bundled = dir.bundled(b"meeting 0.2.0");
        let target = dir.target(b"meeting 0.1.0");
        fs::set_permissions(dir.bin(), Permissions::from_mode(0o555)).unwrap();
        let error = swap(&bundled, Some(&target)).unwrap_err();
        fs::set_permissions(dir.bin(), Permissions::from_mode(0o755)).unwrap();
        assert_eq!(
            error,
            format!(
                "The meeting tool wasn't updated: Permission denied (os error 13). Make {} \
                 writable or run `meeting install` from a writable location",
                dir.bin().display()
            )
        );
        assert_eq!(fs::read(&target).unwrap(), b"meeting 0.1.0");
        assert_eq!(dir.listing(), ["meeting"]);
        // The next launch tries again.
        assert_eq!(swap(&bundled, Some(&target)), Ok(Swap::Replaced));
    }

    #[test]
    fn a_failed_rename_removes_the_temp_file() {
        let dir = Dir::new("rename-fails");
        let bundled = dir.bundled(b"meeting 0.2.0");
        // A non-empty directory where the target should be: the rename fails after the write.
        let target = dir.bin().join("meeting");
        fs::create_dir_all(target.join("inside")).unwrap();
        assert!(replace(&bundled, &target, &dir.bin()).is_err());
        assert_eq!(dir.listing(), ["meeting"]);
        assert!(target.is_dir());
    }

    #[test]
    fn a_job_holding_the_old_tool_still_reads_the_old_bytes() {
        let dir = Dir::new("open-handle");
        let bundled = dir.bundled(b"meeting 0.2.0");
        let target = dir.target(b"meeting 0.1.0");
        let mut running = File::open(&target).unwrap();
        assert_eq!(swap(&bundled, Some(&target)), Ok(Swap::Replaced));
        let mut old = Vec::new();
        running.read_to_end(&mut old).unwrap();
        assert_eq!(old, b"meeting 0.1.0");
        assert_eq!(fs::read(&target).unwrap(), b"meeting 0.2.0");
    }
}
