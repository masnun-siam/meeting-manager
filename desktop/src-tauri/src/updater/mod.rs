//! App updates (FR-25, FR-26): check, download and install a newer release through
//! `tauri-plugin-updater`, Rust side only (D47). An install waits until the recorder is idle and
//! then holds it in `saving` until the relaunch, so a relaunch never interrupts a recording or
//! its conversion, and no recording can start between the install and the relaunch.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use meeting::store::{self, Store};
use serde_json::{Value, json};
use tauri::{AppHandle, Manager, Runtime, State};
use tauri_plugin_updater::UpdaterExt;

use crate::App;
use crate::admin::blocking;
use crate::recorder::Recorder;
use crate::reply;

/// The store setting behind "Install updates automatically": `"true"` or `"false"`, absent =
/// off (FR-25).
pub const AUTO_UPDATE_KEY: &str = "auto_update";

/// FR-26: with automatic install on, how often the app checks while open.
const EVERY: Duration = Duration::from_secs(6 * 3600);

/// How often a downloaded update looks again for an idle recorder.
const POLL: Duration = Duration::from_secs(1);

/// Asks the update server for a newer release; `Err` is the reason shown after "Couldn't check
/// for updates: ".
pub type CheckFn = Arc<dyn Fn() -> Result<Option<Found>, String> + Send + Sync>;
/// Downloads and verifies the package, reporting progress 0–100.
pub type DownloadFn = Arc<dyn Fn(&mut dyn FnMut(u8)) -> Result<Vec<u8>, String> + Send + Sync>;
/// Replaces the app bundle with the downloaded package.
pub type InstallFn = Arc<dyn Fn(&[u8]) -> Result<(), String> + Send + Sync>;
/// Relaunches the app; the real one never returns.
pub type RestartFn = Arc<dyn Fn() + Send + Sync>;

/// A newer release a check found, with what downloads and installs it.
#[derive(Clone)]
pub struct Found {
    pub version: String,
    pub notes: Option<String>,
    pub download: DownloadFn,
    pub install: InstallFn,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Phase {
    Idle,
    Checking,
    Downloading,
    /// Downloaded; waiting for the recorder to be idle.
    Ready,
    Installing,
}

impl Phase {
    fn as_str(self) -> &'static str {
        match self {
            Phase::Idle => "idle",
            Phase::Checking => "checking",
            Phase::Downloading => "downloading",
            Phase::Ready => "ready",
            Phase::Installing => "installing",
        }
    }
}

struct Shared {
    phase: Phase,
    progress: Option<u8>,
    /// The result of the last check that answered.
    available: Option<Found>,
    error: Option<String>,
    tool_error: Option<String>,
}

/// The update state and its outside effects, swapped for fakes in tests.
#[derive(Clone)]
pub struct Updater {
    shared: Arc<Mutex<Shared>>,
    /// The running app's version.
    pub version: String,
    pub check: CheckFn,
    pub restart: RestartFn,
    pub every: Duration,
    pub poll: Duration,
}

impl Updater {
    /// The real updater: checks through the plugin, relaunches with Tauri's own restart.
    pub fn new<R: Runtime>(app: &AppHandle<R>) -> Updater {
        let handle = app.clone();
        Updater::with(
            app.package_info().version.to_string(),
            plugin_check(app.clone()),
            Arc::new(move || handle.restart()),
        )
    }

    pub fn with(version: String, check: CheckFn, restart: RestartFn) -> Updater {
        Updater {
            shared: Arc::new(Mutex::new(Shared {
                phase: Phase::Idle,
                progress: None,
                available: None,
                error: None,
                tool_error: None,
            })),
            version,
            check,
            restart,
            every: EVERY,
            poll: POLL,
        }
    }

    fn shared(&self) -> MutexGuard<'_, Shared> {
        self.shared.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The tool swap's failure (#173), shown beside the update state; `None` clears it.
    pub fn set_tool_error(&self, error: Option<String>) {
        self.shared().tool_error = error;
    }

    /// `update_status`, with the `auto_update` setting read from `store_path`.
    pub fn status(&self, store_path: &Path) -> Value {
        let auto_update = auto_update(store_path);
        let shared = self.shared();
        let available = shared
            .available
            .as_ref()
            .map(|found| json!({"version": found.version, "notes": found.notes}));
        reply::ok(json!({
            "version": self.version,
            "auto_update": auto_update,
            "state": shared.phase.as_str(),
            "progress": shared.progress,
            "available": available,
            "error": shared.error,
            "tool_error": shared.tool_error,
        }))
    }

    /// Moves `Idle` → `to`; `false` when something already runs, so it never runs twice.
    fn begin(&self, to: Phase) -> bool {
        let mut shared = self.shared();
        if shared.phase != Phase::Idle {
            return false;
        }
        shared.phase = to;
        shared.progress = (to == Phase::Downloading).then_some(0);
        shared.error = None;
        true
    }

    fn set_phase(&self, phase: Phase) {
        let mut shared = self.shared();
        shared.phase = phase;
        shared.progress = (phase == Phase::Downloading).then_some(0);
    }

    fn fail(&self, stage: &'static str, error: String) {
        tracing::warn!(stage, "update failed");
        let mut shared = self.shared();
        shared.phase = Phase::Idle;
        shared.progress = None;
        shared.error = Some(error);
    }

    /// Starts one check on its own thread; with `install`, a newer release found is downloaded
    /// and installed as soon as the recorder is idle. `None` when a check, download or install
    /// is already running.
    pub fn start_check(&self, recorder: &Recorder, install: bool) -> Option<JoinHandle<()>> {
        if !self.begin(Phase::Checking) {
            return None;
        }
        let (updater, recorder) = (self.clone(), recorder.clone());
        self.spawn("update-check", move || {
            if let Some(found) = updater.run_check() {
                if install {
                    updater.set_phase(Phase::Downloading);
                    updater.download_and_install(&found, &recorder);
                }
            }
        })
    }

    /// The check itself; leaves the phase `Idle` unless it returns a release to install.
    fn run_check(&self) -> Option<Found> {
        match (self.check)() {
            Err(reason) => {
                self.fail("check", format!("Couldn't check for updates: {reason}"));
                None
            }
            Ok(found) => {
                let mut shared = self.shared();
                shared.phase = Phase::Idle;
                shared.available = found.clone();
                found
            }
        }
    }

    /// `update_install`: downloads on its own thread, then installs once the recorder is idle.
    /// `Ok` also when an update is already being checked, downloaded or installed.
    pub fn start_install(&self, recorder: &Recorder) -> Result<(), Value> {
        let found = {
            let shared = self.shared();
            if shared.phase != Phase::Idle {
                return Ok(());
            }
            shared.available.clone().ok_or_else(|| {
                reply::err(
                    "no_update",
                    "No update is available; check for updates first",
                )
            })?
        };
        if recorder.status().state != "idle" {
            return Err(reply::err(
                "recording_active",
                "Finish the recording to install",
            ));
        }
        if !self.begin(Phase::Downloading) {
            return Ok(());
        }
        let (updater, recorder) = (self.clone(), recorder.clone());
        self.spawn("update-install", move || {
            updater.download_and_install(&found, &recorder)
        });
        Ok(())
    }

    /// From `Downloading`: downloads, waits for the idle gate, installs and relaunches. Any
    /// failure leaves the running app as it was, `Idle` with the reason in `error`.
    fn download_and_install(&self, found: &Found, recorder: &Recorder) {
        let bytes = match (found.download)(&mut |percent| self.shared().progress = Some(percent)) {
            Ok(bytes) => bytes,
            Err(reason) => return self.fail("download", install_error(&reason)),
        };
        self.set_phase(Phase::Ready);
        // Shared by manual and automatic install: the guard keeps the recorder in `saving` from
        // here to the relaunch, so Start is refused in between.
        let saving = loop {
            if let Some(saving) = recorder.begin_saving() {
                break saving;
            }
            thread::sleep(self.poll);
        };
        self.set_phase(Phase::Installing);
        if let Err(reason) = (found.install)(&bytes) {
            drop(saving);
            return self.fail("install", install_error(&reason));
        }
        tracing::info!("update installed, relaunching");
        (self.restart)();
        // Reached only when `restart` returns, which the real one never does.
        drop(saving);
    }

    /// FR-26's timer: one named thread that checks every `every` while `auto_update` is on.
    pub fn start_timer(&self, recorder: &Recorder, store_path: PathBuf) -> Option<JoinHandle<()>> {
        let (updater, recorder) = (self.clone(), recorder.clone());
        self.spawn("update-timer", move || {
            loop {
                thread::sleep(updater.every);
                updater.tick(&recorder, &store_path);
            }
        })
    }

    /// A launch or timer tick: an automatic check and install, only while `auto_update` is on.
    pub fn tick(&self, recorder: &Recorder, store_path: &Path) -> Option<JoinHandle<()>> {
        if !auto_update(store_path) {
            return None;
        }
        self.start_check(recorder, true)
    }

    fn spawn(&self, name: &str, work: impl FnOnce() + Send + 'static) -> Option<JoinHandle<()>> {
        let spawned = thread::Builder::new().name(name.into()).spawn(work);
        if let Err(e) = &spawned {
            tracing::warn!(kind = ?e.kind(), "cannot start the update thread");
            self.fail("thread", "Couldn't start the update".into());
        }
        spawned.ok()
    }
}

fn install_error(reason: &str) -> String {
    format!("Couldn't install the update: {reason}")
}

/// Whether "Install updates automatically" is on; a setting that can't be read counts as off.
pub fn auto_update(store_path: &Path) -> bool {
    match Store::open(store_path).and_then(|s| s.setting(AUTO_UPDATE_KEY)) {
        Ok(value) => value.as_deref() == Some("true"),
        Err(e) => {
            tracing::warn!(kind = store::error_kind(&e), "auto update setting not read");
            false
        }
    }
}

/// `set_auto_update`. Turning it on checks nothing at once: the next launch or tick does.
pub fn save_auto_update(store_path: &Path, enabled: bool) -> Value {
    let value = if enabled { "true" } else { "false" };
    let saved = Store::open(store_path).and_then(|s| s.set_setting(AUTO_UPDATE_KEY, value));
    if let Err(e) = saved {
        tracing::error!(
            kind = store::error_kind(&e),
            "auto update setting not saved"
        );
        return reply::err("store_unavailable", format!("{e:#}"));
    }
    reply::ok(json!({ "auto_update": enabled }))
}

/// Whether `remote` is a newer release than `current` (`major.minor.patch`). A pre-release, an
/// equal or older version, or one that does not parse is never newer (D48: no downgrade).
pub fn is_newer(current: &str, remote: &str) -> bool {
    let (Some((remote, false)), Some((current, current_pre))) = (parse(remote), parse(current))
    else {
        return false;
    };
    remote > current || (remote == current && current_pre)
}

/// `major.minor.patch` and whether it is a pre-release; build metadata is ignored.
fn parse(version: &str) -> Option<((u64, u64, u64), bool)> {
    let version = version.split('+').next()?;
    let (core, pre) = match version.split_once('-') {
        Some((core, _)) => (core, true),
        None => (version, false),
    };
    let mut parts = core.split('.').map(|part| part.parse::<u64>().ok());
    let triple = (parts.next()??, parts.next()??, parts.next()??);
    parts.next().is_none().then_some((triple, pre))
}

/// The plugin's error as the reason shown to the user: `offline` when the server can't be
/// reached in time.
fn reason(error: &tauri_plugin_updater::Error) -> String {
    match error {
        tauri_plugin_updater::Error::Reqwest(e) if e.is_connect() || e.is_timeout() => {
            "offline".into()
        }
        other => other.to_string(),
    }
}

fn percent(done: u64, total: u64) -> u8 {
    u8::try_from(done.saturating_mul(100) / total.max(1))
        .unwrap_or(100)
        .min(100)
}

/// The check through `tauri-plugin-updater` (registered in `run`).
fn plugin_check<R: Runtime>(app: AppHandle<R>) -> CheckFn {
    Arc::new(move || {
        let updater = app
            .updater_builder()
            .version_comparator(|current, release| {
                is_newer(&current.to_string(), &release.version.to_string())
            })
            .build()
            .map_err(|e| reason(&e))?;
        let update = tauri::async_runtime::block_on(updater.check()).map_err(|e| reason(&e))?;
        Ok(update.map(found))
    })
}

fn found(update: tauri_plugin_updater::Update) -> Found {
    let update = Arc::new(update);
    let downloading = update.clone();
    Found {
        version: update.version.clone(),
        notes: update.body.clone(),
        download: Arc::new(move |progress| {
            let mut done = 0u64;
            let on_chunk = |chunk: usize, total: Option<u64>| {
                done += chunk as u64;
                if let Some(total) = total {
                    progress(percent(done, total));
                }
            };
            tauri::async_runtime::block_on(downloading.download(on_chunk, || {}))
                .map_err(|e| reason(&e))
        }),
        install: Arc::new(move |bytes| update.install(bytes).map_err(|e| reason(&e))),
    }
}

/// Called once at startup, last: with automatic install on, a check now and the 6-hourly timer.
pub fn on_launch<R: Runtime>(app: &AppHandle<R>) {
    if let Some(state) = app.try_state::<App>() {
        launch(&state.updater, &state.recorder, &state.env.store_path);
    }
}

fn launch(updater: &Updater, recorder: &Recorder, store_path: &Path) {
    updater.tick(recorder, store_path);
    updater.start_timer(recorder, store_path.to_path_buf());
}

#[tauri::command(rename_all = "snake_case")]
pub async fn update_status<R: Runtime>(app: AppHandle<R>) -> Value {
    blocking(app, |state| state.updater.status(&state.env.store_path)).await
}

#[tauri::command(rename_all = "snake_case")]
pub fn update_check(app: State<'_, App>) -> Value {
    app.updater.start_check(&app.recorder, false);
    reply::ok(json!({ "started": true }))
}

/// Off the main thread: `Recorder::status` reads the store.
#[tauri::command(rename_all = "snake_case")]
pub async fn update_install<R: Runtime>(app: AppHandle<R>) -> Value {
    blocking(app, |state| {
        match state.updater.start_install(&state.recorder) {
            Ok(()) => reply::ok(json!({ "started": true })),
            Err(error) => error,
        }
    })
    .await
}

#[tauri::command(rename_all = "snake_case")]
pub async fn set_auto_update<R: Runtime>(app: AppHandle<R>, enabled: bool) -> Value {
    blocking(app, move |state| {
        save_auto_update(&state.env.store_path, enabled)
    })
    .await
}

#[cfg(test)]
pub(crate) mod tests {
    use std::fs;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Instant;

    use super::*;
    use crate::recorder::tests::{Rig, ok_capture, push, rig};

    /// An updater whose checks fail, for apps built without the plugin (the mock runtime).
    pub(crate) fn unplugged<R: Runtime>(_: &AppHandle<R>) -> Updater {
        Updater::with(
            env!("CARGO_PKG_VERSION").into(),
            Arc::new(|| Err("no updater in tests".into())),
            Arc::new(|| {}),
        )
    }

    /// What the fakes saw, shared between the test and the update threads.
    #[derive(Default)]
    struct Calls {
        checks: AtomicUsize,
        downloads: AtomicUsize,
        installs: AtomicUsize,
        restarts: AtomicUsize,
        /// The recorder state seen at restart, and whether a second `begin_saving` was refused.
        at_restart: Mutex<Option<(&'static str, bool)>>,
    }

    type Answer = Result<(), String>;

    struct Fakes {
        calls: Arc<Calls>,
        check: Result<bool, String>,
        download: Answer,
        install: Answer,
        /// Held by the fake download until the test drops its sender.
        hold_download: Option<std::sync::mpsc::Receiver<()>>,
    }

    impl Fakes {
        fn new() -> Fakes {
            Fakes {
                calls: Arc::default(),
                check: Ok(true),
                download: Ok(()),
                install: Ok(()),
                hold_download: None,
            }
        }

        fn updater(self, recorder: &Recorder) -> Updater {
            let Fakes {
                calls,
                check,
                download,
                install,
                hold_download,
            } = self;
            let hold = Arc::new(Mutex::new(hold_download));
            let (c1, c2, c3, c4) = (calls.clone(), calls.clone(), calls.clone(), calls.clone());
            let found = Found {
                version: "0.2.0".into(),
                notes: Some("Faster saving.".into()),
                download: Arc::new(move |progress| {
                    c2.downloads.fetch_add(1, Ordering::SeqCst);
                    progress(40);
                    if let Some(hold) = hold.lock().unwrap().take() {
                        let _ = hold.recv();
                    }
                    progress(100);
                    download.clone().map(|()| b"bundle".to_vec())
                }),
                install: Arc::new(move |bytes| {
                    assert_eq!(bytes, b"bundle");
                    c3.installs.fetch_add(1, Ordering::SeqCst);
                    install.clone()
                }),
            };
            let watched = recorder.clone();
            let mut updater = Updater::with(
                "0.1.0".into(),
                Arc::new(move || {
                    c1.checks.fetch_add(1, Ordering::SeqCst);
                    check.clone().map(|newer| newer.then(|| found.clone()))
                }),
                Arc::new(move || {
                    let refused = watched.begin_saving().is_none();
                    *c4.at_restart.lock().unwrap() = Some((watched.status().state, refused));
                    // Last, so a test that saw the count also sees what was recorded.
                    c4.restarts.fetch_add(1, Ordering::SeqCst);
                }),
            );
            updater.poll = Duration::from_millis(10);
            updater
        }
    }

    fn store_path(rig: &Rig) -> PathBuf {
        rig.env().store_path
    }

    fn status(updater: &Updater, rig: &Rig) -> Value {
        updater.status(&store_path(rig))
    }

    fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !done() {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn checked(updater: &Updater, recorder: &Recorder) {
        updater
            .start_check(recorder, false)
            .expect("a check started")
            .join()
            .unwrap();
    }

    #[test]
    fn is_newer_compares_major_minor_patch() {
        assert!(!is_newer("0.1.0", "0.1.0"));
        assert!(!is_newer("0.2.0", "0.1.9"));
        assert!(!is_newer("1.0.0", "0.9.9"));
        assert!(is_newer("0.1.0", "0.1.1"));
        assert!(is_newer("0.1.9", "0.2.0"));
        assert!(is_newer("0.9.9", "1.0.0"));
        assert!(is_newer("0.1.0", "0.1.10"));
        assert!(is_newer("0.1.0", "0.1.1+build.7"));
    }

    #[test]
    fn a_pre_release_or_unparsable_version_is_never_newer() {
        assert!(!is_newer("0.1.0", "0.2.0-beta.1"));
        assert!(!is_newer("0.1.0", "0.2"));
        assert!(!is_newer("0.1.0", "0.2.0.1"));
        assert!(!is_newer("0.1.0", "v0.2.0"));
        assert!(!is_newer("nonsense", "0.2.0"));
        // The release a running pre-release leads to.
        assert!(is_newer("0.2.0-beta.1", "0.2.0"));
    }

    #[test]
    fn percent_is_clamped() {
        assert_eq!(percent(0, 200), 0);
        assert_eq!(percent(50, 200), 25);
        assert_eq!(percent(300, 200), 100);
        assert_eq!(percent(5, 0), 100);
    }

    #[test]
    fn status_has_the_prd_shape_before_any_check() {
        let rig = rig("update-status", ok_capture());
        let updater = Fakes::new().updater(&rig.recorder);
        assert_eq!(
            status(&updater, &rig),
            json!({"ok": true, "version": "0.1.0", "auto_update": false, "state": "idle",
                "progress": null, "available": null, "error": null, "tool_error": null})
        );
        updater.set_tool_error(Some("Couldn't update the meeting tool: disk full".into()));
        assert_eq!(
            status(&updater, &rig)["tool_error"],
            "Couldn't update the meeting tool: disk full"
        );
        updater.set_tool_error(None);
        assert_eq!(status(&updater, &rig)["tool_error"], Value::Null);
    }

    #[test]
    fn a_check_offers_a_newer_release_with_its_notes() {
        let rig = rig("update-found", ok_capture());
        let updater = Fakes::new().updater(&rig.recorder);
        checked(&updater, &rig.recorder);
        let status = status(&updater, &rig);
        assert_eq!(status["state"], "idle");
        assert_eq!(
            status["available"],
            json!({"version": "0.2.0", "notes": "Faster saving."})
        );
        assert_eq!(status["error"], Value::Null);
    }

    #[test]
    fn a_check_with_nothing_newer_offers_nothing() {
        let rig = rig("update-none", ok_capture());
        let fakes = Fakes {
            check: Ok(false),
            ..Fakes::new()
        };
        let updater = fakes.updater(&rig.recorder);
        checked(&updater, &rig.recorder);
        assert_eq!(status(&updater, &rig)["available"], Value::Null);
        assert_eq!(
            updater.start_install(&rig.recorder).unwrap_err()["code"],
            "no_update"
        );
    }

    #[test]
    fn a_failed_check_shows_the_reason_and_the_next_check_tries_again() {
        let rig = rig("update-offline", ok_capture());
        let fakes = Fakes {
            check: Err("offline".into()),
            ..Fakes::new()
        };
        let calls = fakes.calls.clone();
        let updater = fakes.updater(&rig.recorder);
        checked(&updater, &rig.recorder);
        let status = status(&updater, &rig);
        assert_eq!(status["state"], "idle");
        assert_eq!(status["error"], "Couldn't check for updates: offline");
        checked(&updater, &rig.recorder);
        assert_eq!(calls.checks.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn only_one_check_or_download_runs_at_a_time() {
        let rig = rig("update-single", ok_capture());
        let (release, hold) = std::sync::mpsc::channel();
        let fakes = Fakes {
            hold_download: Some(hold),
            install: Err("stop here".into()),
            ..Fakes::new()
        };
        let calls = fakes.calls.clone();
        let updater = fakes.updater(&rig.recorder);
        checked(&updater, &rig.recorder);
        updater.start_install(&rig.recorder).unwrap();
        wait_for("the download", || status(&updater, &rig)["progress"] == 40);
        assert_eq!(status(&updater, &rig)["state"], "downloading");
        assert!(updater.start_check(&rig.recorder, false).is_none());
        updater.start_install(&rig.recorder).unwrap();
        assert_eq!(status(&updater, &rig)["state"], "downloading");
        drop(release);
        wait_for("the failed install", || {
            status(&updater, &rig)["error"] != Value::Null
        });
        assert_eq!(calls.checks.load(Ordering::SeqCst), 1);
        assert_eq!(calls.downloads.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn install_is_refused_while_recording_or_saving_and_downloads_nothing() {
        let rig = rig("update-busy", ok_capture());
        let fakes = Fakes::new();
        let calls = fakes.calls.clone();
        let updater = fakes.updater(&rig.recorder);
        checked(&updater, &rig.recorder);
        rig.start().unwrap();
        let refused = updater.start_install(&rig.recorder).unwrap_err();
        assert_eq!(refused["code"], "recording_active");
        assert_eq!(refused["message"], "Finish the recording to install");
        rig.recorder.pause().unwrap();
        assert_eq!(
            updater.start_install(&rig.recorder).unwrap_err()["code"],
            "recording_active"
        );
        rig.stop(crate::recorder::Reason::User);
        let saving = rig.recorder.begin_saving().unwrap();
        assert_eq!(
            updater.start_install(&rig.recorder).unwrap_err()["code"],
            "recording_active"
        );
        drop(saving);
        assert_eq!(calls.downloads.load(Ordering::SeqCst), 0);
        assert_eq!(status(&updater, &rig)["state"], "idle");
    }

    #[test]
    fn install_downloads_installs_and_relaunches_holding_the_recorder() {
        let rig = rig("update-install", ok_capture());
        let fakes = Fakes::new();
        let calls = fakes.calls.clone();
        let updater = fakes.updater(&rig.recorder);
        checked(&updater, &rig.recorder);
        updater.start_install(&rig.recorder).unwrap();
        wait_for("the restart", || calls.restarts.load(Ordering::SeqCst) == 1);
        assert_eq!(calls.installs.load(Ordering::SeqCst), 1);
        // Between install and restart the recorder was held: Start is refused.
        assert_eq!(*calls.at_restart.lock().unwrap(), Some(("saving", true)));
    }

    #[test]
    fn the_gate_waits_for_the_recording_to_be_saved() {
        let rig = rig("update-gate", ok_capture());
        let (release, hold) = std::sync::mpsc::channel();
        let fakes = Fakes {
            hold_download: Some(hold),
            ..Fakes::new()
        };
        let calls = fakes.calls.clone();
        let updater = fakes.updater(&rig.recorder);
        checked(&updater, &rig.recorder);
        updater.start_install(&rig.recorder).unwrap();
        // A recording starts before the download finishes.
        wait_for("the download", || status(&updater, &rig)["progress"] == 40);
        rig.start().unwrap();
        push(48_000);
        drop(release);
        wait_for("ready", || status(&updater, &rig)["state"] == "ready");
        thread::sleep(Duration::from_millis(100));
        assert_eq!(calls.installs.load(Ordering::SeqCst), 0);
        assert_eq!(rig.recorder.status().state, "recording");
        rig.stop(crate::recorder::Reason::User);
        wait_for("the restart", || calls.restarts.load(Ordering::SeqCst) == 1);
        assert_eq!(*calls.at_restart.lock().unwrap(), Some(("saving", true)));
    }

    #[test]
    fn a_failed_download_or_install_keeps_the_app_and_frees_the_recorder() {
        for (name, download, install, error) in [
            (
                "download",
                Err("signature mismatch".to_string()),
                Ok(()),
                "Couldn't install the update: signature mismatch",
            ),
            (
                "install",
                Ok(()),
                Err("disk full".to_string()),
                "Couldn't install the update: disk full",
            ),
        ] {
            let rig = rig(&format!("update-fail-{name}"), ok_capture());
            let fakes = Fakes {
                download,
                install,
                ..Fakes::new()
            };
            let calls = fakes.calls.clone();
            let updater = fakes.updater(&rig.recorder);
            checked(&updater, &rig.recorder);
            updater.start_install(&rig.recorder).unwrap();
            wait_for(name, || status(&updater, &rig)["error"] != Value::Null);
            let status = status(&updater, &rig);
            assert_eq!(status["state"], "idle", "{name}");
            assert_eq!(status["error"], error, "{name}");
            assert_eq!(status["progress"], Value::Null, "{name}");
            assert_eq!(calls.restarts.load(Ordering::SeqCst), 0, "{name}");
            assert_eq!(rig.recorder.status().state, "idle", "{name}");
            // The next try starts again.
            assert!(
                updater.start_check(&rig.recorder, false).is_some(),
                "{name}"
            );
        }
    }

    #[test]
    fn with_auto_update_off_nothing_checks_on_its_own() {
        let rig = rig("update-auto-off", ok_capture());
        let fakes = Fakes::new();
        let calls = fakes.calls.clone();
        let mut updater = fakes.updater(&rig.recorder);
        updater.every = Duration::from_secs(3600);
        launch(&updater, &rig.recorder, &store_path(&rig));
        assert!(updater.tick(&rig.recorder, &store_path(&rig)).is_none());
        save_auto_update(&store_path(&rig), false);
        assert!(updater.tick(&rig.recorder, &store_path(&rig)).is_none());
        assert_eq!(calls.checks.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn with_auto_update_on_launch_checks_and_installs_without_a_click() {
        let rig = rig("update-auto-on", ok_capture());
        let fakes = Fakes::new();
        let calls = fakes.calls.clone();
        let mut updater = fakes.updater(&rig.recorder);
        updater.every = Duration::from_secs(3600);
        assert_eq!(
            save_auto_update(&store_path(&rig), true)["auto_update"],
            true
        );
        launch(&updater, &rig.recorder, &store_path(&rig));
        wait_for("the restart", || calls.restarts.load(Ordering::SeqCst) == 1);
        assert_eq!(calls.checks.load(Ordering::SeqCst), 1);
        assert_eq!(*calls.at_restart.lock().unwrap(), Some(("saving", true)));
    }

    #[test]
    fn an_automatic_check_downloads_during_a_recording_and_installs_after_it() {
        let rig = rig("update-auto-recording", ok_capture());
        let fakes = Fakes::new();
        let calls = fakes.calls.clone();
        let updater = fakes.updater(&rig.recorder);
        save_auto_update(&store_path(&rig), true);
        rig.start().unwrap();
        // Not joined: the thread waits at the gate until the recording is saved.
        assert!(updater.tick(&rig.recorder, &store_path(&rig)).is_some());
        wait_for("ready", || status(&updater, &rig)["state"] == "ready");
        assert_eq!(calls.downloads.load(Ordering::SeqCst), 1);
        assert_eq!(calls.installs.load(Ordering::SeqCst), 0);
        rig.stop(crate::recorder::Reason::User);
        wait_for("the restart", || calls.restarts.load(Ordering::SeqCst) == 1);
    }

    #[test]
    fn the_auto_update_setting_round_trips() {
        let rig = rig("update-setting", ok_capture());
        let path = store_path(&rig);
        assert!(!auto_update(&path));
        assert_eq!(
            save_auto_update(&path, true),
            json!({"ok": true, "auto_update": true})
        );
        assert!(auto_update(&path));
        assert_eq!(
            Store::open(&path)
                .unwrap()
                .setting(AUTO_UPDATE_KEY)
                .unwrap()
                .as_deref(),
            Some("true")
        );
        assert_eq!(
            save_auto_update(&path, false),
            json!({"ok": true, "auto_update": false})
        );
        assert!(!auto_update(&path));
        Store::open(&path)
            .unwrap()
            .set_setting(AUTO_UPDATE_KEY, "yes")
            .unwrap();
        assert!(!auto_update(&path));
    }

    #[test]
    fn an_unwritable_store_is_store_unavailable_and_reads_as_off() {
        let rig = rig("update-store", ok_capture());
        let path = store_path(&rig);
        fs::create_dir_all(&path).unwrap();
        assert_eq!(save_auto_update(&path, true)["code"], "store_unavailable");
        assert!(!auto_update(&path));
        let updater = Fakes::new().updater(&rig.recorder);
        assert_eq!(updater.status(&path)["auto_update"], false);
    }
}
