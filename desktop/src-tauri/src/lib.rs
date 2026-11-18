//! The Meeting desktop app's Rust side: one window, the commands the UI calls and the launch
//! hooks. Built without `FEATURE_DESKTOP_APP=1` it opens a window that says so and nothing else.

use tauri::{AppHandle, Manager, RunEvent, Runtime, WebviewUrl, WebviewWindowBuilder, WindowEvent};

pub mod admin;
pub mod calendar;
pub mod capture;
pub mod env;
pub mod indicator;
pub mod link;
pub mod notes;
pub mod recorder;
pub mod reply;
pub mod tray;
pub mod updater;

/// The state every command reads, managed once at startup.
pub struct App {
    pub env: env::Env,
    pub recorder: recorder::Recorder,
    pub calendar: calendar::Calendar,
    pub notes: notes::Notes,
    pub admin: admin::Admin,
    pub updater: updater::Updater,
}

fn flag_gate(value: Option<&str>) -> bool {
    value == Some("1")
}

/// Whether the app was built with `FEATURE_DESKTOP_APP=1`. Read at build time, as the tool does.
pub fn desktop_enabled() -> bool {
    flag_gate(option_env!("FEATURE_DESKTOP_APP"))
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Mode {
    Off,
    On,
}

fn mode(enabled: bool) -> Mode {
    if enabled { Mode::On } else { Mode::Off }
}

impl Mode {
    fn window_label(self) -> &'static str {
        match self {
            Mode::Off => "off",
            Mode::On => "main",
        }
    }

    fn window_page(self) -> &'static str {
        match self {
            Mode::Off => "off.html",
            Mode::On => "index.html",
        }
    }
}

/// The builder for `mode`, without the single-instance plugin: `run` adds that, so tests can
/// build several apps in one process.
fn configure<R: Runtime>(builder: tauri::Builder<R>, mode: Mode) -> tauri::Builder<R> {
    let builder = builder.setup(move |app| Ok(launch(app.handle(), mode)?));
    match mode {
        Mode::Off => builder,
        Mode::On => notes::media::register(builder).invoke_handler(tauri::generate_handler![
            recorder::recorder_status,
            recorder::recorder_start,
            recorder::recorder_pause,
            recorder::recorder_resume,
            recorder::recorder_stop,
            recorder::recorder_retry,
            recorder::recorder_dismiss_start_error,
            calendar::calendar_status,
            calendar::calendar_events,
            calendar::calendar_event,
            calendar::calendars,
            calendar::set_hidden_calendars,
            notes::notes_list,
            notes::note_get,
            notes::note_update,
            notes::note_rerun,
            notes::note_open,
            notes::note_copy,
            notes::speaker_suggestions,
            notes::note_delete_recording,
            admin::rescan,
            admin::rescan_status,
            admin::health,
            admin::note_archive,
            admin::note_trash,
            admin::settings_info,
            updater::update_status,
            updater::update_check,
            updater::update_install,
            updater::set_auto_update,
            indicator::indicator_settings,
            indicator::set_indicator_enabled,
            indicator::indicator_dismiss,
            indicator::window_open_main,
        ]),
    }
}

/// What startup does: the state first, so no command can run without it, then the tool swap
/// (FR-27), so every job the app starts uses the bundled tool, then the window, then the hooks.
/// Built on, the main window starts hidden: the link hook decides whether it shows (FR-4).
fn launch<R: Runtime>(app: &AppHandle<R>, mode: Mode) -> anyhow::Result<()> {
    launch_in(app, mode, env::Env::new, updater::Updater::new)
}

/// `launch` with the `Env` from `new_env` and the `Updater` from `new_updater`, so tests keep
/// every command off the real home and the update server.
fn launch_in<R: Runtime>(
    app: &AppHandle<R>,
    mode: Mode,
    new_env: impl FnOnce() -> anyhow::Result<env::Env>,
    new_updater: impl FnOnce(&AppHandle<R>) -> updater::Updater,
) -> anyhow::Result<()> {
    if mode == Mode::On {
        let env = new_env()?;
        app.manage(App {
            recorder: recorder::Recorder::new(&env),
            calendar: calendar::Calendar::new(&env),
            notes: notes::Notes::new(&env),
            admin: admin::Admin::new(&env),
            updater: new_updater(app),
            env,
        });
        // Before the window and the hooks: a command, recovery (FR-7) or a rerun can spawn.
        updater::swap_tool(app);
    }
    WebviewWindowBuilder::new(
        app,
        mode.window_label(),
        WebviewUrl::App(mode.window_page().into()),
    )
    .title("Meeting")
    .inner_size(1120.0, 760.0)
    .min_inner_size(720.0, 520.0)
    .visible(mode == Mode::Off)
    .build()?;
    if mode == Mode::On {
        tray::setup(app);
        link::on_launch(app);
        indicator::on_launch(app);
        recorder::on_launch(app);
        admin::on_launch(app);
        calendar::on_launch(app);
        recorder::on_sleep(app);
        updater::on_launch(app);
    }
    Ok(())
}

/// A second launch, or the menu-bar item's Open Meeting: bring the window forward.
pub(crate) fn focus_main<R: Runtime>(app: &AppHandle<R>) {
    if let Some(window) = app.get_webview_window(Mode::On.window_label()) {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

/// D15: closing the main window hides it; the app and its menu-bar item keep running until Quit.
fn hides_on_close(mode: Mode, label: &str) -> bool {
    mode == Mode::On && label == Mode::On.window_label()
}

/// Whether an exit request goes through the recorder's quit rules (FR-5). Not the updater's
/// relaunch: it holds the recorder in `saving` until the process ends, so waiting for that
/// save would never return, and Tauri does not let a restart be prevented anyway.
fn asks_recorder(mode: Mode, code: Option<i32>) -> bool {
    mode == Mode::On && code != Some(tauri::RESTART_EXIT_CODE)
}

pub fn run() {
    meeting::logging::init();
    let mode = mode(desktop_enabled());
    let mut builder = tauri::Builder::default();
    if mode == Mode::On {
        // Registered before anything else, as the plugin requires. Rust side only: no
        // capability lists it.
        builder = builder
            .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
                focus_main(app)
            }))
            // Rust side only (D47): no capability lists it.
            .plugin(tauri_plugin_updater::Builder::new().build());
    }
    match configure(builder, mode).build(tauri::generate_context!()) {
        Ok(app) => app.run(move |app, event| match event {
            RunEvent::ExitRequested { api, code, .. } => {
                // Built off, no hook runs: quitting is never held back.
                // The updater first: while it installs, the recorder's quit would wait forever.
                if asks_recorder(mode, code)
                    && !(updater::on_exit_requested(app) && recorder::on_exit_requested(app))
                {
                    api.prevent_exit();
                }
            }
            RunEvent::WindowEvent {
                label,
                event: WindowEvent::CloseRequested { api, .. },
                ..
            } if hides_on_close(mode, &label) => {
                api.prevent_close();
                if let Some(window) = app.get_webview_window(&label) {
                    let _ = window.hide();
                }
            }
            // The Dock icon brings a hidden window back.
            RunEvent::Reopen { .. } if mode == Mode::On => focus_main(app),
            // A `meeting-manager://` link, at launch or while running (FR-1).
            #[cfg(target_os = "macos")]
            RunEvent::Opened { urls } if mode == Mode::On => link::on_opened(app, urls),
            _ => {}
        }),
        Err(e) => {
            tracing::error!("the app could not start: {e}");
            eprintln!("Meeting could not start: {e}");
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::ops::Deref;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};

    use serde_json::Value;
    use tauri::ipc::{CallbackFn, InvokeBody};
    use tauri::test::{
        INVOKE_KEY, MockRuntime, get_ipc_response, mock_builder, mock_context, noop_assets,
    };
    use tauri::webview::InvokeRequest;

    use super::*;

    /// Every command of the PRD's API table.
    const COMMANDS: [&str; 34] = [
        "recorder_status",
        "recorder_start",
        "recorder_pause",
        "recorder_resume",
        "recorder_stop",
        "recorder_retry",
        "recorder_dismiss_start_error",
        "calendar_status",
        "calendar_events",
        "calendar_event",
        "calendars",
        "set_hidden_calendars",
        "notes_list",
        "note_get",
        "note_update",
        "note_rerun",
        "note_open",
        "note_copy",
        "speaker_suggestions",
        "note_delete_recording",
        "rescan",
        "rescan_status",
        "health",
        "note_archive",
        "note_trash",
        "settings_info",
        "update_status",
        "update_check",
        "update_install",
        "set_auto_update",
        "indicator_settings",
        "set_indicator_enabled",
        "indicator_dismiss",
        "window_open_main",
    ];

    /// A started app on the mock runtime, its `Env` in a temp home removed on drop: invoking a
    /// command must never touch the real store, vault or recordings.
    struct Started {
        app: tauri::App<MockRuntime>,
        home: PathBuf,
    }

    impl Deref for Started {
        type Target = tauri::App<MockRuntime>;

        fn deref(&self) -> &Self::Target {
            &self.app
        }
    }

    impl Drop for Started {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.home);
        }
    }

    /// The mock runtime runs `setup` only inside its event loop, so the test does what `setup`
    /// does: call `launch`, here with a temp home.
    fn started(mode: Mode) -> Started {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let home = std::env::temp_dir().join(format!(
            "meeting-desktop-lib-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&home).unwrap();
        let app = configure(mock_builder(), mode)
            .build(mock_context(noop_assets()))
            .unwrap();
        let env_home = home.clone();
        launch_in(
            app.handle(),
            mode,
            || Ok(env::Env::at(env_home)),
            updater::tests::unplugged,
        )
        .unwrap();
        Started { app, home }
    }

    fn invoke(app: &tauri::App<MockRuntime>, window: &str, command: &str) -> Result<Value, Value> {
        let webview = app.get_webview_window(window).unwrap();
        let request = InvokeRequest {
            cmd: command.into(),
            callback: CallbackFn(0),
            error: CallbackFn(1),
            url: "tauri://localhost".parse().unwrap(),
            body: InvokeBody::default(),
            headers: Default::default(),
            invoke_key: INVOKE_KEY.to_string(),
        };
        get_ipc_response(&webview, request).map(|body| body.deserialize().unwrap())
    }

    fn labels(app: &tauri::App<MockRuntime>) -> Vec<String> {
        app.webview_windows().into_keys().collect()
    }

    fn manifest_file(name: &str) -> String {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(name);
        fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
    }

    fn json_file(name: &str) -> Value {
        serde_json::from_str(&manifest_file(name)).unwrap()
    }

    #[test]
    fn the_gate_is_on_only_for_the_value_1() {
        assert!(flag_gate(Some("1")));
        for off in [None, Some("0"), Some(""), Some("true"), Some("yes")] {
            assert!(!flag_gate(off), "{off:?}");
        }
        assert_eq!(
            desktop_enabled(),
            flag_gate(option_env!("FEATURE_DESKTOP_APP"))
        );
    }

    #[test]
    fn the_mode_follows_the_flag() {
        assert_eq!(mode(false), Mode::Off);
        assert_eq!(mode(true), Mode::On);
        assert_eq!(
            (Mode::Off.window_label(), Mode::Off.window_page()),
            ("off", "off.html")
        );
        assert_eq!(
            (Mode::On.window_label(), Mode::On.window_page()),
            ("main", "index.html")
        );
    }

    #[test]
    fn only_the_main_window_hides_on_close() {
        assert!(hides_on_close(Mode::On, "main"));
        assert!(!hides_on_close(Mode::On, "other"));
        assert!(!hides_on_close(Mode::Off, "off"));
        assert!(!hides_on_close(Mode::Off, "main"));
    }

    #[test]
    fn off_opens_one_window_and_registers_nothing() {
        let app = started(Mode::Off);
        assert_eq!(labels(&app), ["off"]);
        assert_eq!(
            app.get_webview_window("off").unwrap().url().unwrap().path(),
            "/off.html"
        );
        assert!(app.try_state::<App>().is_none());
        for command in COMMANDS {
            let error = invoke(&app, "off", command).unwrap_err();
            assert_eq!(error, format!("Command {command} not found"));
        }
    }

    #[test]
    fn on_opens_the_main_window_with_the_state_and_every_command() {
        let app = started(Mode::On);
        assert_eq!(labels(&app), ["main"]);
        // Tauri serves `index.html` as the root of the app's origin.
        let url = app.get_webview_window("main").unwrap().url().unwrap();
        assert!(matches!(url.path(), "" | "/" | "/index.html"), "{url}");
        assert_eq!(app.state::<App>().env.home, app.home);
        for command in COMMANDS {
            // Only that it is registered: the issue that owns a command changes what it answers.
            let reply = invoke(&app, "main", command);
            let not_found = Value::String(format!("Command {command} not found"));
            assert_ne!(reply.as_ref().err(), Some(&not_found), "{command}");
        }
        let unknown = invoke(&app, "main", "no_such_command").unwrap_err();
        assert_eq!(unknown, "Command no_such_command not found");
    }

    fn recording_request(
        method: &str,
        path: &str,
        range: Option<&str>,
    ) -> tauri::http::Request<Vec<u8>> {
        let mut request = tauri::http::Request::builder()
            .method(method)
            .uri(format!("recording://localhost{path}"));
        if let Some(range) = range {
            request = request.header("Range", range);
        }
        request.body(Vec::new()).unwrap()
    }

    #[test]
    fn the_recording_scheme_answers_from_the_started_state() {
        let app = started(Mode::On);
        let recordings = app.home.join("Recordings");
        let folder = recordings.join("m");
        fs::create_dir_all(&folder).unwrap();
        let path = folder.join("recording.m4a");
        fs::write(&path, b"0123456789").unwrap();
        meeting::config::Config {
            recordings_dir: recordings,
            vault_dir: app.home.join("Vault"),
            vault_folder: "Meetings".into(),
            ffmpeg_path: "/opt/homebrew/bin/ffmpeg".into(),
            claude_path: "/usr/local/bin/claude".into(),
            extension_id: "abcdefghijklmnopabcdefghijklmnop".into(),
            meeting_path: None,
        }
        .save_to(&app.state::<App>().env.config_path())
        .unwrap();
        let result = meeting::store::RunResult {
            status: meeting::store::Status::Done,
            failed_stage: None,
            error: None,
            title: Some("Standup".into()),
            body_md: Some("## Summary\n".into()),
            speakers: vec![],
            language: None,
            duration_s: Some(1),
            transcript: Some("[00:00:01] Speaker 1: Hi.\n".into()),
            event: None,
            source: meeting::store::Source::Browser,
            recorded_at: "2026-10-19T14:30".into(),
            attendees: None,
        };
        let id = app
            .state::<App>()
            .env
            .store()
            .unwrap()
            .merge_run(&path, &result)
            .unwrap()
            .id;

        let response = notes::media::respond(
            app.handle(),
            &recording_request("GET", &format!("/{id}"), Some("bytes=2-4")),
        );
        assert_eq!(response.status(), 206);
        assert_eq!(response.headers()["content-range"], "bytes 2-4/10");
        assert_eq!(response.headers()["content-type"], "audio/mp4");
        assert_eq!(response.headers()["accept-ranges"], "bytes");
        assert_eq!(response.body(), b"234");

        let unknown = notes::media::respond(app.handle(), &recording_request("GET", "/999", None));
        assert_eq!(unknown.status(), 404);
        assert!(unknown.body().is_empty());
        let head = notes::media::respond(
            app.handle(),
            &recording_request("HEAD", &format!("/{id}"), None),
        );
        assert_eq!(head.status(), 405);
        assert_eq!(head.headers()["allow"], "GET");
    }

    #[test]
    fn the_recording_scheme_is_404_without_the_state() {
        let app = started(Mode::Off);
        let response = notes::media::respond(app.handle(), &recording_request("GET", "/1", None));
        assert_eq!(response.status(), 404);
    }

    #[test]
    fn retry_refuses_a_folder_that_is_not_a_leftover() {
        let app = started(Mode::On);
        let webview = app.get_webview_window("main").unwrap();
        let request = InvokeRequest {
            cmd: "recorder_retry".into(),
            callback: CallbackFn(0),
            error: CallbackFn(1),
            url: "tauri://localhost".parse().unwrap(),
            body: InvokeBody::Json(serde_json::json!({"folder": "/etc"})),
            headers: Default::default(),
            invoke_key: INVOKE_KEY.to_string(),
        };
        let reply: Value = get_ipc_response(&webview, request)
            .unwrap()
            .deserialize()
            .unwrap();
        assert_eq!(reply["ok"], false);
        assert_eq!(reply["code"], "not_found");
    }

    #[test]
    fn the_recorder_commands_answer_through_ipc() {
        let app = started(Mode::On);
        let status = invoke(&app, "main", "recorder_status").unwrap();
        assert_eq!(status["ok"], true);
        assert_eq!(status["state"], "idle");
        assert_eq!(status["language"], "bn");
        for command in ["recorder_pause", "recorder_resume", "recorder_stop"] {
            let reply = invoke(&app, "main", command).unwrap();
            assert_eq!(reply["code"], "not_recording", "{command}");
        }
    }

    #[test]
    fn the_update_status_answers_through_ipc() {
        let app = started(Mode::On);
        let status = invoke(&app, "main", "update_status").unwrap();
        assert_eq!(status["ok"], true);
        assert_eq!(status["state"], "idle");
        assert_eq!(status["version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(status["auto_update"], false);
        // No bundled tool in tests (D50): the launch's swap did nothing.
        assert_eq!(status["tool_error"], Value::Null);
        let install = invoke(&app, "main", "update_install").unwrap();
        assert_eq!(install["code"], "no_update");
        let check = invoke(&app, "main", "update_check").unwrap();
        assert_eq!(check, serde_json::json!({"ok": true, "started": true}));
        assert!(updater::on_exit_requested(app.handle()));
    }

    #[test]
    fn set_auto_update_answers_through_ipc() {
        let app = started(Mode::On);
        let webview = app.get_webview_window("main").unwrap();
        let request = InvokeRequest {
            cmd: "set_auto_update".into(),
            callback: CallbackFn(0),
            error: CallbackFn(1),
            url: "tauri://localhost".parse().unwrap(),
            body: InvokeBody::Json(serde_json::json!({"enabled": true})),
            headers: Default::default(),
            invoke_key: INVOKE_KEY.to_string(),
        };
        let reply: Value = get_ipc_response(&webview, request)
            .unwrap()
            .deserialize()
            .unwrap();
        assert_eq!(reply, serde_json::json!({"ok": true, "auto_update": true}));
        assert_eq!(
            invoke(&app, "main", "update_status").unwrap()["auto_update"],
            true
        );
    }

    #[test]
    fn the_quick_record_stubs_answer_the_contract_through_ipc() {
        let app = started(Mode::On);
        assert_eq!(
            invoke(&app, "main", "recorder_status").unwrap()["start_error"],
            Value::Null
        );
        for (command, reply) in [
            (
                "recorder_dismiss_start_error",
                serde_json::json!({"ok": true}),
            ),
            (
                "indicator_settings",
                serde_json::json!({"ok": true, "enabled": true}),
            ),
            ("indicator_dismiss", serde_json::json!({"ok": true})),
            ("window_open_main", serde_json::json!({"ok": true})),
        ] {
            assert_eq!(invoke(&app, "main", command).unwrap(), reply, "{command}");
        }
        let webview = app.get_webview_window("main").unwrap();
        for enabled in [false, true] {
            let request = InvokeRequest {
                cmd: "set_indicator_enabled".into(),
                callback: CallbackFn(0),
                error: CallbackFn(1),
                url: "tauri://localhost".parse().unwrap(),
                body: InvokeBody::Json(serde_json::json!({"enabled": enabled})),
                headers: Default::default(),
                invoke_key: INVOKE_KEY.to_string(),
            };
            let reply: Value = get_ipc_response(&webview, request)
                .unwrap()
                .deserialize()
                .unwrap();
            assert_eq!(reply, serde_json::json!({"ok": true, "enabled": enabled}));
        }
    }

    #[test]
    fn only_the_relaunch_skips_the_recorders_quit_rules() {
        assert!(asks_recorder(Mode::On, None));
        assert!(asks_recorder(Mode::On, Some(0)));
        assert!(!asks_recorder(Mode::On, Some(tauri::RESTART_EXIT_CODE)));
        assert!(!asks_recorder(Mode::Off, None));
    }

    #[test]
    fn quitting_is_allowed() {
        let app = started(Mode::On);
        assert!(recorder::on_exit_requested(app.handle()));
    }

    #[test]
    fn the_ui_contract_names_the_same_commands() {
        let api = manifest_file("../src/api.ts");
        for command in COMMANDS {
            assert!(api.contains(&format!("'{command}'")), "{command}");
        }
        assert_eq!(api.matches("call<").count() - 1, COMMANDS.len());
    }

    #[test]
    fn the_config_is_locked_down() {
        let config = json_file("tauri.conf.json");
        assert_eq!(config["productName"], "Meeting");
        assert_eq!(config["identifier"], "com.masnunsiam.meeting-manager");
        assert_eq!(config["build"]["frontendDist"], "../dist");
        assert_eq!(config["build"]["devUrl"], "http://localhost:1420");
        assert_eq!(config["app"]["windows"], serde_json::json!([]));
        assert_eq!(config["app"]["withGlobalTauri"], false);
        // The indicator's transparent window (D17); `tauri-build` checks the cargo feature
        // matches.
        assert_eq!(config["app"]["macOSPrivateApi"], true);
        assert!(config["app"]["security"].get("assetProtocol").is_none());
        assert_eq!(config["bundle"]["targets"], serde_json::json!(["app"]));
        let macos = &config["bundle"]["macOS"];
        assert_eq!(macos["minimumSystemVersion"], "14.4");
        assert_eq!(macos["signingIdentity"], "-");
        // Release-only settings live in the overlay (D50), so a local build needs no signing
        // key and no prebuilt `meeting`.
        assert!(config["bundle"].get("createUpdaterArtifacts").is_none());
        assert!(config["bundle"].get("resources").is_none());
        let updater = &config["plugins"]["updater"];
        assert_eq!(
            updater["endpoints"],
            serde_json::json!([
                "https://github.com/masnun-siam/meeting-manager/releases/latest/download/latest.json"
            ])
        );
        assert!(!updater["pubkey"].as_str().unwrap().is_empty());
    }

    #[test]
    fn the_release_overlay_adds_only_the_bundle_settings() {
        let overlay = json_file("tauri.release.conf.json");
        assert_eq!(
            overlay,
            serde_json::json!({
                "bundle": {
                    "createUpdaterArtifacts": true,
                    "resources": { "../../target/release/meeting": "meeting" }
                }
            })
        );
    }

    /// The `version` under `[package]` of a Cargo manifest.
    fn package_version(manifest: &str) -> &str {
        let package = manifest
            .split("\n[")
            .find(|section| section.trim_start_matches('[').starts_with("package]"))
            .expect("a [package] section");
        package
            .lines()
            .find_map(|line| line.strip_prefix("version = \""))
            .and_then(|rest| rest.strip_suffix('"'))
            .expect("a package version")
    }

    #[test]
    fn the_versions_match() {
        let app = json_file("tauri.conf.json");
        let tool = manifest_file("../../crates/meeting/Cargo.toml");
        assert_eq!(app["version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(package_version(&tool), env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn the_package_version_is_read_from_the_package_section() {
        let manifest =
            "[package]\nname = \"x\"\nversion = \"1.2.3\"\n\n[dependencies]\nversion = \"9\"\n";
        assert_eq!(package_version(manifest), "1.2.3");
    }

    #[test]
    fn the_csp_allows_no_remote_content() {
        let config = json_file("tauri.conf.json");
        let security = &config["app"]["security"];
        let csp = security["csp"].as_str().unwrap();
        let directives: Vec<&str> = csp.split(';').map(str::trim).collect();
        assert_eq!(
            directives,
            [
                "default-src 'self'",
                "connect-src ipc: http://ipc.localhost",
                "media-src recording:",
                "object-src 'none'",
                "base-uri 'none'",
                "frame-ancestors 'none'",
            ]
        );
        // The dev server's websocket is the only thing development adds.
        let dev = security["devCsp"].as_str().unwrap();
        assert_eq!(dev.replace(" ws://localhost:1420", ""), csp);
        for unsafe_source in ["unsafe-inline", "unsafe-eval", "https:", "*"] {
            assert!(!dev.contains(unsafe_source), "{unsafe_source}");
        }
    }

    #[test]
    fn the_capabilities_grant_the_windows_nothing_extra() {
        let mut names: Vec<_> =
            fs::read_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("capabilities"))
                .unwrap()
                .map(|entry| entry.unwrap().file_name().into_string().unwrap())
                .collect();
        names.sort();
        assert_eq!(names, ["default.json", "indicator.json"]);
        let default = json_file("capabilities/default.json");
        assert_eq!(default["windows"], serde_json::json!(["main", "indicator"]));
        assert!(default.get("webviews").is_none());
        assert_eq!(default["permissions"], serde_json::json!([]));
        // D19: only the pill's drag region.
        let indicator = json_file("capabilities/indicator.json");
        assert_eq!(indicator["windows"], serde_json::json!(["indicator"]));
        assert!(indicator.get("webviews").is_none());
        assert_eq!(
            indicator["permissions"],
            serde_json::json!(["core:window:allow-start-dragging"])
        );
    }

    /// The `<string>` values of the array after `<key>CFBundleURLSchemes</key>`.
    fn url_schemes(plist: &str) -> Vec<&str> {
        let mut keys = plist.split("<key>CFBundleURLSchemes</key>").skip(1);
        let after = keys.next().expect("CFBundleURLSchemes is missing");
        assert!(
            keys.next().is_none(),
            "CFBundleURLSchemes is declared twice"
        );
        let (array, _) = after.split_once("</array>").expect("an array of schemes");
        array
            .split("<string>")
            .skip(1)
            .map(|value| value.split_once("</string>").unwrap().0.trim())
            .collect()
    }

    #[test]
    fn the_url_scheme_is_only_meeting_manager() {
        assert_eq!(
            url_schemes(&manifest_file("Info.plist")),
            ["meeting-manager"]
        );
    }

    #[test]
    fn the_url_scheme_check_catches_an_extra_scheme() {
        let two =
            "<key>CFBundleURLSchemes</key><array><string>a</string>\n<string>b</string></array>";
        assert_eq!(url_schemes(two), ["a", "b"]);
        let twice = format!("{two}{two}");
        assert!(std::panic::catch_unwind(|| url_schemes(&twice)).is_err());
    }

    #[test]
    fn the_privacy_strings_are_set() {
        let plist = manifest_file("Info.plist");
        for key in [
            "NSMicrophoneUsageDescription",
            "NSAudioCaptureUsageDescription",
            "NSCalendarsFullAccessUsageDescription",
        ] {
            let (_, after) = plist
                .split_once(&format!("<key>{key}</key>"))
                .unwrap_or_else(|| panic!("{key} is missing"));
            let (_, value) = after.split_once("<string>").unwrap();
            let (value, _) = value.split_once("</string>").unwrap();
            assert!(value.trim().len() > 20, "{key}: {value:?}");
        }
    }

    /// The bundle is signed with the hardened runtime, under which macOS refuses the
    /// microphone and calendar without a prompt unless these entitlements are present.
    #[test]
    fn the_hardened_runtime_entitlements_are_set() {
        let conf = json_file("tauri.conf.json");
        assert_eq!(
            conf["bundle"]["macOS"]["entitlements"],
            "./Entitlements.plist"
        );
        let plist = manifest_file("Entitlements.plist");
        for key in [
            "com.apple.security.device.audio-input",
            "com.apple.security.personal-information.calendars",
        ] {
            let (_, after) = plist
                .split_once(&format!("<key>{key}</key>"))
                .unwrap_or_else(|| panic!("{key} is missing"));
            assert!(
                after.trim_start().starts_with("<true/>"),
                "{key} is not true"
            );
        }
    }
}
