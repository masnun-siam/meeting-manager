//! `meeting-manager://` links (FR-1 to FR-4, FR-12): the launch hook that decides whether the
//! main window shows, and the handler for links macOS opens. #235 fills both; until then a
//! launch shows the main window as before and an opened link does nothing. Link text is never
//! logged (FR-12).

use tauri::{AppHandle, Runtime, Url};

/// Called once at startup, after the tray: shows and focuses the main window, which `launch`
/// builds hidden.
pub fn on_launch<R: Runtime>(app: &AppHandle<R>) {
    crate::focus_main(app);
}

/// Called for macOS's opened-URLs run event. Does nothing until #235.
pub fn on_opened<R: Runtime>(_app: &AppHandle<R>, _urls: Vec<Url>) {}
