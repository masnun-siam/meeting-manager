use std::fs;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, anyhow};
use serde::{Deserialize, Serialize};

use crate::join::part_number;

/// `~/.config/meeting-manager/config.toml`. Paths are absolute because Chrome
/// launches the host with a bare `PATH`. Holds no secrets: the key is in the Keychain.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct Config {
    pub recordings_dir: PathBuf,
    pub vault_dir: PathBuf,
    #[serde(default = "default_vault_folder")]
    pub vault_folder: String,
    pub ffmpeg_path: PathBuf,
    pub claude_path: PathBuf,
    pub extension_id: String,
    /// Absolute path of the `meeting` binary the desktop app spawns. Written by `install`
    /// only in a `FEATURE_DESKTOP_APP` build; absent otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub meeting_path: Option<PathBuf>,
}

fn default_vault_folder() -> String {
    "Meetings".into()
}

pub fn home() -> anyhow::Result<PathBuf> {
    std::env::home_dir().ok_or_else(|| anyhow!("cannot find the home directory"))
}

impl Config {
    pub fn path() -> anyhow::Result<PathBuf> {
        Ok(home()?.join(".config/meeting-manager/config.toml"))
    }

    pub fn load() -> anyhow::Result<Config> {
        Self::load_from(&Self::path()?)
    }

    pub fn load_from(path: &Path) -> anyhow::Result<Config> {
        let text = match fs::read_to_string(path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(anyhow!("meeting is not configured; run `meeting install`"));
            }
            r => r.with_context(|| format!("cannot read {}", path.display()))?,
        };
        toml::from_str(&text).with_context(|| format!("invalid config {}", path.display()))
    }

    pub fn save_to(&self, path: &Path) -> anyhow::Result<()> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
        }
        fs::write(path, toml::to_string(self)?)
            .with_context(|| format!("cannot write {}", path.display()))
    }
}

/// The extensions a meeting recording may have (`recording.<ext>`, lower case).
pub const RECORDING_EXTENSIONS: [&str; 8] =
    ["webm", "mp4", "m4a", "mov", "mkv", "mp3", "wav", "ogg"];

/// The canonical path of an existing `recording.<ext>` (ext in `RECORDING_EXTENSIONS`) whose
/// folder sits directly inside `recordings_dir` or its `Archive/`.
pub fn check_recording(recordings_dir: &Path, path: &Path) -> Result<PathBuf, String> {
    if !path.is_absolute() {
        return Err(format!("{} is not an absolute path", path.display()));
    }
    let canonical = fs::canonicalize(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if !canonical.is_file() {
        return Err(format!("{} is not a file", path.display()));
    }
    check_layout(recordings_dir, path, canonical)
}

/// As `check_recording`, for a recording whose file is gone but whose folder is still there
/// (the app deleted it, FR-10): the folder is canonicalised and the file name joined to it.
pub fn check_recording_folder(recordings_dir: &Path, path: &Path) -> Result<PathBuf, String> {
    if !path.is_absolute() {
        return Err(format!("{} is not an absolute path", path.display()));
    }
    let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
        return Err(format!("{} is not a file", path.display()));
    };
    let folder = fs::canonicalize(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    if !folder.is_dir() {
        return Err(format!("{} is not a folder", parent.display()));
    }
    check_layout(recordings_dir, path, folder.join(name))
}

/// The canonical `recording.<ext>` a continued part belongs to. `path` must be an existing
/// `recording-part-N.<ext>` (N >= 2, a regular file, no `..`) in a meeting folder directly
/// inside `recordings_dir` (not a symlink, not under `Archive/`), with a `recording.<same
/// ext>` that is a real file of that folder.
pub fn check_part(recordings_dir: &Path, path: &Path) -> Result<PathBuf, String> {
    if !path.is_absolute() {
        return Err(format!("{} is not an absolute path", path.display()));
    }
    if path.components().any(|c| c == Component::ParentDir) {
        return Err(format!(
            "{} contains ..; send the folder's own path",
            path.display()
        ));
    }
    let (Some(parent), Some(name)) = (path.parent(), path.file_name().and_then(|n| n.to_str()))
    else {
        return Err(format!("{} is not a file", path.display()));
    };
    let Some(ext) = RECORDING_EXTENSIONS
        .into_iter()
        .find(|ext| part_number(name, ext).is_some())
    else {
        return Err(format!(
            "{} is not a part; a part is recording-part-N.<ext> with N >= 2",
            path.display()
        ));
    };
    let part = fs::symlink_metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if !part.file_type().is_file() {
        return Err(format!("{} is not a regular file", path.display()));
    }
    let folder = fs::symlink_metadata(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    if !folder.file_type().is_dir() {
        return Err(format!(
            "{} is not a folder; a symlinked meeting folder is refused",
            parent.display()
        ));
    }
    let folder = fs::canonicalize(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    let recording = check_recording(recordings_dir, &folder.join(format!("recording.{ext}")))?;
    if recording.parent() != Some(folder.as_path()) {
        return Err(format!(
            "{} is not a file of {}; replace it with the recorded file",
            recording.display(),
            parent.display()
        ));
    }
    let root = fs::canonicalize(recordings_dir)
        .map_err(|e| format!("{}: {e}", recordings_dir.display()))?;
    if folder.parent() != Some(root.as_path()) {
        return Err(format!(
            "{} is archived; continue only a recording that is not in Archive/",
            parent.display()
        ));
    }
    Ok(recording)
}

/// The rules both checks share: `canonical` is `recording.<ext>` directly inside
/// `<recordings_dir>/<name>` or `<recordings_dir>/Archive/<name>`. `path` is what the caller
/// gave, for the messages.
fn check_layout(recordings_dir: &Path, path: &Path, canonical: PathBuf) -> Result<PathBuf, String> {
    let root = fs::canonicalize(recordings_dir)
        .map_err(|e| format!("{}: {e}", recordings_dir.display()))?;
    if !canonical.starts_with(&root) {
        return Err(format!(
            "{} is outside the recordings folder",
            path.display()
        ));
    }
    let archive = root.join("Archive");
    let folder = canonical.parent();
    let named = canonical.file_stem().is_some_and(|s| s == "recording")
        && canonical
            .extension()
            .is_some_and(|e| RECORDING_EXTENSIONS.iter().any(|x| e == *x));
    let foldered = named
        && folder != Some(&archive)
        && folder
            .and_then(Path::parent)
            .is_some_and(|p| p == root || p == archive);
    if !foldered {
        return Err(format!(
            "{} is not in its own folder; move it to {}/<name>/recording.<ext>, or run meeting import {} --language bn|en",
            path.display(),
            recordings_dir.display(),
            path.display()
        ));
    }
    Ok(canonical)
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::symlink;

    use super::*;

    fn temp(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("meeting-test-{}-{name}", std::process::id()))
    }

    fn sample() -> Config {
        Config {
            recordings_dir: "/Users/a/Downloads/Meetings".into(),
            vault_dir: "/Users/a/Vault".into(),
            vault_folder: "Calls".into(),
            ffmpeg_path: "/opt/homebrew/bin/ffmpeg".into(),
            claude_path: "/Users/a/.local/bin/claude".into(),
            extension_id: "obbaikcmcdooakogedoiiflflbbhpgdh".into(),
            meeting_path: None,
        }
    }

    #[test]
    fn save_then_load_round_trips() {
        let dir = temp("round-trip");
        let path = dir.join("nested/config.toml");
        sample().save_to(&path).unwrap();
        let loaded = Config::load_from(&path).unwrap();
        fs::remove_dir_all(&dir).unwrap();
        assert_eq!(loaded, sample());
    }

    #[test]
    fn vault_folder_defaults_to_meetings() {
        let config: Config = toml::from_str(
            r#"
            recordings_dir = "/r"
            vault_dir = "/v"
            ffmpeg_path = "/f"
            claude_path = "/c"
            extension_id = "id"
            "#,
        )
        .unwrap();
        assert_eq!(config.vault_folder, "Meetings");
    }

    #[test]
    fn a_config_without_meeting_path_serialises_as_before() {
        assert_eq!(
            toml::to_string(&sample()).unwrap(),
            r#"recordings_dir = "/Users/a/Downloads/Meetings"
vault_dir = "/Users/a/Vault"
vault_folder = "Calls"
ffmpeg_path = "/opt/homebrew/bin/ffmpeg"
claude_path = "/Users/a/.local/bin/claude"
extension_id = "obbaikcmcdooakogedoiiflflbbhpgdh"
"#
        );
    }

    #[test]
    fn meeting_path_round_trips() {
        let dir = temp("meeting-path");
        let path = dir.join("config.toml");
        let config = Config {
            meeting_path: Some("/Users/a/.cargo/bin/meeting".into()),
            ..sample()
        };
        config.save_to(&path).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        let loaded = Config::load_from(&path).unwrap();
        fs::remove_dir_all(&dir).unwrap();
        assert!(text.ends_with("meeting_path = \"/Users/a/.cargo/bin/meeting\"\n"));
        assert_eq!(loaded, config);
    }

    #[test]
    fn an_old_config_file_loads_with_no_meeting_path() {
        let config: Config = toml::from_str(
            r#"
            recordings_dir = "/r"
            vault_dir = "/v"
            vault_folder = "Meetings"
            ffmpeg_path = "/f"
            claude_path = "/c"
            extension_id = "id"
            "#,
        )
        .unwrap();
        assert_eq!(config.meeting_path, None);
    }

    #[test]
    fn missing_file_points_to_install() {
        let err = Config::load_from(&temp("missing.toml")).unwrap_err();
        assert!(err.to_string().contains("meeting install"));
    }

    #[test]
    fn check_recording_accepts_only_foldered_recordings() {
        let dir = temp("config-check");
        let _ = fs::remove_dir_all(&dir);
        let rec = dir.join("rec");
        for sub in [
            "m",
            "Archive/m",
            "a/b",
            "Archive/a/b",
            "Archive",
            "dir/recording.webm",
            "upper",
        ] {
            fs::create_dir_all(rec.join(sub)).unwrap();
        }
        for file in [
            "m/recording.webm",
            "Archive/m/recording.webm",
            "x.webm",
            "m/other.webm",
            "Archive/recording.webm",
            "recording.webm",
            "a/b/recording.webm",
            "Archive/a/b/recording.webm",
        ] {
            fs::write(rec.join(file), "x").unwrap();
        }
        for ext in RECORDING_EXTENSIONS {
            for sub in ["m", "Archive/m"] {
                fs::write(rec.join(sub).join(format!("recording.{ext}")), "x").unwrap();
            }
        }
        for file in [
            "m/recording.txt",
            "x.m4a",
            "m/recording.m4a.part",
            "upper/recording.M4A",
        ] {
            fs::write(rec.join(file), "x").unwrap();
        }
        fs::write(dir.join("outside.webm"), "x").unwrap();
        fs::create_dir_all(dir.join("elsewhere")).unwrap();
        fs::write(dir.join("elsewhere/recording.webm"), "x").unwrap();
        fs::create_dir_all(rec.join("l")).unwrap();
        symlink(dir.join("outside.webm"), rec.join("l/recording.webm")).unwrap();
        symlink(dir.join("elsewhere"), rec.join("linked")).unwrap();

        let canon = |p: &str| fs::canonicalize(rec.join(p)).unwrap();
        let want = (canon("m/recording.webm"), canon("Archive/m/recording.webm"));
        let check = |p: &Path| check_recording(&rec, p);
        let accepted = (
            check(&rec.join("m/recording.webm")),
            check(&rec.join("Archive/m/recording.webm")),
        );
        let refused = [
            rec.join("x.webm"),
            rec.join("m/other.webm"),
            rec.join("Archive/recording.webm"),
            rec.join("recording.webm"),
            rec.join("a/b/recording.webm"),
            rec.join("Archive/a/b/recording.webm"),
            rec.join("m/missing.webm"),
            rec.join("dir/recording.webm"),
            PathBuf::from("rec/m/recording.webm"),
            dir.join("elsewhere/recording.webm"),
            rec.join("l/recording.webm"),
            rec.join("linked/recording.webm"),
            rec.join("m/recording.txt"),
            rec.join("x.m4a"),
            rec.join("m/recording.m4a.part"),
            rec.join("upper/recording.M4A"),
        ]
        .map(|p| check(&p));
        let flat = check(&rec.join("x.webm")).unwrap_err();
        let every_ext: Vec<_> = RECORDING_EXTENSIONS
            .iter()
            .flat_map(|e| {
                ["m", "Archive/m"].map(|sub| check(&rec.join(sub).join(format!("recording.{e}"))))
            })
            .collect();
        fs::remove_dir_all(&dir).unwrap();

        assert_eq!(accepted.0, Ok(want.0));
        assert_eq!(accepted.1, Ok(want.1));
        for r in refused {
            assert!(r.is_err(), "{r:?}");
        }
        assert!(
            flat.contains("is not in its own folder; move it to "),
            "{flat}"
        );
        assert!(flat.contains("meeting import"), "{flat}");
        assert!(every_ext.iter().all(Result::is_ok), "{every_ext:?}");
    }

    #[test]
    fn check_part_accepts_only_a_part_beside_its_recording() {
        let dir = temp("config-part");
        let _ = fs::remove_dir_all(&dir);
        let rec = dir.join("rec");
        for sub in ["m", "a", "Archive/m", "x/y", "only", "real", "dir", "two"] {
            fs::create_dir_all(rec.join(sub)).unwrap();
        }
        for file in [
            "m/recording.webm",
            "m/recording-part-2.webm",
            "m/recording-part-10.webm",
            "m/recording-part-1.webm",
            "m/recording-part-0.webm",
            "m/recording-part-x.webm",
            "m/recording-part-2.webm.part",
            "m/recording-part-2.partial.wav",
            "m/recording-part-2.txt",
            "m/recording-part-3.m4a",
            "m/recording-part-4.WEBM",
            "a/recording.m4a",
            "a/recording-part-2.m4a",
            "Archive/m/recording.webm",
            "Archive/m/recording-part-2.webm",
            "x/y/recording.webm",
            "x/y/recording-part-2.webm",
            "recording.webm",
            "recording-part-2.webm",
            "only/recording-part-2.webm",
            "two/recording-part-2.webm",
            "two/other.webm",
        ] {
            fs::write(rec.join(file), "x").unwrap();
        }
        fs::create_dir_all(rec.join("dir/recording-part-2.webm")).unwrap();
        fs::write(rec.join("dir/recording.webm"), "x").unwrap();
        fs::write(dir.join("outside-part.webm"), "x").unwrap();
        fs::create_dir_all(dir.join("elsewhere")).unwrap();
        for file in ["recording.webm", "recording-part-2.webm"] {
            fs::write(dir.join("elsewhere").join(file), "x").unwrap();
        }
        // Symlinked parts, inside and outside the folder, and symlinked meeting folders.
        symlink(
            dir.join("outside-part.webm"),
            rec.join("m/recording-part-5.webm"),
        )
        .unwrap();
        symlink(
            rec.join("m/recording-part-2.webm"),
            rec.join("m/recording-part-6.webm"),
        )
        .unwrap();
        symlink(dir.join("elsewhere"), rec.join("out")).unwrap();
        symlink(rec.join("m"), rec.join("in")).unwrap();
        // A recording that is a symlink to another meeting's recording.
        fs::write(rec.join("real/recording-part-2.webm"), "x").unwrap();
        symlink(
            rec.join("m/recording.webm"),
            rec.join("real/recording.webm"),
        )
        .unwrap();

        let check = |p: &str| check_part(&rec, &rec.join(p));
        let accepted = (
            check("m/recording-part-2.webm"),
            check("m/recording-part-10.webm"),
            check("a/recording-part-2.m4a"),
        );
        let refused = [
            check_part(&rec, Path::new("rec/m/recording-part-2.webm")),
            check("m/recording-part-7.webm"),
            check("dir/recording-part-2.webm"),
            check_part(&rec, &dir.join("elsewhere/recording-part-2.webm")),
            check("m/recording-part-1.webm"),
            check("m/recording-part-0.webm"),
            check("m/recording-part-x.webm"),
            check("m/recording-part-2.webm.part"),
            check("m/recording-part-2.partial.wav"),
            check("m/recording-part-2.txt"),
            check("only/recording-part-2.webm"),
            check("m/recording-part-3.m4a"),
            check("recording-part-2.webm"),
            check("x/y/recording-part-2.webm"),
            check("Archive/m/recording-part-2.webm"),
            check("m/../m/recording-part-2.webm"),
            check("m/recording-part-5.webm"),
            check("m/recording-part-6.webm"),
            check("out/recording-part-2.webm"),
            check("in/recording-part-2.webm"),
            check("m/recording-part-4.WEBM"),
            check("real/recording-part-2.webm"),
            check("two/recording-part-2.webm"),
        ];
        let archived = check("Archive/m/recording-part-2.webm").unwrap_err();
        let want = (
            fs::canonicalize(rec.join("m/recording.webm")).unwrap(),
            fs::canonicalize(rec.join("a/recording.m4a")).unwrap(),
        );
        fs::remove_dir_all(&dir).unwrap();

        assert_eq!(accepted.0, Ok(want.0.clone()));
        assert_eq!(accepted.1, Ok(want.0));
        assert_eq!(accepted.2, Ok(want.1));
        for (i, r) in refused.iter().enumerate() {
            assert!(r.is_err(), "case {i}: {r:?}");
        }
        assert!(archived.contains("is archived"), "{archived}");
    }

    #[test]
    fn check_recording_folder_accepts_an_own_folder_without_the_file() {
        let dir = temp("config-check-folder");
        let _ = fs::remove_dir_all(&dir);
        let rec = dir.join("rec");
        for sub in ["m", "Archive/m", "a/b", "Archive/a/b"] {
            fs::create_dir_all(rec.join(sub)).unwrap();
        }
        fs::create_dir_all(dir.join("elsewhere")).unwrap();
        symlink(dir.join("elsewhere"), rec.join("linked")).unwrap();
        fs::write(rec.join("file"), "x").unwrap();

        let check = |p: &str| check_recording_folder(&rec, &rec.join(p));
        let canon = |p: &str| {
            fs::canonicalize(rec.join(p))
                .unwrap()
                .join("recording.webm")
        };
        let want = (canon("m"), canon("Archive/m"));
        let accepted = (check("m/recording.webm"), check("Archive/m/recording.webm"));
        let m4a = check("m/recording.m4a");
        let refused = [
            check("recording.webm"),
            check("Archive/recording.webm"),
            check("a/b/recording.webm"),
            check("Archive/a/b/recording.webm"),
            check("m/other.webm"),
            check("m/recording.txt"),
            check("linked/recording.webm"),
            check("gone/recording.webm"),
            check("file/recording.webm"),
            check_recording_folder(&rec, &dir.join("elsewhere/recording.webm")),
            check_recording_folder(&rec, Path::new("rec/m/recording.webm")),
        ];
        let flat = check("recording.webm").unwrap_err();
        fs::remove_dir_all(&dir).unwrap();

        assert_eq!(accepted.0, Ok(want.0));
        assert_eq!(accepted.1, Ok(want.1));
        assert!(m4a.is_ok(), "{m4a:?}");
        for r in refused {
            assert!(r.is_err(), "{r:?}");
        }
        assert!(flat.contains("is not in its own folder"), "{flat}");
    }
}
