//! The menu-bar item (PRD desktop-app FR-3, D15): the recording state and its controls, with the
//! window closed. It follows every recorder change (`Recorder::on_change`), from the window too.
//! Tray and menu calls run on the main thread; other threads queue them there and never wait,
//! so a main thread blocked in `quit` cannot deadlock a render.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use objc2::MainThreadMarker;
use tauri::image::Image;
use tauri::menu::{IsMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tauri::tray::{TrayIcon, TrayIconBuilder};
use tauri::{AppHandle, Manager, Runtime};

use crate::App;
use crate::recorder::notice::Kind;
use crate::recorder::{Reason, Recorder, Status};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Icon {
    Idle,
    Recording,
    Paused,
}

impl Icon {
    fn png(self) -> &'static [u8] {
        match self {
            Icon::Idle => include_bytes!("../icons/tray-idle.png"),
            Icon::Recording => include_bytes!("../icons/tray-recording.png"),
            Icon::Paused => include_bytes!("../icons/tray-paused.png"),
        }
    }

    /// Idle and paused are monochrome templates macOS tints; the recording dot stays red.
    fn template(self) -> bool {
        self != Icon::Recording
    }
}

/// What the menu-bar item shows for a recorder status.
#[derive(Debug, PartialEq)]
pub struct MenuState {
    pub title: Option<String>,
    pub icon: Icon,
    pub start_enabled: bool,
    /// Idle with something to continue (continue-recording FR-6).
    pub continue_enabled: bool,
    pub pause_label: &'static str,
    pub pause_enabled: bool,
    pub stop_enabled: bool,
    /// The first, disabled line: a start error, else an `error` or `setup` notice.
    pub notice: Option<String>,
}

pub fn menu_state(status: &Status, start_error: Option<&str>) -> MenuState {
    let (icon, controls) = match status.state {
        "recording" => (Icon::Recording, true),
        "paused" => (Icon::Paused, true),
        _ => (Icon::Idle, false),
    };
    let notice = start_error.map(str::to_string).or_else(|| {
        status
            .notice
            .as_ref()
            .filter(|n| matches!(n.kind, Kind::Error | Kind::Setup))
            .map(|n| n.message.clone())
    });
    MenuState {
        title: controls.then(|| clock(status.elapsed_s)),
        icon,
        start_enabled: status.state == "idle",
        continue_enabled: status.state == "idle" && status.continuable.is_some(),
        pause_label: if icon == Icon::Paused {
            "Resume"
        } else {
            "Pause"
        },
        pause_enabled: controls,
        stop_enabled: controls,
        notice,
    }
}

const SEPARATOR: &str = "separator";

/// The menu's item ids in order, the notice line aside (it is prepended while shown). The
/// Continue item exists only in a build with `FEATURE_CONTINUE_RECORDING` (FR-14).
fn menu_ids(continue_enabled: bool) -> Vec<&'static str> {
    let mut ids = vec!["start"];
    if continue_enabled {
        ids.push("continue");
    }
    ids.extend(["pause", "stop", SEPARATOR, "open", "quit"]);
    ids
}

/// Whether the Continue item, shown enabled or not as `shown`, no longer matches `status`.
fn continue_stale(status: &Status, shown: bool) -> bool {
    menu_state(status, None).continue_enabled != shown
}

/// `H:MM:SS`.
pub fn clock(secs: u64) -> String {
    format!("{}:{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60)
}

struct Tray<R: Runtime> {
    app: AppHandle<R>,
    recorder: Recorder,
    icon: TrayIcon<R>,
    menu: Menu<R>,
    notice: MenuItem<R>,
    start: MenuItem<R>,
    /// `None` in a build without `FEATURE_CONTINUE_RECORDING`.
    continue_last: Option<MenuItem<R>>,
    /// Whether `continue_last` was last shown enabled.
    continue_shown: AtomicBool,
    pause: MenuItem<R>,
    stop: MenuItem<R>,
}

/// Called once at startup, on the main thread, before the other launch hooks. Without the main
/// thread (tests) or when AppKit refuses the item, the app runs without it.
pub fn setup<R: Runtime>(app: &AppHandle<R>) {
    if MainThreadMarker::new().is_none() {
        tracing::warn!("tray: not on the main thread; no menu-bar item");
        return;
    }
    let tray = match build(app) {
        Ok(tray) => Arc::new(tray),
        Err(e) => {
            tracing::warn!("tray: the menu-bar item could not be created: {e}");
            return;
        }
    };
    let on_menu = tray.clone();
    tray.icon
        .on_menu_event(move |_, event: MenuEvent| clicked(&on_menu, event.id().as_ref()));
    let on_change = tray.clone();
    tray.recorder.on_change(move || render(&on_change));
    tick(tray.clone());
    tray.apply();
}

fn build<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<Tray<R>> {
    let item = |id: &str, text: &str, enabled: bool| {
        MenuItem::with_id(app, id, text, enabled, None::<&str>)
    };
    let notice = item("notice", "", false)?;
    let start = item("start", "Start recording", true)?;
    let ids = menu_ids(crate::continue_enabled());
    let continue_last = if ids.contains(&"continue") {
        Some(item("continue", "Continue last recording", false)?)
    } else {
        None
    };
    let pause = item("pause", "Pause", false)?;
    let stop = item("stop", "Stop", false)?;
    let open = item("open", "Open Meeting", true)?;
    let quit = item("quit", "Quit", true)?;
    let separator = PredefinedMenuItem::separator(app)?;
    let items: Vec<&dyn IsMenuItem<R>> = ids
        .iter()
        .filter_map(|&id| -> Option<&dyn IsMenuItem<R>> {
            match id {
                "start" => Some(&start),
                "continue" => continue_last.as_ref().map(|i| i as &dyn IsMenuItem<R>),
                "pause" => Some(&pause),
                "stop" => Some(&stop),
                SEPARATOR => Some(&separator),
                "open" => Some(&open),
                "quit" => Some(&quit),
                _ => None,
            }
        })
        .collect();
    let menu = Menu::with_items(app, &items)?;
    let icon = TrayIconBuilder::with_id("main")
        .icon(Image::from_bytes(Icon::Idle.png())?)
        .icon_as_template(Icon::Idle.template())
        .tooltip("Meeting")
        .menu(&menu)
        .show_menu_on_left_click(true)
        .build(app)?;
    Ok(Tray {
        app: app.clone(),
        recorder: app.state::<App>().recorder.clone(),
        icon,
        menu,
        notice,
        start,
        continue_last,
        continue_shown: AtomicBool::new(false),
        pause,
        stop,
    })
}

fn clicked<R: Runtime>(tray: &Arc<Tray<R>>, id: &str) {
    let recorder = &tray.recorder;
    match id {
        "start" => {
            let recorder = recorder.clone();
            // Off the main thread: Start may wait on a permission prompt. A refusal becomes the
            // recorder's start error, and its announcement renders the menu.
            thread::spawn(move || {
                let _ = recorder.start_ad_hoc(None);
            });
        }
        "continue" => {
            let recorder = recorder.clone();
            // Off the main thread, as Start: a refusal becomes the start error.
            thread::spawn(move || {
                let _ = recorder.continue_ad_hoc();
            });
        }
        "pause" => {
            // A refusal means the state already moved on; the render shows where it went.
            let _ = if recorder.recording_elapsed_s().is_some() {
                recorder.pause()
            } else {
                recorder.resume()
            };
        }
        "stop" => {
            // The saving thread finishes on its own.
            let _saving = recorder.stop(Reason::User);
        }
        "open" => crate::focus_main(&tray.app),
        // Emits `ExitRequested`, so `recorder::on_exit_requested` decides, as for any quit.
        "quit" => tray.app.exit(0),
        _ => {}
    }
}

impl<R: Runtime> Tray<R> {
    /// Shows the current status. Main thread only.
    fn apply(&self) {
        let status = self.recorder.status();
        let error = status.start_error.as_ref().map(|e| e.message.as_str());
        let state = menu_state(&status, error);
        let updated = self
            .icon
            .set_icon(Image::from_bytes(state.icon.png()).ok())
            .and_then(|()| self.icon.set_icon_as_template(state.icon.template()))
            .and_then(|()| self.icon.set_title(state.title.as_deref()))
            .and_then(|()| self.start.set_enabled(state.start_enabled))
            .and_then(|()| match &self.continue_last {
                Some(item) => {
                    self.continue_shown
                        .store(state.continue_enabled, Ordering::Relaxed);
                    item.set_enabled(state.continue_enabled)
                }
                None => Ok(()),
            })
            .and_then(|()| self.pause.set_text(state.pause_label))
            .and_then(|()| self.pause.set_enabled(state.pause_enabled))
            .and_then(|()| self.stop.set_enabled(state.stop_enabled))
            .and_then(|()| self.show_notice(state.notice.as_deref()));
        if let Err(e) = updated {
            tracing::warn!("tray: the menu-bar item was not updated: {e}");
        }
    }

    fn show_notice(&self, notice: Option<&str>) -> tauri::Result<()> {
        // Removing an item that is not there fails harmlessly.
        let _ = self.menu.remove(&self.notice);
        match notice {
            Some(text) => {
                self.notice.set_text(text)?;
                self.menu.prepend(&self.notice)
            }
            None => Ok(()),
        }
    }
}

/// Queues `apply` on the main thread; runs it at once when already there.
fn render<R: Runtime>(tray: &Arc<Tray<R>>) {
    let queued = tray.clone();
    if tray.app.run_on_main_thread(move || queued.apply()).is_err() {
        tracing::warn!("tray: the update could not be queued");
    }
}

/// How often an idle menu re-checks whether the last recording can still be continued.
const CONTINUE_CHECK_EVERY_S: u64 = 30;

/// Refreshes the title each second while recording. While idle, re-checks the Continue item every
/// `CONTINUE_CHECK_EVERY_S`: it stops being offered with no recorder change (the 2 hours pass, the
/// folder is moved).
fn tick<R: Runtime>(tray: Arc<Tray<R>>) {
    let spawned = thread::Builder::new()
        .name("tray-timer".into())
        .spawn(move || {
            let mut second: u64 = 0;
            loop {
                thread::sleep(Duration::from_secs(1));
                second = second.wrapping_add(1);
                if tray.continue_last.is_some() && second.is_multiple_of(CONTINUE_CHECK_EVERY_S) {
                    let shown = tray.continue_shown.load(Ordering::Relaxed);
                    if continue_stale(&tray.recorder.status(), shown) {
                        render(&tray);
                    }
                }
                if tray.recorder.recording_elapsed_s().is_some() {
                    let queued = tray.clone();
                    let _ = tray.app.run_on_main_thread(move || {
                        // Read again here: a Stop or Pause handled first must keep its title.
                        if let Some(secs) = queued.recorder.recording_elapsed_s() {
                            let _ = queued.icon.set_title(Some(clock(secs)));
                        }
                    });
                }
            }
        });
    if spawned.is_err() {
        tracing::warn!("tray: the timer could not start");
    }
}

#[cfg(test)]
mod tests {
    use meeting::Language;

    use super::*;
    use crate::capture::Denied;
    use crate::recorder::Continuable;
    use crate::recorder::notice::{self, Notice};

    fn status(state: &'static str, elapsed_s: u64, notice: Option<Notice>) -> Status {
        Status {
            state,
            elapsed_s,
            language: Language::Bn,
            folder: None,
            notice,
            start_error: None,
            continuable: None,
            part: None,
        }
    }

    #[test]
    fn idle_offers_only_start() {
        assert_eq!(
            menu_state(&status("idle", 0, None), None),
            MenuState {
                title: None,
                icon: Icon::Idle,
                start_enabled: true,
                continue_enabled: false,
                pause_label: "Pause",
                pause_enabled: false,
                stop_enabled: false,
                notice: None,
            }
        );
    }

    #[test]
    fn recording_shows_the_timer_and_pause_and_stop() {
        assert_eq!(
            menu_state(&status("recording", 3_725, None), None),
            MenuState {
                title: Some("1:02:05".into()),
                icon: Icon::Recording,
                start_enabled: false,
                continue_enabled: false,
                pause_label: "Pause",
                pause_enabled: true,
                stop_enabled: true,
                notice: None,
            }
        );
    }

    #[test]
    fn paused_shows_resume_and_the_frozen_timer() {
        let state = menu_state(&status("paused", 59, None), None);
        assert_eq!(
            (state.icon, state.title.as_deref(), state.pause_label),
            (Icon::Paused, Some("0:00:59"), "Resume")
        );
        assert!(state.pause_enabled && state.stop_enabled && !state.start_enabled);
    }

    #[test]
    fn saving_disables_all_three() {
        let state = menu_state(&status("saving", 12, None), None);
        assert_eq!(state.icon, Icon::Idle);
        assert_eq!(state.title, None);
        assert!(!state.start_enabled && !state.pause_enabled && !state.stop_enabled);
    }

    #[test]
    fn a_permission_error_is_the_first_line_word_for_word() {
        for denied in [Denied::Microphone, Denied::SystemAudio] {
            let message = denied.to_string();
            let state = menu_state(&status("idle", 0, None), Some(&message));
            assert_eq!(state.notice.as_deref(), Some(message.as_str()));
        }
        assert_eq!(
            Denied::Microphone.to_string(),
            "Allow Meeting in System Settings › Privacy & Security › Microphone"
        );
    }

    #[test]
    fn only_error_and_setup_notices_are_shown() {
        let shown = [
            Notice::retry("ffmpeg failed: boom", std::path::Path::new("/r/m")),
            Notice::new(Kind::Setup, notice::SETUP),
        ];
        for n in shown {
            let message = n.message.clone();
            let state = menu_state(&status("idle", 0, Some(n)), None);
            assert_eq!(state.notice, Some(message));
        }
        for kind in [Kind::Saved, Kind::TooShort, Kind::Recovered] {
            let state = menu_state(&status("idle", 0, Some(Notice::new(kind, "x"))), None);
            assert_eq!(state.notice, None, "{kind:?}");
        }
        // A start error is newer than the notice.
        let state = menu_state(
            &status("idle", 0, Some(Notice::new(Kind::Setup, notice::SETUP))),
            Some("Could not start"),
        );
        assert_eq!(state.notice.as_deref(), Some("Could not start"));
    }

    #[test]
    fn a_dismissed_notice_leaves_no_first_line() {
        for n in [
            Notice::retry("ffmpeg failed: boom", std::path::Path::new("/r/m")),
            Notice::new(Kind::Setup, notice::SETUP),
        ] {
            assert!(
                menu_state(&status("idle", 0, Some(n)), None)
                    .notice
                    .is_some()
            );
            // After `recorder_dismiss_notice` the status has no notice.
            assert_eq!(menu_state(&status("idle", 0, None), None).notice, None);
        }
        // A start error is cleared by its own command, not by dismissing the notice.
        let state = menu_state(&status("idle", 0, None), Some("Could not start"));
        assert_eq!(state.notice.as_deref(), Some("Could not start"));
    }

    fn continuable() -> Continuable {
        Continuable {
            folder: "/r/meeting-2026-11-20_0400".into(),
            name: "meeting-2026-11-20_0400".into(),
            recorded_s: 20,
            stopped_at: 0,
        }
    }

    #[test]
    fn continue_is_offered_only_when_idle_with_something_to_continue() {
        let mut idle = status("idle", 0, None);
        assert!(!menu_state(&idle, None).continue_enabled);
        idle.continuable = Some(continuable());
        let state = menu_state(&idle, None);
        assert!(state.continue_enabled && state.start_enabled);
        for busy in ["recording", "paused", "saving"] {
            let mut s = status(busy, 12, None);
            // The recorder offers nothing while busy; the menu must not rely on that.
            s.continuable = Some(continuable());
            assert!(!menu_state(&s, None).continue_enabled, "{busy}");
        }
        // A refused continue is the menu's first line, as a refused start.
        let state = menu_state(&status("idle", 0, None), Some("Nothing to continue: …"));
        assert_eq!(state.notice.as_deref(), Some("Nothing to continue: …"));
    }

    #[test]
    fn an_offer_that_ran_out_makes_the_continue_item_stale() {
        let mut offered = status("idle", 0, None);
        offered.continuable = Some(continuable());
        let gone = status("idle", 0, None);
        assert!(continue_stale(&gone, true));
        assert!(continue_stale(&offered, false));
        assert!(!continue_stale(&offered, true));
        assert!(!continue_stale(&gone, false));
        assert!(!continue_stale(&status("recording", 5, None), false));
    }

    #[test]
    fn the_continue_item_sits_under_start_only_with_the_flag() {
        assert_eq!(
            menu_ids(true),
            [
                "start", "continue", "pause", "stop", SEPARATOR, "open", "quit"
            ]
        );
        assert_eq!(
            menu_ids(false),
            ["start", "pause", "stop", SEPARATOR, "open", "quit"]
        );
    }

    #[test]
    fn the_clock_is_hours_minutes_seconds() {
        assert_eq!(clock(0), "0:00:00");
        assert_eq!(clock(3_725), "1:02:05");
        assert_eq!(clock(36_000), "10:00:00");
    }

    #[test]
    fn the_icons_are_44_pixel_pngs() {
        for icon in [Icon::Idle, Icon::Recording, Icon::Paused] {
            let image = Image::from_bytes(icon.png()).unwrap();
            assert_eq!((image.width(), image.height()), (44, 44), "{icon:?}");
        }
        assert!(Icon::Idle.template() && Icon::Paused.template());
        assert!(!Icon::Recording.template());
    }
}
