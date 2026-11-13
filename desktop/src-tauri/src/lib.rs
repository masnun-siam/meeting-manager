//! The Meeting desktop app's Rust side: one window, the commands the UI calls and the launch
//! hooks. Built without `FEATURE_DESKTOP_APP=1` it opens a window that says so and nothing else.

use tauri::{AppHandle, Manager, RunEvent, Runtime, WebviewUrl, WebviewWindowBuilder};

pub mod admin;
pub mod calendar;
pub mod capture;
pub mod env;
pub mod notes;
pub mod recorder;
pub mod reply;
pub mod tray;

/// The state every command reads, managed once at startup.
pub struct App {
    pub env: env::Env,
    pub recorder: recorder::Recorder,
    pub calendar: calendar::Calendar,
    pub notes: notes::Notes,
    pub admin: admin::Admin,
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
        Mode::On => builder.invoke_handler(tauri::generate_handler![
            recorder::recorder_status,
            recorder::recorder_start,
            recorder::recorder_pause,
            recorder::recorder_resume,
            recorder::recorder_stop,
            recorder::recorder_retry,
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
            admin::rescan,
            admin::rescan_status,
            admin::health,
            admin::note_archive,
            admin::note_trash,
            admin::settings_info,
        ]),
    }
}

/// What startup does: the state first, so no command can run without it, then the window, then
/// the hooks.
fn launch<R: Runtime>(app: &AppHandle<R>, mode: Mode) -> anyhow::Result<()> {
    if mode == Mode::On {
        let env = env::Env::new()?;
        app.manage(App {
            recorder: recorder::Recorder::new(&env),
            calendar: calendar::Calendar::new(&env),
            notes: notes::Notes::new(&env),
            admin: admin::Admin::new(&env),
            env,
        });
    }
    WebviewWindowBuilder::new(
        app,
        mode.window_label(),
        WebviewUrl::App(mode.window_page().into()),
    )
    .title("Meeting")
    .build()?;
    if mode == Mode::On {
        tray::setup(app);
        recorder::on_launch(app);
        admin::on_launch(app);
        calendar::on_launch(app);
        recorder::on_sleep(app);
    }
    Ok(())
}

/// A second launch lands here, in the first instance: bring its window forward.
fn focus_main<R: Runtime>(app: &AppHandle<R>) {
    if let Some(window) = app.get_webview_window(Mode::On.window_label()) {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

pub fn run() {
    meeting::logging::init();
    let mode = mode(desktop_enabled());
    let mut builder = tauri::Builder::default();
    if mode == Mode::On {
        // Registered before anything else, as the plugin requires. Rust side only: no
        // capability lists it.
        builder = builder.plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            focus_main(app)
        }));
    }
    match configure(builder, mode).build(tauri::generate_context!()) {
        Ok(app) => app.run(move |app, event| {
            if let RunEvent::ExitRequested { api, .. } = event {
                // Built off, no hook runs: quitting is never held back.
                if mode == Mode::On && !recorder::on_exit_requested(app) {
                    api.prevent_exit();
                }
            }
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
    use std::path::Path;

    use serde_json::Value;
    use tauri::ipc::{CallbackFn, InvokeBody};
    use tauri::test::{
        INVOKE_KEY, MockRuntime, get_ipc_response, mock_builder, mock_context, noop_assets,
    };
    use tauri::webview::InvokeRequest;

    use super::*;

    /// Every command of the PRD's API table.
    const COMMANDS: [&str; 22] = [
        "recorder_status",
        "recorder_start",
        "recorder_pause",
        "recorder_resume",
        "recorder_stop",
        "recorder_retry",
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
        "rescan",
        "rescan_status",
        "health",
        "note_archive",
        "note_trash",
        "settings_info",
    ];

    /// A started app on the mock runtime. The mock runtime runs `setup` only inside its event
    /// loop, so the test does what `setup` does: call `launch`.
    fn started(mode: Mode) -> tauri::App<MockRuntime> {
        let app = configure(mock_builder(), mode)
            .build(mock_context(noop_assets()))
            .unwrap();
        launch(app.handle(), mode).unwrap();
        app
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
        assert!(app.try_state::<App>().is_some());
        for command in COMMANDS {
            // Only that it is registered: the issue that owns a command changes what it answers.
            let reply = invoke(&app, "main", command);
            let not_found = Value::String(format!("Command {command} not found"));
            assert_ne!(reply.as_ref().err(), Some(&not_found), "{command}");
        }
        let unknown = invoke(&app, "main", "no_such_command").unwrap_err();
        assert_eq!(unknown, "Command no_such_command not found");
    }

    #[test]
    fn a_stub_answers_not_implemented() {
        let app = started(Mode::On);
        let reply = invoke(&app, "main", "recorder_status").unwrap();
        assert_eq!(reply["ok"], false);
        assert_eq!(reply["code"], "not_implemented");
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
        assert_eq!(config["identifier"], "com.meeting-manager.desktop");
        assert_eq!(config["build"]["frontendDist"], "../dist");
        assert_eq!(config["build"]["devUrl"], "http://localhost:1420");
        assert_eq!(config["app"]["windows"], serde_json::json!([]));
        assert_eq!(config["app"]["withGlobalTauri"], false);
        assert!(config["app"]["security"].get("assetProtocol").is_none());
        assert_eq!(config["bundle"]["targets"], serde_json::json!(["app"]));
        let macos = &config["bundle"]["macOS"];
        assert_eq!(macos["minimumSystemVersion"], "14.4");
        assert_eq!(macos["signingIdentity"], "-");
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
    fn the_capability_grants_the_main_window_nothing_extra() {
        let names: Vec<_> =
            fs::read_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("capabilities"))
                .unwrap()
                .map(|entry| entry.unwrap().file_name().into_string().unwrap())
                .collect();
        assert_eq!(names, ["default.json"]);
        let capability = json_file("capabilities/default.json");
        assert_eq!(capability["windows"], serde_json::json!(["main"]));
        assert!(capability.get("webviews").is_none());
        for permission in capability["permissions"].as_array().unwrap() {
            assert_eq!(permission, "core:default");
        }
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
}
