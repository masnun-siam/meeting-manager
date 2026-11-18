//! Moving a file to the macOS Trash with the system's own call, so Finder's Put Back works.
//! `/usr/bin/trash` needs macOS 15 and the app supports 14.4, hence `NSFileManager`.

use std::io;
use std::path::Path;

use objc2_foundation::{NSFileManager, NSString, NSURL};

/// Moves `path` to the user's Trash. The error carries the system's reason.
pub fn move_to_trash(path: &Path) -> io::Result<()> {
    let text = path
        .to_str()
        .ok_or_else(|| io::Error::other("the path is not valid UTF-8"))?;
    let url = NSURL::fileURLWithPath(&NSString::from_str(text));
    NSFileManager::defaultManager()
        .trashItemAtURL_resultingItemURL_error(&url, None)
        .map_err(|e| io::Error::other(e.localizedDescription().to_string()))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::ffi::OsStrExt;

    use super::*;

    #[test]
    fn a_missing_file_is_an_error_with_the_systems_reason() {
        let path = std::env::temp_dir().join(format!(
            "meeting-desktop-trash-missing-{}/recording.webm",
            std::process::id()
        ));
        let error = move_to_trash(&path).unwrap_err();
        assert!(!error.to_string().is_empty());
    }

    #[test]
    fn a_path_that_is_not_utf8_is_an_error() {
        let path = Path::new(std::ffi::OsStr::from_bytes(b"/tmp/\xff/recording.webm"));
        let error = move_to_trash(path).unwrap_err();
        assert_eq!(error.to_string(), "the path is not valid UTF-8");
    }

    /// Puts a temp file in the user's real Trash:
    /// `cargo test -p meeting-desktop notes::trash -- --ignored`.
    #[test]
    #[ignore]
    fn the_real_trash_takes_the_file() {
        let dir =
            std::env::temp_dir().join(format!("meeting-desktop-trash-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let name = format!("meeting-trash-test-{}.webm", std::process::id());
        let path = dir.join(&name);
        fs::write(&path, b"audio").unwrap();

        move_to_trash(&path).unwrap();
        assert!(!path.exists());
        let trash = Path::new(&std::env::var("HOME").unwrap()).join(".Trash");
        let found = fs::read_dir(&trash).unwrap().any(|e| {
            e.unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with("meeting-trash-test-")
        });
        assert!(found, "not in {}", trash.display());
        fs::remove_dir_all(&dir).unwrap();
    }
}
