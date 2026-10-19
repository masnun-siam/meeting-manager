use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde::Serialize;
use serde_json::{Value, json};

use crate::config::Config;

pub const HOST_NAME: &str = "com.meeting_manager.host";

#[derive(Serialize, Debug, PartialEq)]
pub struct Check {
    pub name: &'static str,
    pub ok: bool,
    pub fix: String,
}

/// What the checks look at. `main` passes the real ones; tests pass temp paths and fakes.
pub struct Env<'a> {
    pub config: &'a Result<Config, String>,
    pub key: fn() -> anyhow::Result<String>,
    pub manifest: PathBuf,
    pub exe: PathBuf,
}

/// Chrome's per-user location for native host manifests on macOS.
pub fn manifest_path(home: &Path) -> PathBuf {
    home.join(format!(
        "Library/Application Support/Google/Chrome/NativeMessagingHosts/{HOST_NAME}.json"
    ))
}

/// What `meeting install` (#27) writes, so the check and the installer can't drift.
pub fn expected_manifest(exe: &Path, extension_id: &str) -> Value {
    json!({
        "name": HOST_NAME,
        "description": "Meeting Manager",
        "path": exe,
        "type": "stdio",
        "allowed_origins": [format!("chrome-extension://{extension_id}/")],
    })
}

/// True when `claude auth status --json` exits 0 with `"loggedIn": true`.
/// Its output holds the account email, so it is parsed and never logged.
pub fn claude_logged_in(claude: &Path) -> bool {
    Command::new(claude)
        .args(["auth", "status", "--json"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| serde_json::from_slice::<Value>(&o.stdout).ok())
        .is_some_and(|v| v["loggedIn"] == true)
}

fn check(name: &'static str, ok: bool, fix: &str) -> Check {
    Check {
        name,
        ok,
        fix: if ok { String::new() } else { fix.into() },
    }
}

fn manifest_matches(env: &Env, config: &Config) -> bool {
    std::fs::read(&env.manifest)
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        .is_some_and(|v| v == expected_manifest(&env.exe, &config.extension_id))
}

pub fn checks(env: &Env) -> Vec<Check> {
    const INSTALL: &str = "Run `meeting install`";
    let key = check(
        "deepgram_key",
        (env.key)().is_ok(),
        "Run `meeting config set-key`",
    );
    let config = check("config", env.config.is_ok(), INSTALL);
    let rest = match env.config {
        Ok(c) => vec![
            check(
                "vault",
                c.vault_dir.is_dir(),
                &format!(
                    "Create {} or run `meeting install` again",
                    c.vault_dir.display()
                ),
            ),
            check(
                "ffmpeg",
                c.ffmpeg_path.is_file(),
                "Install ffmpeg (`brew install ffmpeg`), then run `meeting install`",
            ),
            check(
                "claude",
                c.claude_path.is_file(),
                "Install Claude Code, then run `meeting install`",
            ),
            check(
                "claude_login",
                claude_logged_in(&c.claude_path),
                "Run `claude auth login`",
            ),
            check("native_host", manifest_matches(env, c), INSTALL),
        ],
        Err(_) => ["vault", "ffmpeg", "claude", "claude_login", "native_host"]
            .map(|name| check(name, false, INSTALL))
            .into(),
    };
    let mut all = vec![config, key];
    all.extend(rest);
    all
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("meeting-test-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn script(path: &Path, body: &str) {
        fs::write(path, format!("#!/bin/sh\n{body}\n")).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn key_ok() -> anyhow::Result<String> {
        Ok("k".into())
    }

    fn key_missing() -> anyhow::Result<String> {
        anyhow::bail!("no key")
    }

    /// A fully working setup under `dir`.
    fn setup(dir: &Path) -> (Result<Config, String>, PathBuf, PathBuf) {
        let config = Config {
            recordings_dir: dir.join("rec"),
            vault_dir: dir.join("vault"),
            vault_folder: "Meetings".into(),
            ffmpeg_path: dir.join("ffmpeg"),
            claude_path: dir.join("claude"),
            extension_id: "ext".into(),
        };
        fs::create_dir_all(&config.vault_dir).unwrap();
        fs::write(&config.ffmpeg_path, "").unwrap();
        script(&config.claude_path, r#"echo '{"loggedIn": true}'"#);
        let exe = dir.join("meeting");
        let manifest = dir.join("host.json");
        fs::write(&manifest, expected_manifest(&exe, "ext").to_string()).unwrap();
        (Ok(config), manifest, exe)
    }

    fn run(
        config: &Result<Config, String>,
        key: fn() -> anyhow::Result<String>,
        manifest: &Path,
        exe: &Path,
    ) -> Vec<Check> {
        checks(&Env {
            config,
            key,
            manifest: manifest.into(),
            exe: exe.into(),
        })
    }

    fn failing(checks: &[Check]) -> Vec<(&'static str, String)> {
        checks
            .iter()
            .filter(|c| !c.ok)
            .map(|c| (c.name, c.fix.clone()))
            .collect()
    }

    fn fx(name: &'static str, fix: &str) -> (&'static str, String) {
        (name, fix.into())
    }

    #[test]
    fn all_pass_in_order_with_empty_fixes() {
        let dir = temp("doctor-ok");
        let (config, manifest, exe) = setup(&dir);
        let all = run(&config, key_ok, &manifest, &exe);
        fs::remove_dir_all(&dir).unwrap();
        let names: Vec<_> = all.iter().map(|c| c.name).collect();
        assert_eq!(
            names,
            [
                "config",
                "deepgram_key",
                "vault",
                "ffmpeg",
                "claude",
                "claude_login",
                "native_host"
            ]
        );
        assert!(all.iter().all(|c| c.ok && c.fix.is_empty()), "{all:?}");
    }

    #[test]
    fn each_failing_check_has_its_exact_fix() {
        let dir = temp("doctor-fail");
        let (config, manifest, exe) = setup(&dir);
        let c = config.as_ref().unwrap();
        let vault = format!(
            "Create {} or run `meeting install` again",
            c.vault_dir.display()
        );
        let key = failing(&run(&config, key_missing, &manifest, &exe));
        fs::remove_dir_all(&c.vault_dir).unwrap();
        let v = failing(&run(&config, key_ok, &manifest, &exe));
        fs::remove_file(&c.ffmpeg_path).unwrap();
        let f = failing(&run(&config, key_ok, &manifest, &exe));
        script(&c.claude_path, r#"echo '{"loggedIn": false}'"#);
        let l = failing(&run(&config, key_ok, &manifest, &exe));
        fs::remove_file(&c.claude_path).unwrap();
        let cl = failing(&run(&config, key_ok, &manifest, &exe));
        fs::write(&manifest, expected_manifest(&exe, "other").to_string()).unwrap();
        let n = failing(&run(&config, key_ok, &manifest, &exe));
        fs::remove_dir_all(&dir).unwrap();
        assert_eq!(key, [fx("deepgram_key", "Run `meeting config set-key`")]);
        assert_eq!(v, [fx("vault", &vault)]);
        assert_eq!(
            f[1..],
            [fx(
                "ffmpeg",
                "Install ffmpeg (`brew install ffmpeg`), then run `meeting install`"
            )]
        );
        assert_eq!(
            l.last(),
            Some(&fx("claude_login", "Run `claude auth login`"))
        );
        assert_eq!(
            cl[2..],
            [
                fx("claude", "Install Claude Code, then run `meeting install`"),
                fx("claude_login", "Run `claude auth login`"),
            ]
        );
        assert_eq!(n.last(), Some(&fx("native_host", "Run `meeting install`")));
    }

    #[test]
    fn missing_manifest_fails_native_host() {
        let dir = temp("doctor-nomanifest");
        let (config, manifest, exe) = setup(&dir);
        fs::remove_file(&manifest).unwrap();
        let bad = failing(&run(&config, key_ok, &manifest, &exe));
        fs::remove_dir_all(&dir).unwrap();
        assert_eq!(bad, [fx("native_host", "Run `meeting install`")]);
    }

    #[test]
    fn config_error_fails_dependents_but_still_checks_the_key() {
        let config = Err("meeting is not configured".to_string());
        let nowhere = Path::new("/nowhere");
        let with_key = run(&config, key_ok, nowhere, nowhere);
        let no_key = run(&config, key_missing, nowhere, nowhere);
        let failed = failing(&with_key);
        let names: Vec<_> = failed.iter().map(|f| f.0).collect();
        assert_eq!(
            names,
            [
                "config",
                "vault",
                "ffmpeg",
                "claude",
                "claude_login",
                "native_host"
            ]
        );
        assert!(
            failing(&with_key)
                .iter()
                .all(|f| f.1 == "Run `meeting install`")
        );
        assert_eq!(failing(&no_key).len(), 7);
    }

    #[test]
    fn claude_logged_in_needs_exit_zero_and_logged_in_true() {
        let dir = temp("doctor-login");
        let claude = dir.join("claude");
        let probe = |body: &str| {
            script(&claude, body);
            claude_logged_in(&claude)
        };
        let yes = probe(r#"echo '{"loggedIn": true, "email": "a@b"}'"#);
        let no = probe(r#"echo '{"loggedIn": false}'"#);
        let failed = probe(r#"echo '{"loggedIn": true}'; exit 1"#);
        let garbage = probe("echo not json");
        let missing = claude_logged_in(&dir.join("nope"));
        fs::remove_dir_all(&dir).unwrap();
        assert!(yes);
        assert!(!no && !failed && !garbage && !missing);
    }

    #[test]
    fn manifest_path_is_chromes_per_user_location() {
        assert_eq!(
            manifest_path(Path::new("/Users/a")),
            PathBuf::from(
                "/Users/a/Library/Application Support/Google/Chrome/NativeMessagingHosts/com.meeting_manager.host.json"
            )
        );
    }
}
