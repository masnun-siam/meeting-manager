//! The Mac going to sleep stops the recording (FR-5), and quitting while recording asks first.
//! Both are AppKit (D32): `NSWorkspaceWillSleepNotification` and an `NSAlert`.

use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, Ordering};

use block2::RcBlock;
use objc2::MainThreadMarker;
use objc2_app_kit::{
    NSAlert, NSAlertFirstButtonReturn, NSApplication, NSWorkspace, NSWorkspaceWillSleepNotification,
};
use objc2_foundation::{NSNotification, NSString};

use super::{Reason, Recorder};

/// Registers `recorder` for the will-sleep notification, once per process.
pub(super) fn register(recorder: Recorder) {
    static REGISTERED: AtomicBool = AtomicBool::new(false);
    if REGISTERED.swap(true, Ordering::SeqCst) {
        return;
    }
    let block = RcBlock::new(move |_: NonNull<NSNotification>| will_sleep(&recorder));
    let center = NSWorkspace::sharedWorkspace().notificationCenter();
    // SAFETY: the name is AppKit's constant, there is no sender filter, and with no queue the
    // block runs on the posting thread with a valid notification it does not keep.
    let observer = unsafe {
        center.addObserverForName_object_queue_usingBlock(
            Some(NSWorkspaceWillSleepNotification),
            None,
            None,
            &block,
        )
    };
    // Observed for the app's whole life, so the token is never handed back to remove it.
    std::mem::forget(observer);
}

/// The notification handler: the system waits for it before sleeping, so it does only the quick
/// part of Stop (the stream ends and the WAV header is patched before `stop` returns). The
/// conversion and the spawn finish on their own thread, after wake if need be. Idle: no-op.
fn will_sleep(recorder: &Recorder) {
    // The saving thread is detached, not joined.
    let _saving = recorder.stop(Reason::Sleep);
}

/// "Quit Meeting?" as an app-modal alert. Off the main thread AppKit cannot show it, so the
/// answer is no: quitting never loses a recording without asking.
pub(super) fn confirm_quit_alert() -> bool {
    let Some(mtm) = MainThreadMarker::new() else {
        tracing::warn!("recorder: the quit confirmation needs the main thread; not quitting");
        return false;
    };
    // A menu-bar app may not be frontmost; the alert must be seen.
    NSApplication::sharedApplication(mtm).activate();
    let alert = NSAlert::new(mtm);
    alert.setMessageText(&NSString::from_str("Quit Meeting?"));
    alert.setInformativeText(&NSString::from_str(
        "The recording will be stopped and saved.",
    ));
    alert.addButtonWithTitle(&NSString::from_str("Stop recording and quit"));
    alert.addButtonWithTitle(&NSString::from_str("Cancel"));
    alert.runModal() == NSAlertFirstButtonReturn
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    use super::*;
    use crate::recorder::handoff::{M4A, WAV};
    use crate::recorder::tests::{SECOND, ok_capture, push, rig, write_config};

    const PREFIX: &str = "Stopped because the Mac went to sleep. ";

    #[test]
    fn sleep_patches_the_header_before_returning_and_saves_after() {
        let rig = rig("sleep", ok_capture());
        // A slow ffmpeg keeps the WAV in place long enough to read it.
        let slow = rig.home.join("ffmpeg-slow");
        fs::write(&slow, "#!/bin/sh\nsleep 1\necho asleep >&2\nexit 1\n").unwrap();
        fs::set_permissions(&slow, fs::Permissions::from_mode(0o755)).unwrap();
        write_config(&rig.env(), slow);
        let folder = rig.start().unwrap();
        push(2 * SECOND);
        will_sleep(&rig.recorder);
        assert_eq!(rig.state(), "saving");
        let bytes = fs::read(folder.join(WAV)).unwrap();
        let data = u32::from_le_bytes(bytes[40..44].try_into().unwrap());
        assert_eq!(data as usize, 2 * 2 * SECOND);
        let notice = rig.wait_idle().notice.unwrap();
        assert_eq!(notice.message, format!("{PREFIX}ffmpeg failed: asleep"));
    }

    #[test]
    fn sleep_stops_a_paused_recording_and_ignores_idle() {
        let rig = rig("sleep-paused", ok_capture());
        will_sleep(&rig.recorder);
        assert_eq!(rig.state(), "idle");
        assert_eq!(rig.recorder.status().notice, None);
        let folder = rig.start().unwrap();
        push(SECOND);
        rig.recorder.pause().unwrap();
        will_sleep(&rig.recorder);
        let notice = rig.wait_idle().notice.unwrap();
        assert!(notice.message.starts_with(PREFIX), "{}", notice.message);
        assert!(folder.join(M4A).is_file());
    }

    #[test]
    fn the_alert_is_not_shown_off_the_main_thread() {
        // Test threads are not the main thread: the answer is to keep running.
        assert!(!confirm_quit_alert());
    }
}
