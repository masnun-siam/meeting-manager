//! The audio capture boundary: the recorder talks to this struct of fn pointers, so its tests
//! swap in a fake and never touch Core Audio.

use std::fmt;

pub const SAMPLE_RATE: u32 = 48_000;

/// A macOS permission the user has not granted.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Denied {
    Microphone,
    SystemAudio,
}

impl fmt::Display for Denied {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Denied::Microphone => {
                "Allow Meeting in System Settings › Privacy & Security › Microphone"
            }
            Denied::SystemAudio => {
                "Allow Meeting in System Settings › Privacy & Security › Screen & System Audio Recording"
            }
        })
    }
}

impl std::error::Error for Denied {}

#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    /// The default input or output device changed and capture followed it.
    DeviceChanged,
    /// Capture ended: it could not be rebuilt within 5 s, the device is gone, or permission was
    /// revoked.
    Failed(String),
}

/// A running capture. Dropping it without `stop` leaves the capture running.
pub struct Stream {
    stop: Box<dyn FnOnce() + Send>,
}

impl Stream {
    pub fn new(stop: Box<dyn FnOnce() + Send>) -> Stream {
        Stream { stop }
    }

    pub fn stop(self) {
        (self.stop)()
    }
}

/// Receives mono 48 kHz i16 samples, already mixed and clipped.
pub type Samples = Box<dyn FnMut(&[i16]) + Send>;
pub type Events = Box<dyn FnMut(Event) + Send>;

pub struct Capture {
    /// Whether both permissions are granted. Never prompts.
    pub check: fn() -> Result<(), Denied>,
    /// Asks macOS for the permissions. May block until the user answers the prompt, so call it
    /// off the main thread (a sync Tauri command runs on the main thread, an async one does not).
    pub request: fn() -> Result<(), Denied>,
    /// Starts capturing. `samples` and `events` may be called from a real-time or system thread:
    /// they must not block, allocate heavily or log.
    pub start: fn(samples: Samples, events: Events) -> anyhow::Result<Stream>,
}

pub fn real() -> Capture {
    Capture {
        check: || Ok(()),
        request: || Ok(()),
        start: |_samples, _events| Err(anyhow::anyhow!("audio capture is not implemented yet")),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    #[test]
    fn stop_runs_the_closure_once() {
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = calls.clone();
        let stream = Stream::new(Box::new(move || {
            counter.fetch_add(1, Ordering::SeqCst);
        }));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        stream.stop();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn the_stub_grants_permissions_and_refuses_to_start() {
        let capture = real();
        assert_eq!((capture.check)(), Ok(()));
        assert_eq!((capture.request)(), Ok(()));
        let error = (capture.start)(Box::new(|_| {}), Box::new(|_| {}))
            .err()
            .expect("the stub must not start");
        assert_eq!(error.to_string(), "audio capture is not implemented yet");
    }

    #[test]
    fn a_denial_names_its_settings_pane_and_survives_anyhow() {
        assert_eq!(
            Denied::Microphone.to_string(),
            "Allow Meeting in System Settings › Privacy & Security › Microphone"
        );
        assert_eq!(
            Denied::SystemAudio.to_string(),
            "Allow Meeting in System Settings › Privacy & Security › Screen & System Audio Recording"
        );
        let error = anyhow::Error::new(Denied::SystemAudio);
        assert_eq!(error.downcast_ref::<Denied>(), Some(&Denied::SystemAudio));
    }
}
