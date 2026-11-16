use std::ffi::OsStr;
use std::fs;
use std::io::{BufRead, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use anyhow::{Context, bail};

use crate::config::Config;
use crate::{doctor, jobs};

/// The extension's fixed ID, set by the `key` in its manifest and pinned in
/// `extension/manifest.test.ts`. Keep the two in step.
pub const EXTENSION_ID: &str = "obbaikcmcdooakogedoiiflflbbhpgdh";

/// The first executable `name` on `path_var`, as found: symlinks are not resolved,
/// because `~/.local/bin/claude` points at a versioned binary the updater replaces.
fn find_on_path(name: &str, path_var: &OsStr) -> Option<PathBuf> {
    std::env::split_paths(path_var)
        .map(|dir| dir.join(name))
        .find(|p| {
            p.metadata()
                .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        })
}

fn require_tool(name: &str, path_var: &OsStr, how: &str) -> anyhow::Result<PathBuf> {
    find_on_path(name, path_var).with_context(|| format!("{name} not found on PATH; {how}"))
}

/// Asks `question`; an empty answer takes `default`, and a leading `~/` means `home`.
fn ask(
    input: &mut impl BufRead,
    out: &mut impl Write,
    question: &str,
    default: &str,
    home: &Path,
) -> anyhow::Result<String> {
    write!(out, "{question} [{default}]: ")?;
    out.flush()?;
    let mut line = String::new();
    input
        .read_line(&mut line)
        .context("cannot read the answer")?;
    let answer = line.trim();
    let answer = if answer.is_empty() { default } else { answer };
    Ok(match answer.strip_prefix("~/") {
        Some(rest) => home.join(rest).to_string_lossy().into_owned(),
        None => answer.to_string(),
    })
}

/// Registers the native host, stores absolute tool paths and writes the config.
/// Everything is injected so tests need no real `HOME`, `PATH` or terminal.
pub fn install(
    input: &mut impl BufRead,
    out: &mut impl Write,
    home: &Path,
    path_var: &OsStr,
    exe: &Path,
    desktop: bool,
) -> anyhow::Result<Config> {
    let config_path = home.join(".config/meeting-manager/config.toml");
    let existing = Config::load_from(&config_path).ok();

    let ffmpeg_path = require_tool("ffmpeg", path_var, "install it with `brew install ffmpeg`")?;
    let claude_path = require_tool("claude", path_var, "install Claude Code")?;
    // Only a desktop build writes the path; any other build carries an existing one over.
    let meeting_path = if desktop {
        Some(
            fs::canonicalize(exe)
                .with_context(|| format!("cannot resolve the path of {}", exe.display()))?,
        )
    } else {
        existing.as_ref().and_then(|c| c.meeting_path.clone())
    };

    let default_vault = existing
        .as_ref()
        .map(|c| c.vault_dir.to_string_lossy().into_owned())
        .unwrap_or_default();
    let vault = ask(input, out, "Vault path", &default_vault, home)?;
    if vault.is_empty() {
        bail!("a vault path is required");
    }
    let vault_dir = PathBuf::from(vault);
    if !vault_dir.is_dir() {
        bail!("vault directory {} does not exist", vault_dir.display());
    }
    let default_folder = existing.as_ref().map_or("Meetings", |c| &c.vault_folder);
    let vault_folder = ask(input, out, "Vault folder", default_folder, home)?;

    let manifest = doctor::manifest_path(home);
    let manifest_dir = manifest.parent().context("manifest path has no parent")?;
    fs::create_dir_all(manifest_dir)
        .with_context(|| format!("cannot create {}", manifest_dir.display()))?;
    jobs::write_atomic(
        &manifest,
        &serde_json::to_vec_pretty(&doctor::expected_manifest(exe, EXTENSION_ID))?,
    )?;

    let config = Config {
        recordings_dir: existing
            .map_or_else(|| home.join("Downloads/Meetings"), |c| c.recordings_dir),
        vault_dir,
        vault_folder,
        ffmpeg_path,
        claude_path,
        extension_id: EXTENSION_ID.into(),
        meeting_path,
    };
    config.save_to(&config_path)?;

    if !doctor::claude_logged_in(&config.claude_path) {
        writeln!(out, "claude is not logged in: run `claude auth login`")?;
    }
    writeln!(
        out,
        "Recordings are read from {}; edit `recordings_dir` in {} if Chrome saves downloads elsewhere.",
        config.recordings_dir.display(),
        config_path.display()
    )?;
    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Env {
        root: PathBuf,
        home: PathBuf,
        bin: PathBuf,
        vault: PathBuf,
    }

    fn env(name: &str, claude_out: &str) -> Env {
        let root =
            std::env::temp_dir().join(format!("meeting-install-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let (home, bin, vault) = (root.join("home"), root.join("bin"), root.join("vault"));
        for d in [&home, &bin, &vault] {
            fs::create_dir_all(d).unwrap();
        }
        stub(&bin, "ffmpeg", "");
        stub(&bin, "claude", &format!("echo '{claude_out}'"));
        Env {
            root,
            home,
            bin,
            vault,
        }
    }

    fn stub(dir: &Path, name: &str, body: &str) {
        crate::test_support::write_script(&dir.join(name), &format!("#!/bin/sh\n{body}\n"));
    }

    fn run(e: &Env, answers: &str) -> (anyhow::Result<Config>, String) {
        run_as(e, answers, Path::new("/opt/meeting"), false)
    }

    fn run_as(
        e: &Env,
        answers: &str,
        exe: &Path,
        desktop: bool,
    ) -> (anyhow::Result<Config>, String) {
        let mut out = Vec::new();
        let r = install(
            &mut answers.as_bytes(),
            &mut out,
            &e.home,
            e.bin.as_os_str(),
            exe,
            desktop,
        );
        (r, String::from_utf8(out).unwrap())
    }

    /// An executable `meeting` reached through a symlinked directory: (as given, canonical).
    fn linked_exe(e: &Env) -> (PathBuf, PathBuf) {
        let real = e.root.join("real");
        fs::create_dir_all(&real).unwrap();
        stub(&real, "meeting", "");
        std::os::unix::fs::symlink(&real, e.root.join("link")).unwrap();
        (
            e.root.join("link/meeting"),
            fs::canonicalize(real.join("meeting")).unwrap(),
        )
    }

    fn files(e: &Env) -> (PathBuf, PathBuf) {
        (
            e.home.join(".config/meeting-manager/config.toml"),
            doctor::manifest_path(&e.home),
        )
    }

    #[test]
    fn fresh_install_writes_config_and_manifest() {
        let e = env("fresh", r#"{"loggedIn": true}"#);
        let (r, out) = run(&e, &format!("{}\n\n", e.vault.display()));
        let config = r.unwrap();
        let (config_path, manifest) = files(&e);
        assert_eq!(
            config,
            Config {
                recordings_dir: e.home.join("Downloads/Meetings"),
                vault_dir: e.vault.clone(),
                vault_folder: "Meetings".into(),
                ffmpeg_path: e.bin.join("ffmpeg"),
                claude_path: e.bin.join("claude"),
                extension_id: EXTENSION_ID.into(),
                meeting_path: None,
            }
        );
        assert_eq!(Config::load_from(&config_path).unwrap(), config);
        let written: serde_json::Value =
            serde_json::from_slice(&fs::read(manifest).unwrap()).unwrap();
        assert_eq!(
            written,
            doctor::expected_manifest(Path::new("/opt/meeting"), EXTENSION_ID)
        );
        assert!(!out.contains("not logged in"));
        assert!(out.contains("Downloads/Meetings"));
        fs::remove_dir_all(&e.root).unwrap();
    }

    #[test]
    fn desktop_install_writes_the_canonical_binary_path() {
        let e = env("desktop", r#"{"loggedIn": true}"#);
        let (exe, canonical) = linked_exe(&e);
        let (r, _) = run_as(&e, &format!("{}\n\n", e.vault.display()), &exe, true);
        let config = r.unwrap();
        let (config_path, manifest) = files(&e);
        let text = fs::read_to_string(&config_path).unwrap();
        let written: serde_json::Value =
            serde_json::from_slice(&fs::read(manifest).unwrap()).unwrap();
        fs::remove_dir_all(&e.root).unwrap();
        assert_ne!(exe, canonical);
        assert_eq!(config.meeting_path, Some(canonical.clone()));
        assert!(text.ends_with(&format!(
            "meeting_path = {:?}\n",
            canonical.to_string_lossy()
        )));
        // The manifest keeps the path as given, as it does without the flag.
        assert_eq!(written, doctor::expected_manifest(&exe, EXTENSION_ID));
    }

    #[test]
    fn desktop_install_twice_is_byte_identical() {
        let e = env("desktop-twice", r#"{"loggedIn": true}"#);
        let (exe, _) = linked_exe(&e);
        let (first, _) = run_as(&e, &format!("{}\nCalls\n", e.vault.display()), &exe, true);
        let (config_path, manifest) = files(&e);
        let snapshot = (
            fs::read(&config_path).unwrap(),
            fs::read(&manifest).unwrap(),
        );
        let (second, _) = run_as(&e, "\n\n", &exe, true);
        let after = (
            fs::read(&config_path).unwrap(),
            fs::read(&manifest).unwrap(),
        );
        fs::remove_dir_all(&e.root).unwrap();
        assert_eq!(first.unwrap(), second.unwrap());
        assert_eq!(snapshot, after);
    }

    #[test]
    fn desktop_install_with_an_unresolvable_exe_writes_nothing() {
        let e = env("desktop-no-exe", "{}");
        let (r, _) = run_as(
            &e,
            &format!("{}\n\n", e.vault.display()),
            &e.root.join("gone/meeting"),
            true,
        );
        let (config_path, manifest) = files(&e);
        let wrote = config_path.exists() || manifest.exists();
        fs::remove_dir_all(&e.root).unwrap();
        assert!(
            r.unwrap_err()
                .to_string()
                .contains("cannot resolve the path of")
        );
        assert!(!wrote);
    }

    #[test]
    fn install_without_the_flag_writes_no_meeting_path() {
        let e = env("no-flag", "{}");
        let (exe, _) = linked_exe(&e);
        let (r, _) = run_as(&e, &format!("{}\n\n", e.vault.display()), &exe, false);
        let text = fs::read_to_string(files(&e).0).unwrap();
        fs::remove_dir_all(&e.root).unwrap();
        assert_eq!(r.unwrap().meeting_path, None);
        assert!(!text.contains("meeting_path"));
    }

    #[test]
    fn install_without_the_flag_keeps_an_existing_meeting_path() {
        let e = env("no-flag-keep", "{}");
        let (exe, _) = linked_exe(&e);
        let answers = format!("{}\n\n", e.vault.display());
        run_as(&e, &answers, &exe, false).0.unwrap();
        let config_path = files(&e).0;
        let mut seeded = fs::read_to_string(&config_path).unwrap();
        seeded.push_str("meeting_path = \"/x/meeting\"\n");
        fs::write(&config_path, &seeded).unwrap();
        let (r, _) = run_as(&e, "\n\n", &exe, false);
        let text = fs::read_to_string(&config_path).unwrap();
        fs::remove_dir_all(&e.root).unwrap();
        assert_eq!(r.unwrap().meeting_path, Some("/x/meeting".into()));
        assert_eq!(text, seeded);
    }

    #[test]
    fn missing_ffmpeg_bails_and_writes_nothing() {
        let e = env("no-ffmpeg", "{}");
        fs::remove_file(e.bin.join("ffmpeg")).unwrap();
        let (r, _) = run(&e, &format!("{}\n\n", e.vault.display()));
        assert!(r.unwrap_err().to_string().contains("brew install ffmpeg"));
        let (config_path, manifest) = files(&e);
        assert!(!config_path.exists() && !manifest.exists());
        fs::remove_dir_all(&e.root).unwrap();
    }

    #[test]
    fn nonexistent_vault_bails() {
        let e = env("bad-vault", "{}");
        let (r, _) = run(&e, "/does/not/exist\n\n");
        assert!(r.unwrap_err().to_string().contains("does not exist"));
        assert!(!files(&e).1.exists());
        fs::remove_dir_all(&e.root).unwrap();
    }

    #[test]
    fn empty_vault_answer_is_rejected() {
        let e = env("empty-vault", "{}");
        let (r, _) = run(&e, "\n\n");
        assert_eq!(r.unwrap_err().to_string(), "a vault path is required");
        fs::remove_dir_all(&e.root).unwrap();
    }

    #[test]
    fn tilde_expands_under_home() {
        let e = env("tilde", "{}");
        fs::create_dir(e.home.join("Vault")).unwrap();
        let (r, _) = run(&e, "~/Vault\nCalls\n");
        let config = r.unwrap();
        assert_eq!(config.vault_dir, e.home.join("Vault"));
        assert_eq!(config.vault_folder, "Calls");
        fs::remove_dir_all(&e.root).unwrap();
    }

    #[test]
    fn second_run_keeps_the_first_runs_values() {
        let e = env("twice", r#"{"loggedIn": true}"#);
        let (first, _) = run(&e, &format!("{}\nCalls\n", e.vault.display()));
        let (config_path, manifest) = files(&e);
        let snapshot = (
            fs::read(&config_path).unwrap(),
            fs::read(&manifest).unwrap(),
        );
        let (second, _) = run(&e, "\n\n");
        assert_eq!(first.unwrap(), second.unwrap());
        assert_eq!(
            snapshot,
            (
                fs::read(&config_path).unwrap(),
                fs::read(&manifest).unwrap()
            )
        );
        fs::remove_dir_all(&e.root).unwrap();
    }

    #[test]
    fn logged_out_claude_warns_and_still_writes() {
        let e = env("logged-out", r#"{"loggedIn": false}"#);
        let (r, out) = run(&e, &format!("{}\n\n", e.vault.display()));
        r.unwrap();
        assert!(out.contains("claude is not logged in: run `claude auth login`"));
        assert!(files(&e).0.exists());
        fs::remove_dir_all(&e.root).unwrap();
    }

    #[test]
    fn symlinked_tool_is_not_resolved() {
        let e = env("symlink", "{}");
        let real = e.root.join("real-claude");
        fs::rename(e.bin.join("claude"), &real).unwrap();
        std::os::unix::fs::symlink(&real, e.bin.join("claude")).unwrap();
        assert_eq!(
            find_on_path("claude", e.bin.as_os_str()),
            Some(e.bin.join("claude"))
        );
        fs::remove_dir_all(&e.root).unwrap();
    }
}
