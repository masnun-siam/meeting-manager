use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde::Serialize;
use serde_json::{Value, json};

use crate::calendar_fetch::FetchError;
use crate::config::Config;
use crate::ical;

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
    /// Whether `FEATURE_MEETING_CONTEXT` was set at build time; off leaves out `calendar`.
    pub context: bool,
    /// `FEATURE_DESKTOP_APP`: off, no store is touched.
    pub desktop: bool,
    pub calendar: fn() -> anyhow::Result<Option<String>>,
    pub fetch: fn(&str) -> Result<String, FetchError>,
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
    if env.context {
        all.push(calendar(env));
    }
    all
}

/// Passes when no address is stored or the stored one fetches and parses. The fix names the
/// cause, never the address.
fn calendar(env: &Env) -> Check {
    const RUN: &str = "run `meeting config set-calendar`";
    let fix = match (env.calendar)() {
        Ok(None) => None,
        Ok(Some(address)) => match (env.fetch)(&address).map(|text| ical::parse(&text)) {
            Ok(Ok(_)) => None,
            Ok(Err(_)) | Err(FetchError::NotIcalendar) => {
                Some(format!("Calendar address is not an iCalendar feed; {RUN}"))
            }
            Err(FetchError::Offline) => {
                Some(format!("Calendar is offline; check the network or {RUN}"))
            }
            Err(e @ FetchError::Http(_)) => {
                Some(format!("Calendar address was rejected ({e}); {RUN}"))
            }
            Err(e) => Some(format!("Calendar check failed ({e}); {RUN}")),
        },
        Err(_) => Some(format!("Could not read the Keychain; {RUN}")),
    };
    check("calendar", fix.is_none(), &fix.unwrap_or_default())
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
            context: false,
            desktop: false,
            calendar: no_calendar,
            fetch: no_fetch,
        })
    }

    const SENTINEL: &str = "SENTINEL-SECRET-42";
    const ICS: &str = "BEGIN:VCALENDAR\r\nEND:VCALENDAR\r\n";

    fn no_calendar() -> anyhow::Result<Option<String>> {
        Ok(None)
    }

    fn stored() -> anyhow::Result<Option<String>> {
        Ok(Some(format!("https://x/{SENTINEL}/basic.ics")))
    }

    fn locked() -> anyhow::Result<Option<String>> {
        anyhow::bail!("cannot read the Keychain: locked")
    }

    fn no_fetch(_: &str) -> Result<String, FetchError> {
        panic!("no address is stored")
    }

    fn feed(address: &str) -> Result<String, FetchError> {
        assert!(address.contains(SENTINEL));
        Ok(ICS.into())
    }

    fn truncated(_: &str) -> Result<String, FetchError> {
        Ok("BEGIN:VCALENDAR\r\n".into())
    }

    fn offline(_: &str) -> Result<String, FetchError> {
        Err(FetchError::Offline)
    }

    fn rejected(_: &str) -> Result<String, FetchError> {
        Err(FetchError::Http(403))
    }

    fn html(_: &str) -> Result<String, FetchError> {
        Err(FetchError::NotIcalendar)
    }

    fn tls(_: &str) -> Result<String, FetchError> {
        Err(FetchError::Tls)
    }

    /// The checks with the gate on and a working setup except for the calendar.
    fn with_calendar(
        config: &Result<Config, String>,
        calendar: fn() -> anyhow::Result<Option<String>>,
        fetch: fn(&str) -> Result<String, FetchError>,
    ) -> Vec<Check> {
        let nowhere = Path::new("/nowhere");
        checks(&Env {
            config,
            key: key_ok,
            manifest: nowhere.into(),
            exe: nowhere.into(),
            context: true,
            desktop: false,
            calendar,
            fetch,
        })
    }

    fn calendar_check(
        calendar: fn() -> anyhow::Result<Option<String>>,
        fetch: fn(&str) -> Result<String, FetchError>,
    ) -> Check {
        let c = with_calendar(&Err("no config".into()), calendar, fetch)
            .pop()
            .unwrap();
        assert_eq!(c.name, "calendar");
        assert!(!c.fix.contains(SENTINEL), "{}", c.fix);
        c
    }

    #[test]
    fn calendar_is_last_and_only_with_the_gate_on() {
        let dir = temp("doctor-calendar");
        let (config, manifest, exe) = setup(&dir);
        let off = run(&config, key_ok, &manifest, &exe);
        let on = checks(&Env {
            config: &config,
            key: key_ok,
            manifest,
            exe,
            context: true,
            desktop: false,
            calendar: stored,
            fetch: feed,
        });
        fs::remove_dir_all(&dir).unwrap();
        assert_eq!(off.len(), 7);
        assert!(off.iter().all(|c| c.name != "calendar"));
        assert_eq!(on[..7], off[..]);
        assert_eq!(on.len(), 8);
        assert_eq!(on[7], check("calendar", true, ""));
    }

    #[test]
    fn calendar_passes_unconfigured_or_fetched_and_parsed() {
        assert!(calendar_check(no_calendar, no_fetch).ok);
        assert!(calendar_check(stored, feed).ok);
    }

    #[test]
    fn calendar_failures_name_the_cause_and_the_command() {
        let fix = |calendar, fetch| {
            let c = calendar_check(calendar, fetch);
            assert!(!c.ok);
            c.fix
        };
        assert_eq!(
            fix(stored, offline),
            "Calendar is offline; check the network or run `meeting config set-calendar`"
        );
        assert_eq!(
            fix(stored, rejected),
            "Calendar address was rejected (http 403); run `meeting config set-calendar`"
        );
        for fetch in [html, truncated] {
            assert_eq!(
                fix(stored, fetch),
                "Calendar address is not an iCalendar feed; run `meeting config set-calendar`"
            );
        }
        assert_eq!(
            fix(stored, tls),
            "Calendar check failed (tls); run `meeting config set-calendar`"
        );
        assert_eq!(
            fix(locked, no_fetch),
            "Could not read the Keychain; run `meeting config set-calendar`"
        );
    }

    #[test]
    fn offline_calendar_leaves_the_other_checks_unchanged() {
        let dir = temp("doctor-calendar-offline");
        let (config, manifest, exe) = setup(&dir);
        let off = run(&config, key_ok, &manifest, &exe);
        let on = checks(&Env {
            config: &config,
            key: key_ok,
            manifest,
            exe,
            context: true,
            desktop: false,
            calendar: stored,
            fetch: offline,
        });
        fs::remove_dir_all(&dir).unwrap();
        assert_eq!(on[..7], off[..]);
        assert!(!on[7].ok && on[7].fix.contains("offline"));
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
