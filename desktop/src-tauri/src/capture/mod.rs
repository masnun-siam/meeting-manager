//! The audio capture boundary: the recorder talks to this struct of fn pointers, so its tests
//! swap in a fake and never touch Core Audio.

mod devices;
mod graph;
mod mix;
mod permission;
mod rebuild;

use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError, mpsc};
use std::thread;
use std::time::Instant;

use anyhow::Context as _;

use graph::Graph;
use rebuild::{Msg, Policy, Worker};

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
        check: permission::check,
        request: permission::request,
        start,
    }
}

fn start(samples: Samples, events: Events) -> anyhow::Result<Stream> {
    start_with(permission::check, open, samples, events)
}

/// Checks the microphone before opening anything, so a denial creates no stream and no thread.
/// A System Audio denial surfaces from `open` as a failed tap (D36). Either error downcasts to
/// `Denied`.
fn start_with(
    check: fn() -> Result<(), Denied>,
    open: fn(Samples, Events) -> anyhow::Result<Stream>,
    samples: Samples,
    events: Events,
) -> anyhow::Result<Stream> {
    check()?;
    open(samples, events)
}

/// Builds the graph, then the worker that rebuilds it on device changes, then the listeners that
/// tell the worker. `Stream::stop` undoes them in reverse: the worker halts the graph, then the
/// listeners are removed.
fn open(samples: Samples, events: Events) -> anyhow::Result<Stream> {
    let samples = Arc::new(Mutex::new(samples));
    let broken = Arc::new(AtomicBool::new(false));
    let graph = Arc::new(Mutex::new(Some(Graph::build(
        samples.clone(),
        broken.clone(),
    )?)));
    let worker = Worker {
        policy: Policy::PRD,
        build: Box::new({
            let graph = graph.clone();
            let broken = broken.clone();
            move || {
                let mut current = graph.lock().unwrap_or_else(PoisonError::into_inner);
                // The old graph goes first: two IOProcs never feed `samples` at once.
                current.take();
                *current = Some(Graph::build(samples.clone(), broken.clone())?);
                Ok(())
            }
        }),
        halt: Box::new(move || {
            graph.lock().unwrap_or_else(PoisonError::into_inner).take();
        }),
        health: Box::new(move || {
            if permission::microphone_revoked() {
                Some("Microphone permission was revoked".into())
            } else if broken.load(Ordering::Relaxed) {
                Some("Audio capture stopped: the audio devices changed their stream layout; start recording again".into())
            } else {
                None
            }
        }),
        events,
        now: Instant::now,
    };
    let (tx, rx) = mpsc::channel();
    // If the thread cannot start, the worker is dropped with it and so is the graph.
    let worker = thread::Builder::new()
        .name("capture-rebuild".into())
        .spawn(move || worker.run(rx))
        .context("Could not start the audio capture thread; quit other apps and try again")?;
    let listeners = match devices::listen(tx.clone()) {
        Ok(listeners) => listeners,
        Err(e) => {
            let _ = tx.send(Msg::Stop);
            let _ = worker.join();
            return Err(e);
        }
    };
    Ok(Stream::new(Box::new(move || {
        let _ = tx.send(Msg::Stop);
        let _ = worker.join();
        drop(listeners);
    })))
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

    fn denied() -> Result<(), Denied> {
        Err(Denied::Microphone)
    }

    fn granted() -> Result<(), Denied> {
        Ok(())
    }

    static OPENED: AtomicUsize = AtomicUsize::new(0);

    fn fake_open(_samples: Samples, _events: Events) -> anyhow::Result<Stream> {
        OPENED.fetch_add(1, Ordering::SeqCst);
        Ok(Stream::new(Box::new(|| {})))
    }

    fn denied_audio(_samples: Samples, _events: Events) -> anyhow::Result<Stream> {
        Err(
            anyhow::anyhow!("Core Audio create process tap failed (OSStatus 560947818)")
                .context(Denied::SystemAudio),
        )
    }

    #[test]
    fn a_denied_microphone_opens_nothing_and_names_the_permission() {
        let before = OPENED.load(Ordering::SeqCst);
        let error = start_with(denied, fake_open, Box::new(|_| {}), Box::new(|_| {}))
            .err()
            .expect("start must fail");
        assert_eq!(OPENED.load(Ordering::SeqCst), before);
        assert_eq!(error.downcast_ref::<Denied>(), Some(&Denied::Microphone));
        assert_eq!(error.to_string(), Denied::Microphone.to_string());
    }

    #[test]
    fn granted_permissions_open_the_stream() {
        let before = OPENED.load(Ordering::SeqCst);
        let stream = start_with(granted, fake_open, Box::new(|_| {}), Box::new(|_| {}));
        assert!(stream.is_ok());
        assert!(OPENED.load(Ordering::SeqCst) > before);
    }

    #[test]
    fn a_failed_tap_downcasts_to_a_system_audio_denial() {
        let error = start_with(granted, denied_audio, Box::new(|_| {}), Box::new(|_| {}))
            .err()
            .expect("start must fail");
        assert_eq!(error.downcast_ref::<Denied>(), Some(&Denied::SystemAudio));
        assert_eq!(error.to_string(), Denied::SystemAudio.to_string());
        assert!(format!("{error:#}").ends_with("(OSStatus 560947818)"));
    }

    #[test]
    fn real_wires_the_permission_functions() {
        let capture = real();
        assert_eq!(capture.check as usize, permission::check as usize);
        assert_eq!(capture.request as usize, permission::request as usize);
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
