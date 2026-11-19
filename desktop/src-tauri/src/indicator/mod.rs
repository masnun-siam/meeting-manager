//! The floating recording indicator (FR-6 to FR-10, D6 to D9, D17): a small always-on-top
//! `indicator` window the Rust side shows and hides from the recorder's changes, and its
//! settings. The page inside it (`indicator.html`) is #237's.
//!
//! - `decide` turns each recorder change into whether the pill shows, and at which size.
//! - `place` finds where it shows; drags are saved to `indicator_position`, debounced.
//! - `window` builds the window and makes it a non-activating panel through AppKit.

mod place;
mod window;

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::Duration;

use meeting::store::{self, Store};
use objc2::MainThreadMarker;
use serde_json::{Value, json};
use tauri::{
    AppHandle, LogicalPosition, LogicalSize, Manager, PhysicalPosition, Runtime, WindowEvent,
};

use crate::recorder::Status;
use crate::{App, admin, reply};
use place::{Point, Size};
use window::{LABEL, LIVE, OUTCOME};

/// "Show floating indicator while recording": `"false"` is off; absent or anything else is on.
pub const ENABLED_KEY: &str = "indicator_enabled";
/// The pill's remembered top-left at its live size, as JSON `{x, y}` in logical points.
pub const POSITION_KEY: &str = "indicator_position";
/// How long a drag must rest before its position is written.
const SAVE_AFTER: Duration = Duration::from_millis(500);

/// The recorder's state as the pill sees it.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Phase {
    #[default]
    Idle,
    /// Recording or paused: one session.
    Live,
    Saving,
}

/// What the indicator last saw: the phase, and whether the current session (or its outcome)
/// is on screen.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Seen {
    pub phase: Phase,
    pub shown: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Visibility {
    Hidden,
    /// Recording, paused or saving.
    Live,
    /// Saved: the page shows the notice until it calls `indicator_dismiss`.
    Outcome,
}

fn phase(status: &Status) -> Phase {
    match status.state {
        "recording" | "paused" => Phase::Live,
        "saving" => Phase::Saving,
        _ => Phase::Idle,
    }
}

/// Whether `status` starts a session: the only moment the setting is read (FR-10).
pub fn starts(prev: Seen, status: &Status) -> bool {
    phase(status) == Phase::Live && prev.phase != Phase::Live
}

fn visibility(seen: Seen) -> Visibility {
    match (seen.shown, seen.phase) {
        (false, _) => Visibility::Hidden,
        (true, Phase::Idle) => Visibility::Outcome,
        (true, _) => Visibility::Live,
    }
}

/// The pill after a recorder change (FR-6, FR-8). A start shows it when `enabled`, replacing any
/// outcome; the session keeps it through pause and saving whatever the setting does meanwhile;
/// back to idle it stays while there is a notice to show. `saving` straight from idle (a retry,
/// the updater) never shows it.
pub fn decide(prev: Seen, status: &Status, enabled: bool) -> (Seen, Visibility) {
    let now = phase(status);
    let shown = match now {
        Phase::Live if prev.phase != Phase::Live => enabled,
        Phase::Live => prev.shown,
        Phase::Saving => prev.phase != Phase::Idle && prev.shown,
        Phase::Idle => prev.shown && status.notice.is_some(),
    };
    let seen = Seen { phase: now, shown };
    (seen, visibility(seen))
}

/// `indicator_dismiss`: hidden when the recorder is idle, else `None` (a no-op).
pub fn dismissed(prev: Seen, status: &Status) -> Option<Seen> {
    (phase(status) == Phase::Idle).then_some(Seen {
        phase: prev.phase,
        shown: false,
    })
}

fn size_of(visibility: Visibility) -> Option<Size> {
    match visibility {
        Visibility::Hidden => None,
        Visibility::Live => Some(LIVE),
        Visibility::Outcome => Some(OUTCOME),
    }
}

/// The live-size top-left of a pill at `p` with `size`: both sizes share a centre.
fn to_live(p: Point, size: Size) -> Point {
    Point {
        x: p.x + (size.width - LIVE.width) / 2.0,
        y: p.y,
    }
}

fn from_live(p: Point, size: Size) -> Point {
    Point {
        x: p.x - (size.width - LIVE.width) / 2.0,
        y: p.y,
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Managed at launch, whether or not the window could be created.
pub struct Indicator {
    /// Read and written with the recorder's status read under it, so changes apply in order.
    seen: Mutex<Seen>,
    /// What the window shows now; written on the main thread.
    showing: Mutex<Visibility>,
    /// The remembered live-size top-left: the store's at launch, then each drag.
    position: Mutex<Option<Point>>,
    saves: Mutex<Option<Sender<Point>>>,
}

impl Indicator {
    #[cfg(test)]
    pub(crate) fn seen(&self) -> Seen {
        *lock(&self.seen)
    }

    #[cfg(test)]
    pub(crate) fn set_seen(&self, seen: Seen) {
        *lock(&self.seen) = seen;
    }
}

/// Called once at startup, on the main thread, after the tray and the link hook. Recording does
/// not depend on any of it: if the window cannot be created the app runs without the pill.
pub fn on_launch<R: Runtime>(app: &AppHandle<R>) {
    let state = app.state::<App>();
    app.manage(Indicator {
        seen: Mutex::new(Seen::default()),
        showing: Mutex::new(Visibility::Hidden),
        position: Mutex::new(saved_position(&state.env.store_path)),
        saves: Mutex::new(None),
    });
    let window = match window::build(app) {
        Ok(window) => window,
        Err(e) => {
            tracing::error!("indicator: the window could not be created: {e}");
            return;
        }
    };
    if let Some(mtm) = MainThreadMarker::new() {
        window::panel(&window, mtm);
    }
    *lock(&app.state::<Indicator>().saves) = start_saver(state.env.store_path.clone());
    let handle = app.clone();
    window.on_window_event(move |event| {
        if let WindowEvent::Moved(position) = event {
            moved(&handle, *position);
        }
    });
    let handle = app.clone();
    state.recorder.on_change(move || changed(&handle));
}

/// The recorder's listener, on whichever thread changed it.
fn changed<R: Runtime>(app: &AppHandle<R>) {
    let (Some(indicator), Some(state)) = (app.try_state::<Indicator>(), app.try_state::<App>())
    else {
        return;
    };
    let mut seen = lock(&indicator.seen);
    let status = state.recorder.status();
    let enabled = starts(*seen, &status) && is_enabled(&state.env.store_path);
    let before = visibility(*seen);
    let (next, after) = decide(*seen, &status, enabled);
    *seen = next;
    if after != before {
        // Queued under the lock, so two changes reach the main thread in their order.
        queue(app, after);
    }
}

fn queue<R: Runtime>(app: &AppHandle<R>, visibility: Visibility) {
    let handle = app.clone();
    if app
        .run_on_main_thread(move || apply(&handle, visibility))
        .is_err()
    {
        tracing::warn!("indicator: the window change could not be queued");
    }
}

/// On the main thread: hides the window, or sizes, places and shows it without taking focus.
fn apply<R: Runtime>(app: &AppHandle<R>, visibility: Visibility) {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let (Some(window), Some(indicator)) =
        (app.get_webview_window(LABEL), app.try_state::<Indicator>())
    else {
        return;
    };
    let was = std::mem::replace(&mut *lock(&indicator.showing), visibility);
    let Some(size) = size_of(visibility) else {
        let _ = window.hide();
        return;
    };
    let current = || -> Option<Point> {
        let scale = window.scale_factor().ok()?;
        let p = window.outer_position().ok()?.to_logical::<f64>(scale);
        Some(Point { x: p.x, y: p.y })
    };
    let live = match (size_of(was), current()) {
        // Already up (live to outcome, or a new start over an outcome): keep its centre.
        (Some(old), Some(at)) => to_live(at, old),
        _ => {
            let saved = *lock(&indicator.position);
            match window::screens(mtm) {
                Some((screens, menu)) => place::place(saved, &screens, menu, LIVE),
                None => saved.unwrap_or(Point { x: 0.0, y: 0.0 }),
            }
        }
    };
    let at = from_live(live, size);
    let _ = window.set_size(LogicalSize::new(size.width, size.height));
    let _ = window.set_position(LogicalPosition::new(at.x, at.y));
    window::order_front(&window, mtm);
}

/// A move of the window, from a drag or from `apply`: remembered at once, saved after a rest.
fn moved<R: Runtime>(app: &AppHandle<R>, position: PhysicalPosition<i32>) {
    let (Some(window), Some(indicator)) =
        (app.get_webview_window(LABEL), app.try_state::<Indicator>())
    else {
        return;
    };
    // Hidden: not on screen, so not a position the user chose.
    let Some(size) = size_of(*lock(&indicator.showing)) else {
        return;
    };
    let Ok(scale) = window.scale_factor() else {
        return;
    };
    let p = position.to_logical::<f64>(scale);
    let live = to_live(Point { x: p.x, y: p.y }, size);
    let mut remembered = lock(&indicator.position);
    if remembered.is_some_and(|r| (r.x - live.x).abs() < 0.5 && (r.y - live.y).abs() < 0.5) {
        return;
    }
    *remembered = Some(live);
    if let Some(saves) = lock(&indicator.saves).as_ref() {
        let _ = saves.send(live);
    }
}

fn start_saver(store_path: PathBuf) -> Option<Sender<Point>> {
    let (send, receive) = mpsc::channel();
    let spawned = thread::Builder::new()
        .name("indicator-position".into())
        .spawn(move || save_positions(&receive, &store_path, SAVE_AFTER));
    match spawned {
        Ok(_) => Some(send),
        Err(e) => {
            tracing::warn!("indicator: the position saver could not start: {e}");
            None
        }
    }
}

/// Writes the last of each burst of positions once none came for `wait`; ends with the sender.
fn save_positions(positions: &Receiver<Point>, store_path: &Path, wait: Duration) {
    while let Ok(mut last) = positions.recv() {
        while let Ok(next) = positions.recv_timeout(wait) {
            last = next;
        }
        let saved =
            Store::open(store_path).and_then(|s| s.set_setting(POSITION_KEY, &place::format(last)));
        if let Err(e) = saved {
            tracing::warn!(kind = store::error_kind(&e), "indicator position not saved");
        }
    }
}

fn saved_position(store_path: &Path) -> Option<Point> {
    match Store::open(store_path).and_then(|s| s.setting(POSITION_KEY)) {
        Ok(value) => place::parse(value.as_deref()),
        Err(e) => {
            tracing::warn!(kind = store::error_kind(&e), "indicator position not read");
            None
        }
    }
}

/// The setting, or the store's error.
fn read_enabled(store_path: &Path) -> anyhow::Result<bool> {
    let value = Store::open(store_path).and_then(|s| s.setting(ENABLED_KEY))?;
    Ok(value.as_deref() != Some("false"))
}

/// Whether the pill shows for a recording starting now; a setting that can't be read is on.
pub fn is_enabled(store_path: &Path) -> bool {
    read_enabled(store_path).unwrap_or_else(|e| {
        tracing::warn!(kind = store::error_kind(&e), "indicator setting not read");
        true
    })
}

/// `indicator_settings`: `store_unavailable` still carries `enabled: true`, the default.
pub fn settings(store_path: &Path) -> Value {
    match read_enabled(store_path) {
        Ok(enabled) => reply::ok(json!({ "enabled": enabled })),
        Err(e) => {
            tracing::warn!(kind = store::error_kind(&e), "indicator setting not read");
            let mut reply = reply::err("store_unavailable", format!("{e:#}"));
            reply["enabled"] = Value::Bool(true);
            reply
        }
    }
}

/// `set_indicator_enabled`. A pill on screen stays: the setting applies from the next start.
pub fn save_enabled(store_path: &Path, enabled: bool) -> Value {
    let value = if enabled { "true" } else { "false" };
    let saved = Store::open(store_path).and_then(|s| s.set_setting(ENABLED_KEY, value));
    if let Err(e) = saved {
        tracing::error!(kind = store::error_kind(&e), "indicator setting not saved");
        return reply::err("store_unavailable", format!("{e:#}"));
    }
    reply::ok(json!({ "enabled": enabled }))
}

/// Hides the pill unless a session is running; off the main thread (the status reads the store).
fn dismiss<R: Runtime>(app: &AppHandle<R>, state: &App) {
    let Some(indicator) = app.try_state::<Indicator>() else {
        return;
    };
    let mut seen = lock(&indicator.seen);
    if let Some(next) = dismissed(*seen, &state.recorder.status()) {
        let before = visibility(*seen);
        *seen = next;
        if before != Visibility::Hidden {
            queue(app, Visibility::Hidden);
        }
    }
}

#[tauri::command(rename_all = "snake_case")]
pub async fn indicator_settings<R: Runtime>(app: AppHandle<R>) -> Value {
    admin::blocking(app, |state| settings(&state.env.store_path)).await
}

#[tauri::command(rename_all = "snake_case")]
pub async fn set_indicator_enabled<R: Runtime>(app: AppHandle<R>, enabled: bool) -> Value {
    admin::blocking(app, move |state| {
        save_enabled(&state.env.store_path, enabled)
    })
    .await
}

#[tauri::command(rename_all = "snake_case")]
pub async fn indicator_dismiss<R: Runtime>(app: AppHandle<R>) -> Value {
    let handle = app.clone();
    admin::blocking(app, move |state| {
        dismiss(&handle, state);
        reply::ok(json!({}))
    })
    .await
}

/// The pill's time and Open Meeting: the one place it activates the app (FR-7, FR-8).
#[tauri::command(rename_all = "snake_case")]
pub async fn window_open_main<R: Runtime>(app: AppHandle<R>) -> Value {
    let handle = app.clone();
    if app
        .run_on_main_thread(move || crate::focus_main(&handle))
        .is_err()
    {
        tracing::warn!("indicator: the main window could not be shown");
    }
    reply::ok(json!({}))
}

#[cfg(test)]
pub(crate) mod tests {
    use std::fs;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use meeting::Language;

    use super::*;
    use crate::recorder::notice::{Kind, Notice};

    fn status(state: &'static str, notice: Option<Kind>) -> Status {
        Status {
            state,
            elapsed_s: 0,
            language: Language::En,
            folder: None,
            notice: notice.map(|kind| Notice::new(kind, "m")),
            start_error: None,
        }
    }

    /// Feeds `states` through `decide` from a fresh launch, reading the setting only at a start
    /// as the listener does; the visibility after each.
    fn run(states: &[(&'static str, Option<Kind>)], enabled: &[bool]) -> Vec<Visibility> {
        let mut seen = Seen::default();
        let mut setting = enabled.iter().copied();
        states
            .iter()
            .map(|(state, notice)| {
                let s = status(state, *notice);
                let on = starts(seen, &s) && setting.next().unwrap();
                let (next, visibility) = decide(seen, &s, on);
                seen = next;
                visibility
            })
            .collect()
    }

    use Visibility::{Hidden, Live, Outcome};

    #[test]
    fn a_start_with_the_setting_on_shows_it_for_the_whole_session() {
        assert_eq!(
            run(
                &[
                    ("idle", None),
                    ("recording", None),
                    ("paused", None),
                    ("recording", None),
                    ("saving", None),
                    ("idle", Some(Kind::Saved)),
                ],
                &[true]
            ),
            [Hidden, Live, Live, Live, Live, Outcome]
        );
    }

    #[test]
    fn saving_without_a_recording_never_shows_it() {
        // A retry's or the updater's `begin_saving`, then the retry's notice.
        assert_eq!(
            run(&[("saving", None), ("idle", Some(Kind::Error))], &[]),
            [Hidden, Hidden]
        );
        // Also right after an outcome was dismissed.
        let after = Seen {
            phase: Phase::Idle,
            shown: false,
        };
        assert_eq!(decide(after, &status("saving", None), true).1, Hidden);
    }

    #[test]
    fn the_setting_off_at_the_start_hides_it_for_that_session() {
        assert_eq!(
            run(
                &[
                    ("recording", None),
                    ("paused", None),
                    ("saving", None),
                    ("idle", Some(Kind::Saved)),
                ],
                &[false]
            ),
            [Hidden, Hidden, Hidden, Hidden]
        );
    }

    #[test]
    fn the_setting_turned_off_mid_recording_keeps_it_until_dismissed() {
        // `enabled` is read only at the start: a later `false` is never consulted.
        let mut seen = Seen::default();
        for (state, enabled, expected) in [
            ("recording", true, Live),
            ("paused", false, Live),
            ("recording", false, Live),
            ("saving", false, Live),
        ] {
            let s = status(state, None);
            let (next, shown) = decide(seen, &s, enabled && starts(seen, &s));
            assert_eq!(shown, expected, "{state}");
            seen = next;
        }
        let (seen, shown) = decide(seen, &status("idle", Some(Kind::Error)), false);
        assert_eq!(shown, Outcome);
        let seen = dismissed(seen, &status("idle", Some(Kind::Error))).unwrap();
        assert_eq!(visibility(seen), Hidden);
    }

    #[test]
    fn a_new_start_replaces_the_outcome() {
        assert_eq!(
            run(
                &[
                    ("recording", None),
                    ("saving", None),
                    ("idle", Some(Kind::Error)),
                    ("recording", None),
                ],
                &[true, true]
            ),
            [Live, Live, Outcome, Live]
        );
        // A new start with the setting now off hides the old outcome.
        assert_eq!(
            run(
                &[
                    ("recording", None),
                    ("saving", None),
                    ("idle", Some(Kind::Error)),
                    ("recording", None),
                ],
                &[true, false]
            ),
            [Live, Live, Outcome, Hidden]
        );
    }

    #[test]
    fn idle_after_saving_with_no_notice_hides_it() {
        assert_eq!(
            run(
                &[("recording", None), ("saving", None), ("idle", None)],
                &[true]
            ),
            [Live, Live, Hidden]
        );
        // A notice cleared while the outcome shows hides it too.
        assert_eq!(
            run(
                &[
                    ("recording", None),
                    ("saving", None),
                    ("idle", Some(Kind::Saved)),
                    ("idle", None),
                ],
                &[true]
            ),
            [Live, Live, Outcome, Hidden]
        );
    }

    #[test]
    fn a_notice_cleared_while_the_outcome_shows_hides_the_pill() {
        use std::sync::{Arc, Mutex};

        use crate::recorder::Reason;
        use crate::recorder::tests::{SECOND, ok_capture, push, rig};

        let rig = rig("indicator-dismiss-notice", ok_capture());
        let seen = Arc::new(Mutex::new((Seen::default(), Hidden)));
        let (recorder, last) = (rig.recorder.clone(), seen.clone());
        // As the indicator's listener: decide on every announced change.
        rig.recorder.on_change(move || {
            let mut last = last.lock().unwrap();
            *last = decide(last.0, &recorder.status(), true);
        });
        rig.start().unwrap();
        push(11 * SECOND);
        rig.stop(Reason::User);
        assert_eq!(seen.lock().unwrap().1, Outcome);
        rig.recorder.dismiss_notice();
        assert_eq!(seen.lock().unwrap().1, Hidden);
    }

    #[test]
    fn dismiss_is_a_no_op_while_a_session_runs() {
        let live = Seen {
            phase: Phase::Live,
            shown: true,
        };
        for state in ["recording", "paused", "saving"] {
            assert_eq!(dismissed(live, &status(state, None)), None, "{state}");
        }
        let outcome = Seen {
            phase: Phase::Idle,
            shown: true,
        };
        let hidden = dismissed(outcome, &status("idle", Some(Kind::Saved))).unwrap();
        assert_eq!(visibility(hidden), Hidden);
        // And it stays hidden through later notices until a new start.
        assert_eq!(
            decide(hidden, &status("idle", Some(Kind::Saved)), true).1,
            Hidden
        );
    }

    #[test]
    fn the_outcome_shares_the_live_pills_centre() {
        let live = Point { x: 646.0, y: 45.0 };
        let outcome = from_live(live, OUTCOME);
        assert_eq!(outcome, Point { x: 576.0, y: 45.0 });
        assert_eq!(to_live(outcome, OUTCOME), live);
        assert_eq!(from_live(live, LIVE), live);
    }

    pub(crate) fn temp_store(name: &str) -> PathBuf {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "meeting-indicator-{name}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir.join("meeting.db")
    }

    #[test]
    fn the_setting_is_on_unless_it_reads_false() {
        let path = temp_store("setting");
        assert!(is_enabled(&path));
        assert_eq!(settings(&path), json!({"ok": true, "enabled": true}));
        for (value, on) in [("false", false), ("true", true), ("no", true), ("", true)] {
            Store::open(&path)
                .unwrap()
                .set_setting(ENABLED_KEY, value)
                .unwrap();
            assert_eq!(is_enabled(&path), on, "{value:?}");
            assert_eq!(settings(&path)["enabled"], on, "{value:?}");
        }
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn the_setting_is_saved_and_survives_a_reopened_store() {
        let path = temp_store("save");
        assert_eq!(
            save_enabled(&path, false),
            json!({"ok": true, "enabled": false})
        );
        assert_eq!(
            Store::open(&path)
                .unwrap()
                .setting(ENABLED_KEY)
                .unwrap()
                .as_deref(),
            Some("false")
        );
        assert!(!is_enabled(&path));
        assert_eq!(
            save_enabled(&path, true),
            json!({"ok": true, "enabled": true})
        );
        assert!(is_enabled(&path));
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn an_unwritable_store_is_store_unavailable_and_reads_as_on() {
        let path = temp_store("unwritable");
        fs::create_dir_all(&path).unwrap();
        let saved = save_enabled(&path, false);
        assert_eq!(saved["ok"], false);
        assert_eq!(saved["code"], "store_unavailable");
        assert!(is_enabled(&path));
        let read = settings(&path);
        assert_eq!(read["code"], "store_unavailable");
        assert_eq!(read["enabled"], true);
        assert_eq!(saved_position(&path), None);
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn a_burst_of_moves_saves_only_the_last_position() {
        let path = temp_store("position");
        let (send, receive) = mpsc::channel();
        for x in [1.0, 2.0, 3.0] {
            send.send(Point { x, y: 9.0 }).unwrap();
        }
        drop(send);
        save_positions(&receive, &path, Duration::from_millis(10));
        assert_eq!(saved_position(&path), Some(Point { x: 3.0, y: 9.0 }));
        Store::open(&path)
            .unwrap()
            .set_setting(POSITION_KEY, "not json")
            .unwrap();
        assert_eq!(saved_position(&path), None);
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }
}
