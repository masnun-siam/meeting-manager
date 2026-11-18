//! The `indicator` window and the AppKit properties Tauri has no option for (FR-9, D8, D17).
//! Every function taking a `MainThreadMarker` runs on the main thread; tests have none and skip
//! them.

use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject};
use objc2::{ClassType, MainThreadMarker, MainThreadOnly, define_class};
use objc2_app_kit::{
    NSPanel, NSResponder, NSScreen, NSStatusWindowLevel, NSWindow, NSWindowCollectionBehavior,
    NSWindowStyleMask,
};
use objc2_foundation::NSObject;
use tauri::{AppHandle, Runtime, WebviewUrl, WebviewWindow, WebviewWindowBuilder};

use super::place::{Rect, Size};

pub const LABEL: &str = "indicator";
pub const PAGE: &str = "indicator.html";

/// While recording, paused or saving.
pub const LIVE: Size = Size {
    width: 220.0,
    height: 40.0,
};
/// While the outcome shows: wider for its message, centred on the same point.
pub const OUTCOME: Size = Size {
    width: 360.0,
    height: 40.0,
};

/// Hidden, borderless, transparent and never focused; `panel` makes it non-activating.
pub fn build<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<WebviewWindow<R>> {
    WebviewWindowBuilder::new(app, LABEL, WebviewUrl::App(PAGE.into()))
        .title("Meeting recording")
        .visible(false)
        .focused(false)
        .focusable(false)
        .decorations(false)
        .transparent(true)
        .shadow(false)
        .resizable(false)
        .always_on_top(true)
        .visible_on_all_workspaces(true)
        .skip_taskbar(true)
        .accept_first_mouse(true)
        .inner_size(LIVE.width, LIVE.height)
        .build()
}

define_class!(
    /// The class the indicator's `NSWindow` becomes: a panel that is never key or main, so a
    /// click on it never activates Meeting (`NonactivatingPanel` only works on an `NSPanel`).
    /// No ivars: the object keeps the memory and ivars of the window Tauri allocated.
    #[unsafe(super(NSPanel, NSWindow, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "MeetingIndicatorPanel"]
    struct IndicatorPanel;

    impl IndicatorPanel {
        #[unsafe(method(canBecomeKeyWindow))]
        fn can_become_key_window(&self) -> bool {
            false
        }

        #[unsafe(method(canBecomeMainWindow))]
        fn can_become_main_window(&self) -> bool {
            false
        }
    }
);

fn ns_window<R: Runtime>(window: &WebviewWindow<R>, _mtm: MainThreadMarker) -> Option<&NSWindow> {
    let ptr = window.ns_window().ok()?.cast::<NSWindow>();
    // SAFETY: Tauri hands out its live `NSWindow`, which the window keeps alive, and the
    // marker proves this is the main thread.
    unsafe { ptr.as_ref() }
}

/// Turns the window into a non-activating panel on every Space, over full-screen apps, out of
/// Mission Control and the window cycle.
pub fn panel<R: Runtime>(window: &WebviewWindow<R>, mtm: MainThreadMarker) {
    let Some(ns) = ns_window(window, mtm) else {
        tracing::warn!("indicator: no native window to configure");
        return;
    };
    let class: &AnyClass = IndicatorPanel::class();
    // SAFETY: `IndicatorPanel` adds no ivars and only overrides two getters, and an `NSPanel`
    // is an `NSWindow`; this is how a Tauri window becomes a panel without a new crate.
    unsafe {
        objc2::ffi::object_setClass(ns as *const NSWindow as *mut AnyObject, class);
    }
    ns.setStyleMask(ns.styleMask() | NSWindowStyleMask::NonactivatingPanel);
    ns.setCollectionBehavior(
        NSWindowCollectionBehavior::CanJoinAllSpaces
            | NSWindowCollectionBehavior::FullScreenAuxiliary
            | NSWindowCollectionBehavior::Stationary
            | NSWindowCollectionBehavior::IgnoresCycle,
    );
    ns.setLevel(NSStatusWindowLevel);
    ns.setHidesOnDeactivate(false);
}

/// Shows the window without making it key or activating the app (tao's `show` does both).
pub fn order_front<R: Runtime>(window: &WebviewWindow<R>, mtm: MainThreadMarker) {
    if let Some(ns) = ns_window(window, mtm) {
        ns.orderFrontRegardless();
    }
}

/// Every screen's frame, and the visible frame of the one with the menu bar (the first).
pub fn screens(mtm: MainThreadMarker) -> Option<(Vec<Rect>, Rect)> {
    let all: Vec<Retained<NSScreen>> = NSScreen::screens(mtm).to_vec();
    let primary = all.first()?.frame();
    let height = primary.size.height;
    let rect = |r: objc2_foundation::NSRect| {
        Rect::from_cocoa(r.origin.x, r.origin.y, r.size.width, r.size.height, height)
    };
    let frames = all.iter().map(|s| rect(s.frame())).collect();
    Some((frames, rect(all[0].visibleFrame())))
}
