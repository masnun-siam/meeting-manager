//! Microphone and System Audio Recording permissions. Nothing is cached, so a permission granted
//! in System Settings counts at the next call without restarting the app.

use std::sync::mpsc;

use block2::RcBlock;
use objc2::runtime::Bool;
use objc2_av_foundation::{AVAuthorizationStatus, AVCaptureDevice, AVMediaTypeAudio};

use super::{Denied, graph};

/// Never prompts. System Audio Recording has no public status API on macOS 14.4, so only the
/// microphone is checked; `request` and `start` find a System Audio denial by creating a tap
/// (D36).
pub fn check() -> Result<(), Denied> {
    match microphone_status() {
        AVAuthorizationStatus::Denied | AVAuthorizationStatus::Restricted => {
            Err(Denied::Microphone)
        }
        _ => Ok(()),
    }
}

/// Asks for the microphone if macOS has not asked yet and waits for the answer (the user may take
/// minutes, so call it off the main thread), then probes System Audio Recording with a throwaway
/// tap, which makes macOS ask on first use. The microphone is checked first; only the first denial
/// is returned.
pub fn request() -> Result<(), Denied> {
    if microphone_status() == AVAuthorizationStatus::NotDetermined && !ask_microphone() {
        return Err(Denied::Microphone);
    }
    check()?;
    graph::probe().map_err(|e| {
        tracing::warn!("system audio probe failed: {e:#}");
        Denied::SystemAudio
    })
}

pub fn microphone_revoked() -> bool {
    check().is_err()
}

fn microphone_status() -> AVAuthorizationStatus {
    // SAFETY: AVMediaTypeAudio is a constant AVFoundation defines; the call has no other inputs.
    unsafe {
        let audio = AVMediaTypeAudio.expect("AVFoundation defines AVMediaTypeAudio");
        AVCaptureDevice::authorizationStatusForMediaType(audio)
    }
}

fn ask_microphone() -> bool {
    let (tx, rx) = mpsc::channel();
    let handler = RcBlock::new(move |granted: Bool| {
        let _ = tx.send(granted.as_bool());
    });
    // SAFETY: AVFoundation copies the block and calls it once on an arbitrary queue.
    unsafe {
        let audio = AVMediaTypeAudio.expect("AVFoundation defines AVMediaTypeAudio");
        AVCaptureDevice::requestAccessForMediaType_completionHandler(audio, &handler);
    }
    rx.recv().unwrap_or(false)
}
