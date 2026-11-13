//! Where the app finds the tool's files. Everything hangs off one home directory so tests can
//! point it at a temp dir.

use std::path::PathBuf;

use meeting::config::{self, Config};
use meeting::jobs::JobStore;
use meeting::store::{self, Store};

pub struct Env {
    pub home: PathBuf,
    pub store_path: PathBuf,
    pub jobs: JobStore,
}

impl Env {
    pub fn new() -> anyhow::Result<Env> {
        Ok(Env::at(config::home()?))
    }

    pub fn at(home: PathBuf) -> Env {
        let store_path = store::default_path(&home);
        // The jobs directory sits beside the store; a test pins it to `JobStore::default_dir`.
        let jobs = JobStore {
            dir: store_path.with_file_name("jobs"),
        };
        Env {
            home,
            store_path,
            jobs,
        }
    }

    fn config_path(&self) -> PathBuf {
        self.home.join(".config/meeting-manager/config.toml")
    }

    /// Reads `config.toml` on every call, so `meeting install` takes effect without a restart.
    pub fn config(&self) -> Result<Config, String> {
        Config::load_from(&self.config_path()).map_err(|e| format!("{e:#}"))
    }

    /// The error stays an `anyhow::Error` so a caller can downcast `store::OpenError`.
    pub fn store(&self) -> anyhow::Result<Store> {
        Store::open(&self.store_path)
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use meeting::store::OpenError;

    use super::*;

    fn temp_home(name: &str) -> PathBuf {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "meeting-desktop-env-{name}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_config(env: &Env, vault_folder: &str) {
        let config = Config {
            recordings_dir: env.home.join("Recordings"),
            vault_dir: env.home.join("Vault"),
            vault_folder: vault_folder.into(),
            ffmpeg_path: "/opt/homebrew/bin/ffmpeg".into(),
            claude_path: "/usr/local/bin/claude".into(),
            extension_id: "abcdefghijklmnopabcdefghijklmnop".into(),
        };
        config.save_to(&env.config_path()).unwrap();
    }

    #[test]
    fn the_real_paths_are_the_tools() {
        let env = Env::new().unwrap();
        assert_eq!(env.config_path(), Config::path().unwrap());
        assert_eq!(env.jobs.dir, JobStore::default_dir().unwrap());
        assert_eq!(env.store_path, store::default_path(&env.home));
    }

    #[test]
    fn a_missing_config_names_the_fix() {
        let env = Env::at(temp_home("missing"));
        let error = env.config().unwrap_err();
        fs::remove_dir_all(&env.home).unwrap();
        assert_eq!(error, "meeting is not configured; run `meeting install`");
    }

    #[test]
    fn a_changed_config_is_seen_on_the_next_call() {
        let env = Env::at(temp_home("changed"));
        write_config(&env, "Meetings");
        let first = env.config().unwrap();
        write_config(&env, "Calls");
        let second = env.config().unwrap();
        fs::remove_dir_all(&env.home).unwrap();
        assert_eq!(first.vault_folder, "Meetings");
        assert_eq!(second.vault_folder, "Calls");
    }

    #[test]
    fn an_invalid_config_is_an_error_string() {
        let env = Env::at(temp_home("invalid"));
        fs::create_dir_all(env.config_path().parent().unwrap()).unwrap();
        fs::write(env.config_path(), "not toml =").unwrap();
        let error = env.config().unwrap_err();
        fs::remove_dir_all(&env.home).unwrap();
        assert!(error.starts_with("invalid config "), "{error}");
    }

    #[test]
    fn the_store_opens_under_the_home() {
        let env = Env::at(temp_home("store"));
        let opened = env.store().is_ok();
        let exists = env.store_path.is_file();
        fs::remove_dir_all(&env.home).unwrap();
        assert!(opened);
        assert!(exists);
    }

    #[test]
    fn an_unopenable_store_keeps_its_typed_error() {
        let env = Env::at(temp_home("corrupt"));
        fs::create_dir_all(env.store_path.parent().unwrap()).unwrap();
        fs::write(&env.store_path, "this is not a database, not at all").unwrap();
        let error = env.store().err().expect("a corrupt file must not open");
        fs::remove_dir_all(&env.home).unwrap();
        assert_eq!(
            error.downcast_ref::<OpenError>(),
            Some(&OpenError::Corrupt),
            "{error:#}"
        );
    }
}
