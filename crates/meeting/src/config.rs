use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, anyhow};
use serde::{Deserialize, Serialize};

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

#[cfg(test)]
mod tests {
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
    fn missing_file_points_to_install() {
        let err = Config::load_from(&temp("missing.toml")).unwrap_err();
        assert!(err.to_string().contains("meeting install"));
    }
}
