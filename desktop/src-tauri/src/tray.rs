//! The menu-bar item (PRD desktop-app FR-3, D15): the recording state and its controls, with the
//! window closed. It follows every recorder change (`Recorder::on_change`), from the window too.
//! Tray and menu calls run on the main thread; other threads queue them there and never wait,
//! so a main thread blocked in `quit` cannot deadlock a render.

use std::sync::{Arc, Mutex, PoisonError};
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

/// A start error stays until a recording is running, from the menu or the window.
fn kept_error(error: Option<String>, status: &Status) -> Option<String> {
    error.filter(|_| !matches!(status.state, "recording" | "paused"))
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
    pause: MenuItem<R>,
    stop: MenuItem<R>,
    start_error: Mutex<Option<String>>,
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
    let pause = item("pause", "Pause", false)?;
    let stop = item("stop", "Stop", false)?;
    let open = item("open", "Open Meeting", true)?;
    let quit = item("quit", "Quit", true)?;
    let separator = PredefinedMenuItem::separator(app)?;
    let items: [&dyn IsMenuItem<R>; 6] = [&start, &pause, &stop, &separator, &open, &quit];
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
        pause,
        stop,
        start_error: Mutex::new(None),
    })
}

fn clicked<R: Runtime>(tray: &Arc<Tray<R>>, id: &str) {
    let recorder = &tray.recorder;
    match id {
        "start" => {
            let tray = tray.clone();
            // Off the main thread: Start may wait on a permission prompt.
            thread::spawn(move || start_ad_hoc(&tray));
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

/// An ad hoc recording in the last language, as the window's Start without an event.
fn start_ad_hoc<R: Runtime>(tray: &Arc<Tray<R>>) {
    let language = tray.recorder.status().language;
    let error = tray.recorder.start(language, || Ok(None)).err();
    *tray
        .start_error
        .lock()
        .unwrap_or_else(PoisonError::into_inner) = error.map(|r| r.message);
    render(tray);
}

impl<R: Runtime> Tray<R> {
    /// Shows the current status. Main thread only.
    fn apply(&self) {
        let status = self.recorder.status();
        let error = {
            let mut slot = self
                .start_error
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            *slot = kept_error(slot.take(), &status);
            slot.clone()
        };
        let state = menu_state(&status, error.as_deref());
        let updated = self
            .icon
            .set_icon(Image::from_bytes(state.icon.png()).ok())
            .and_then(|()| self.icon.set_icon_as_template(state.icon.template()))
            .and_then(|()| self.icon.set_title(state.title.as_deref()))
            .and_then(|()| self.start.set_enabled(state.start_enabled))
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

/// Refreshes the title each second while recording; nothing else changes on its own.
fn tick<R: Runtime>(tray: Arc<Tray<R>>) {
    let spawned = thread::Builder::new()
        .name("tray-timer".into())
        .spawn(move || {
            loop {
                thread::sleep(Duration::from_secs(1));
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
    use crate::recorder::notice::{self, Notice};

    fn status(state: &'static str, elapsed_s: u64, notice: Option<Notice>) -> Status {
        Status {
            state,
            elapsed_s,
            language: Language::Bn,
            folder: None,
            notice,
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
    fn a_start_error_clears_once_a_recording_runs() {
        let error = || Some("A recording is already running; stop it first".to_string());
        assert_eq!(kept_error(error(), &status("idle", 0, None)), error());
        assert_eq!(kept_error(error(), &status("saving", 0, None)), error());
        assert_eq!(kept_error(error(), &status("recording", 1, None)), None);
        assert_eq!(kept_error(error(), &status("paused", 1, None)), None);
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
