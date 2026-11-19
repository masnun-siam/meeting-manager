//! Recording (PRD desktop-app FR-1 to FR-6): start, pause, resume, stop and the automatic stops
//! (3 hours of recorded time, capture failure, full disk), the WAV written while recording, and
//! the hand-off to `meeting process`. One lock guards the state, held only for short transitions:
//! the Rust analogue of the extension's `serial()`. Sleep and quit stops: `sleep.rs`; crash
//! recovery and `recorder_retry`: `recover.rs`.

mod continuable;
pub mod convert;
pub mod folder;
pub mod handoff;
pub mod notice;
mod recover;
mod sleep;
pub mod wav;

use std::fmt::Display;
use std::fs;
use std::io::ErrorKind;
use std::mem;
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use meeting::Language;
use meeting::context::CalendarEvent;
use objc2_foundation::NSTimeZone;
use serde::Serialize;
use serde_json::{Value, json};
use tauri::{AppHandle, Manager, Runtime};

use crate::App;
use crate::calendar::{self, CalendarError};
use crate::capture::{self, Capture, Denied, Event, SAMPLE_RATE, Stream};
use crate::env::Env;
use crate::recorder::continuable::LastRecording;
use crate::reply;
use continuable::Continuable;
use handoff::{CheckToolFn, SpawnFn, Tools};
use notice::Notice;
use wav::{Chunk, WavWriter};

/// The cap on recorded time. Debug builds use 15 minutes so the cap can be checked by hand.
pub const CAP_S: u64 = if cfg!(debug_assertions) {
    15 * 60
} else {
    3 * 3600
};
const FLUSH_EVERY: Duration = Duration::from_secs(5);
/// About 30 s of the capture's ~10 ms chunks.
const QUEUE_CHUNKS: usize = 3_000;
/// Events starting further ahead than this cannot be recorded (FR-11).
const EVENT_HORIZON_S: i64 = 7 * 86_400;
const DEFAULT_LANGUAGE: Language = Language::Bn;
const LANGUAGE_SETTING: &str = "language";

/// The local time a recording starts at, as `context.json` records it.
#[derive(Clone, Debug, PartialEq)]
pub struct Clock {
    pub now_ms: i64,
    pub utc_offset_min: i32,
    /// The IANA name, or `None` when macOS reports one the tool's `check` would reject.
    pub time_zone: Option<String>,
}

pub fn real_clock() -> Clock {
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX));
    let zone = NSTimeZone::localTimeZone();
    let utc_offset_min = i32::try_from(zone.secondsFromGMT() / 60).unwrap_or(0);
    let name = zone.name().to_string();
    Clock {
        now_ms,
        utc_offset_min,
        time_zone: meeting::context::time_zone_ok(&name).then_some(name),
    }
}

/// Everything the recorder calls out to, so tests swap in fakes.
pub struct Deps {
    pub capture: Capture,
    pub clock: fn() -> Clock,
    pub local_minute: folder::LocalMinute,
    /// Creates the WAV's sink (`wav::open`).
    pub open_wav: fn(&std::path::Path) -> anyhow::Result<Box<dyn wav::Sink>>,
    pub spawn: SpawnFn,
    /// Hands off a continued meeting (`meeting::spawn::continue_detached`).
    pub spawn_continue: SpawnFn,
    pub check_tool: CheckToolFn,
    pub cap_s: u64,
    pub flush_every: Duration,
    /// Asks whether to stop the recording and quit (`sleep::confirm_quit_alert`).
    pub confirm_quit: fn() -> bool,
    /// Built with `FEATURE_CONTINUE_RECORDING=1` (`crate::continue_enabled`).
    pub continue_enabled: bool,
}

/// Why a recording stopped; the notice's message starts with it.
#[derive(Clone, Debug, PartialEq)]
pub enum Reason {
    User,
    Cap,
    CaptureFailed(String),
    DiskFull,
    Sleep,
    Quit,
}

impl Reason {
    fn prefix(&self) -> String {
        match self {
            Reason::Cap => "Stopped at 3 hours. ".into(),
            Reason::CaptureFailed(why) => format!("Recording stopped: {why}. "),
            Reason::Sleep => "Stopped because the Mac went to sleep. ".into(),
            Reason::User | Reason::Quit | Reason::DiskFull => String::new(),
        }
    }

    /// Safe to log: no message text.
    fn kind(&self) -> &'static str {
        match self {
            Reason::User => "user",
            Reason::Cap => "cap",
            Reason::CaptureFailed(_) => "capture failed",
            Reason::DiskFull => "disk full",
            Reason::Sleep => "sleep",
            Reason::Quit => "quit",
        }
    }
}

/// A command's refusal: the reply's `code` and `message`.
#[derive(Debug, PartialEq)]
pub struct Refused {
    pub code: &'static str,
    pub message: String,
}

impl Refused {
    fn new(code: &'static str, message: impl Display) -> Refused {
        Refused {
            code,
            message: message.to_string(),
        }
    }
}

#[derive(Serialize, Debug, PartialEq)]
pub struct Status {
    pub state: &'static str,
    pub elapsed_s: u64,
    pub language: Language,
    pub folder: Option<String>,
    pub notice: Option<Notice>,
    /// Why the last start from a link or the menu bar failed (FR-5), until a Start succeeds or
    /// it is dismissed.
    pub start_error: Option<StartError>,
    /// The last recording, while it can be continued (continue-recording FR-4); `null` unless
    /// idle and built with the flag.
    pub continuable: Option<Continuable>,
    /// The part number while a continued part records (continue-recording FR-8), else `null`.
    pub part: Option<u32>,
}

/// A start that failed with no window to show it in (FR-5), until the user dismisses it.
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct StartError {
    pub code: &'static str,
    pub message: String,
}

/// What the watcher thread of a session waits for.
enum Msg {
    Stop(Reason),
    Ended,
}

/// Shared with the samples callback, which runs on a real-time thread: atomics only.
#[derive(Default)]
struct Shared {
    /// Frames queued to the writer: the recorded time.
    frames: AtomicU64,
    paused: AtomicBool,
    capped: AtomicBool,
    /// Chunks the writer could not take; shared with it so it can log once.
    dropped: Arc<AtomicU64>,
}

struct Session {
    id: u64,
    folder: PathBuf,
    language: Language,
    started: Clock,
    event: Option<CalendarEvent>,
    ffmpeg: PathBuf,
    shared: Arc<Shared>,
    writer: WavWriter,
    stream: Stream,
    ctl: SyncSender<Msg>,
    /// Set when the session continues an earlier meeting.
    part: Option<Part>,
}

/// A continued part of an earlier meeting (continue-recording FR-8, FR-9).
#[derive(Clone)]
struct Part {
    /// The part number: `recording-part-N.partial.wav`.
    n: u32,
    /// The meeting's recorded frames before this part.
    base_frames: u64,
    /// A hard link of the part's WAV directly under the recordings folder, so the audio
    /// survives the meeting folder being moved while the part records (D17).
    link: PathBuf,
}

impl Part {
    fn wav(&self, folder: &Path) -> PathBuf {
        folder.join(handoff::part_wav(self.n))
    }

    /// Removes the part's WAV and its link: a part that never became active keeps nothing.
    fn discard(&self, folder: &Path) {
        let _ = fs::remove_file(self.wav(folder));
        let _ = fs::remove_file(&self.link);
    }
}

enum State {
    Idle,
    /// `start` is checking permissions and opening the session; a second start is refused.
    /// Holds the new meeting folder once `open` has created it, so recovery skips it.
    Starting(Option<PathBuf>),
    Active(Box<Session>),
    Saving {
        folder: Option<PathBuf>,
        frames: u64,
        language: Option<Language>,
    },
}

type OnChange = Arc<dyn Fn() + Send + Sync>;

struct Inner {
    env: Env,
    deps: Deps,
    next_id: AtomicU64,
    state: Mutex<State>,
    notice: Mutex<Option<Notice>>,
    /// Memory only, never stored (FR-5).
    start_error: Mutex<Option<StartError>>,
    on_change: Mutex<Vec<OnChange>>,
    /// Held while recovery or a retry converts a folder, so two never work on the same one.
    converting: Mutex<()>,
}

#[derive(Clone)]
pub struct Recorder {
    inner: Arc<Inner>,
}

/// Holds the recorder in `saving` for work outside a session (#149's retry); `idle` on drop.
pub struct SavingGuard {
    recorder: Recorder,
}

impl Drop for SavingGuard {
    fn drop(&mut self) {
        *self.recorder.state() = State::Idle;
        self.recorder.changed();
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Recorder {
    pub fn new(env: &Env) -> Recorder {
        Recorder::with(
            env,
            Deps {
                capture: capture::real(),
                clock: real_clock,
                local_minute: meeting::import::local_minute,
                open_wav: wav::open,
                spawn: meeting::spawn::process_detached,
                spawn_continue: meeting::spawn::continue_detached,
                check_tool: meeting::spawn::check_tool,
                cap_s: CAP_S,
                flush_every: FLUSH_EVERY,
                confirm_quit: sleep::confirm_quit_alert,
                continue_enabled: crate::continue_enabled(),
            },
        )
    }

    pub fn with(env: &Env, deps: Deps) -> Recorder {
        Recorder {
            inner: Arc::new(Inner {
                env: Env::at(env.home.clone()),
                deps,
                next_id: AtomicU64::new(1),
                state: Mutex::new(State::Idle),
                notice: Mutex::new(None),
                start_error: Mutex::new(None),
                on_change: Mutex::new(Vec::new()),
                converting: Mutex::new(()),
            }),
        }
    }

    /// Adds a listener: `f` runs after every change of state or notice, on whichever thread
    /// made it, after the listeners added before it.
    pub fn on_change(&self, f: impl Fn() + Send + Sync + 'static) {
        lock(&self.inner.on_change).push(Arc::new(f));
    }

    fn changed(&self) {
        // Called outside the lock: a listener may read `status()` or add a listener.
        let listeners = lock(&self.inner.on_change).clone();
        for f in listeners {
            f();
        }
    }

    fn state(&self) -> MutexGuard<'_, State> {
        lock(&self.inner.state)
    }

    pub fn status(&self) -> Status {
        let (state, frames, language, folder, idle, part) = match &*self.state() {
            State::Idle => ("idle", 0, None, None, true, None),
            // A start in progress offers nothing: it is about to replace the last recording.
            State::Starting(_) => ("idle", 0, None, None, false, None),
            State::Active(s) => {
                let paused = s.shared.paused.load(Ordering::Relaxed);
                (
                    if paused { "paused" } else { "recording" },
                    s.shared.frames.load(Ordering::Relaxed),
                    Some(s.language),
                    Some(s.folder.clone()),
                    false,
                    s.part.as_ref().map(|p| p.n),
                )
            }
            State::Saving {
                folder,
                frames,
                language,
            } => ("saving", *frames, *language, folder.clone(), false, None),
        };
        Status {
            state,
            elapsed_s: frames / u64::from(SAMPLE_RATE),
            language: language.unwrap_or_else(|| self.saved_language()),
            folder: folder.map(|f| f.display().to_string()),
            notice: lock(&self.inner.notice).clone(),
            start_error: lock(&self.inner.start_error).clone(),
            continuable: if idle { self.continuable() } else { None },
            part,
        }
    }

    /// The last language started with; `bn` when unset or the store cannot be read.
    fn saved_language(&self) -> Language {
        let saved = self
            .inner
            .env
            .store()
            .ok()
            .and_then(|store| store.setting(LANGUAGE_SETTING).ok().flatten());
        match saved.as_deref() {
            Some("en") => Language::En,
            Some("bn") => Language::Bn,
            _ => DEFAULT_LANGUAGE,
        }
    }

    fn save_language(&self, language: Language) {
        let code = match language {
            Language::Bn => "bn",
            Language::En => "en",
        };
        let saved = self
            .inner
            .env
            .store()
            .and_then(|store| store.set_setting(LANGUAGE_SETTING, code));
        if let Err(e) = saved {
            tracing::warn!(
                "recorder: language not saved: {}",
                meeting::store::error_kind(&e)
            );
        }
    }

    /// Starts recording into a new meeting folder. `event` looks up the chosen calendar event
    /// (`Ok(None)`: none chosen). Every refusal leaves no folder behind and the state `idle`.
    pub fn start(
        &self,
        language: Language,
        event: impl FnOnce() -> Result<Option<CalendarEvent>, Refused>,
    ) -> Result<PathBuf, Refused> {
        self.claim()?;
        let folder = self.activate(self.guarded(|| self.open(language, event)))?;
        tracing::info!("recorder: started");
        Ok(folder)
    }

    /// Records the next part of the last recording into its meeting folder (continue-recording
    /// FR-6, FR-8): Start's checks after `nothing_to_continue`, the recorded time counted from
    /// the meeting's total. Every refusal leaves the meeting folder as it was and the state `idle`.
    pub fn continue_recording(&self) -> Result<PathBuf, Refused> {
        self.claim()?;
        let folder = self.activate(self.guarded(|| self.open_part()))?;
        tracing::info!("recorder: continued part");
        Ok(folder)
    }

    /// `idle` → `starting`, so a second start or continue is refused until this one is done.
    fn claim(&self) -> Result<(), Refused> {
        let mut state = self.state();
        if !matches!(*state, State::Idle) {
            return Err(Refused::new(
                "already_recording",
                "A recording is already running; stop it first",
            ));
        }
        *state = State::Starting(None);
        Ok(())
    }

    /// Runs `open`: a panic in an injected call must not leave the recorder stuck in `Starting`.
    fn guarded(
        &self,
        open: impl FnOnce() -> Result<(Session, Receiver<Msg>), Refused>,
    ) -> Result<(Session, Receiver<Msg>), Refused> {
        panic::catch_unwind(AssertUnwindSafe(open)).unwrap_or_else(|_| {
            Err(Refused::new(
                "capture_failed",
                "Could not start recording; try again",
            ))
        })
    }

    /// `starting` → `recording` with the opened session and its watcher, or back to `idle`.
    fn activate(
        &self,
        opened: Result<(Session, Receiver<Msg>), Refused>,
    ) -> Result<PathBuf, Refused> {
        let mut state = self.state();
        let (session, rx) = match opened {
            Ok(opened) => opened,
            Err(refused) => {
                *state = State::Idle;
                return Err(refused);
            }
        };
        let (id, folder, language) = (session.id, session.folder.clone(), session.language);
        let continued = session.part.is_some();
        // Cleared before the session is visible, so a fast automatic stop's notice survives.
        *lock(&self.inner.notice) = None;
        *lock(&self.inner.start_error) = None;
        // A new recording is the last one from here (continue-recording FR-4); cleared before
        // it is visible too, so a fast automatic stop's `remember_last` is never overwritten. A
        // continued part keeps it: the meeting stays the last recording.
        if !continued {
            self.clear_last();
        }
        *state = State::Active(Box::new(session));
        // Spawned with the lock held, so a stop queued before it ran finds the session active.
        let weak = Arc::downgrade(&self.inner);
        let watcher = thread::Builder::new()
            .name("recorder-watch".into())
            .spawn(move || watch(weak, id, rx));
        if watcher.is_err() {
            let State::Active(session) = mem::replace(&mut *state, State::Idle) else {
                unreachable!("set just above");
            };
            drop(state);
            (*session).abandon();
            return Err(Refused::new(
                "capture_failed",
                "Could not start recording; quit other apps and try again",
            ));
        }
        drop(state);
        self.save_language(language);
        self.changed();
        Ok(folder)
    }

    /// An ad hoc Start from outside the window (the menu bar, a link), in `language`, else the
    /// last used. A refusal other than `already_recording` becomes the start error (FR-5).
    pub fn start_ad_hoc(&self, language: Option<Language>) -> Result<PathBuf, Refused> {
        let language = language.unwrap_or_else(|| self.status().language);
        let started = self.start(language, || Ok(None));
        if let Err(r) = &started
            && r.code != "already_recording"
        {
            tracing::info!("recorder: start refused ({})", r.code);
            {
                // Under the state lock, as `start` clears it: a Start from the window that
                // succeeded in the meantime wins, and the stale refusal is dropped.
                let state = self.state();
                if matches!(*state, State::Active(_)) {
                    return started;
                }
                *lock(&self.inner.start_error) = Some(StartError {
                    code: r.code,
                    message: r.message.clone(),
                });
            }
            self.changed();
        }
        started
    }

    /// Clears the start error; nothing changes when there is none.
    pub fn dismiss_start_error(&self) {
        let dismissed = lock(&self.inner.start_error).take();
        if dismissed.is_some() {
            self.changed();
        }
    }

    /// Clears the notice; nothing changes when there is none. A kept WAV stays for the next
    /// launch's recovery.
    pub fn dismiss_notice(&self) {
        let dismissed = lock(&self.inner.notice).take();
        if dismissed.is_some() {
            self.changed();
        }
    }

    /// The checks in the order the contract gives, then the folder, the WAV, `context.json` and
    /// the capture stream.
    fn open(
        &self,
        language: Language,
        event: impl FnOnce() -> Result<Option<CalendarEvent>, Refused>,
    ) -> Result<(Session, Receiver<Msg>), Refused> {
        let deps = &self.inner.deps;
        let config = self.configured()?;
        let started = (deps.clock)();
        let event = event()?;
        if let Some(event) = &event {
            within_horizon(event, started.now_ms.div_euclid(1000))?;
        }
        permitted(deps)?;
        let now_s = u64::try_from(started.now_ms.div_euclid(1000)).unwrap_or(0);
        let folder = folder::new_folder(&config.recordings_dir, deps.local_minute, now_s)
            .map_err(|e| Refused::new("capture_failed", format!("{e:#}")))?;
        // Before the WAV exists: launch recovery treats any WAV whose folder the state does not
        // name as a leftover (#262).
        *self.state() = State::Starting(Some(folder.clone()));
        let opened = self.open_in(
            folder.clone(),
            None,
            language,
            started,
            event,
            config.ffmpeg_path,
        );
        if opened.is_err() {
            let _ = fs::remove_dir_all(&folder);
        }
        opened
    }

    /// `open` for the next part of the last recording: `nothing_to_continue`, then Start's
    /// checks, then the part's WAV and its link in the existing meeting folder. The meeting's
    /// `recording.m4a` and `context.json` are never touched; a refusal removes only the part's
    /// own files.
    fn open_part(&self) -> Result<(Session, Receiver<Msg>), Refused> {
        let last = self.last_continuable().ok_or_else(|| {
            Refused::new(
                "nothing_to_continue",
                "Nothing to continue: the last recording stopped more than 2 hours ago, or was moved.",
            )
        })?;
        let deps = &self.inner.deps;
        let config = self.configured()?;
        let started = (deps.clock)();
        permitted(deps)?;
        let folder = PathBuf::from(&last.folder);
        let part = next_part(&folder, &config.recordings_dir, &last);
        // Before the part's WAV exists, as for a new folder (#262).
        *self.state() = State::Starting(Some(folder.clone()));
        let opened = self.open_in(
            folder.clone(),
            Some(part.clone()),
            last.language,
            started,
            None,
            config.ffmpeg_path,
        );
        if opened.is_err() {
            part.discard(&folder);
        }
        opened
    }

    /// The configuration, with ffmpeg where it says: `not_configured` otherwise.
    fn configured(&self) -> Result<meeting::config::Config, Refused> {
        let config = self
            .inner
            .env
            .config()
            .map_err(|message| Refused::new("not_configured", message))?;
        if !config.ffmpeg_path.is_file() {
            return Err(Refused::new(
                "not_configured",
                format!(
                    "ffmpeg not found at {}; run `meeting doctor`",
                    config.ffmpeg_path.display()
                ),
            ));
        }
        Ok(config)
    }

    fn open_in(
        &self,
        folder: PathBuf,
        part: Option<Part>,
        language: Language,
        started: Clock,
        event: Option<CalendarEvent>,
        ffmpeg: PathBuf,
    ) -> Result<(Session, Receiver<Msg>), Refused> {
        let deps = &self.inner.deps;
        // A part's recorded time starts at the meeting's, so the cap and `elapsed_s` count it.
        let shared = Arc::new(Shared {
            frames: AtomicU64::new(part.as_ref().map_or(0, |p| p.base_frames)),
            ..Shared::default()
        });
        let (ctl, rx) = mpsc::sync_channel(4);
        let on_error = {
            let ctl = ctl.clone();
            Box::new(move |kind| {
                let _ = ctl.try_send(Msg::Stop(write_failure(kind)));
            })
        };
        let wav = part
            .as_ref()
            .map_or_else(|| folder.join(handoff::WAV), |p| p.wav(&folder));
        let sink =
            (deps.open_wav)(&wav).map_err(|e| Refused::new("capture_failed", format!("{e:#}")))?;
        if let Some(part) = &part {
            fs::hard_link(&wav, &part.link)
                .map_err(|e| Refused::new("capture_failed", format!("cannot write audio: {e}")))?;
        }
        let writer = WavWriter::start(
            sink,
            deps.flush_every,
            QUEUE_CHUNKS,
            shared.dropped.clone(),
            on_error,
        )
        .map_err(|e| Refused::new("capture_failed", format!("cannot write audio: {e}")))?;
        if part.is_none() {
            handoff::write_context(&folder, &started, event.clone());
        }
        let cap_frames = deps.cap_s * u64::from(SAMPLE_RATE);
        let samples = samples_callback(shared.clone(), writer.sender(), cap_frames, ctl.clone());
        let stream = match (deps.capture.start)(samples, events_callback(ctl.clone())) {
            Ok(stream) => stream,
            Err(e) => {
                let _ = writer.finish();
                return Err(match e.downcast_ref::<Denied>() {
                    Some(d) => denied(*d),
                    None => Refused::new("capture_failed", format!("{e:#}")),
                });
            }
        };
        let session = Session {
            id: self.inner.next_id.fetch_add(1, Ordering::Relaxed),
            folder,
            language,
            started,
            event,
            ffmpeg,
            shared,
            writer,
            stream,
            ctl,
            part,
        };
        Ok((session, rx))
    }

    pub fn pause(&self) -> Result<(), Refused> {
        self.set_paused(true)
    }

    pub fn resume(&self) -> Result<(), Refused> {
        self.set_paused(false)
    }

    /// Pause keeps the stream open and drops its samples, so Resume continues the same file.
    fn set_paused(&self, paused: bool) -> Result<(), Refused> {
        {
            let state = self.state();
            match &*state {
                State::Active(s) if s.shared.paused.load(Ordering::Relaxed) != paused => {
                    s.shared.paused.store(paused, Ordering::Relaxed);
                }
                _ => {
                    let what = if paused { "pause" } else { "resume" };
                    return Err(Refused::new(
                        "not_recording",
                        format!("There is no recording to {what}"),
                    ));
                }
            }
        }
        self.changed();
        Ok(())
    }

    /// Stops the running recording, if any: the stream ends and the WAV is finished before this
    /// returns; conversion and the hand-off run on the returned thread, which leaves the state
    /// `idle` with the notice. `None` when nothing was recording (a second stop is a no-op).
    pub fn stop(&self, reason: Reason) -> Option<JoinHandle<()>> {
        self.stop_session(None, reason)
    }

    /// `stop`, only if session `id` (when given) is the one running: a late automatic stop never
    /// ends a newer recording.
    fn stop_session(&self, id: Option<u64>, reason: Reason) -> Option<JoinHandle<()>> {
        let session = {
            let mut state = self.state();
            let ours = matches!(&*state, State::Active(s) if id.is_none_or(|id| s.id == id));
            if !ours {
                return None;
            }
            let State::Active(s) = mem::replace(&mut *state, State::Idle) else {
                unreachable!("checked just above");
            };
            *state = State::Saving {
                folder: Some(s.folder.clone()),
                frames: s.shared.frames.load(Ordering::Relaxed),
                language: Some(s.language),
            };
            s
        };
        tracing::info!("recorder: stopping ({})", reason.kind());
        self.changed();
        let Session {
            folder,
            language,
            started,
            event,
            ffmpeg,
            writer,
            stream,
            ctl,
            part,
            ..
        } = *session;
        stream.stop();
        let _ = ctl.try_send(Msg::Ended);
        let (frames, disk_full) = match writer.finish() {
            Ok(frames) => (frames, reason == Reason::DiskFull),
            Err(kind) => {
                tracing::warn!("recorder: the WAV writer failed: {kind}");
                // A part's link is the same file, and still there if the folder was moved.
                let wav = part
                    .as_ref()
                    .map_or_else(|| folder.join(handoff::WAV), |p| p.link.clone());
                let frames = wav::repair(&wav).unwrap_or(0);
                (
                    frames,
                    kind == ErrorKind::StorageFull || reason == Reason::DiskFull,
                )
            }
        };
        let recorder = self.clone();
        Some(thread::spawn(move || {
            let stopped = Stopped {
                folder,
                language,
                started,
                ffmpeg,
                frames,
                disk_full,
                prefix: reason.prefix(),
            };
            let saved = panic::catch_unwind(AssertUnwindSafe(|| match &part {
                None => recorder.save(&stopped, event),
                Some(part) => recorder.save_part(&stopped, part),
            }));
            // Never stuck in `saving`: the WAV, if still there, can be retried.
            let (notice, last) = saved.unwrap_or_else(|_| {
                let notice = Notice::retry("Saving failed unexpectedly; retry", &stopped.folder);
                // A new recording is the last one whatever the outcome, as Start cleared it; a
                // part leaves the meeting's stored value as it was.
                let last = part.is_none().then(|| Saved {
                    folder: stopped.folder.clone(),
                    recorded_s: stopped.frames / u64::from(SAMPLE_RATE),
                    parts: 1,
                });
                (notice, last)
            });
            // Before `idle`, so a poll never sees `idle` without it.
            if let Some(last) = last {
                recorder.remember_last(&last.folder, last.recorded_s, language, last.parts);
            }
            recorder.finish_saving(notice);
        }))
    }

    /// Saves a new recording into its folder; it is the last recording whatever the outcome.
    fn save(&self, stopped: &Stopped, event: Option<CalendarEvent>) -> (Notice, Option<Saved>) {
        let Stopped { folder, .. } = stopped;
        handoff::ensure_context(folder, &stopped.started, event);
        let notice = if stopped.disk_full {
            Notice::retry(notice::DISK_FULL, folder)
        } else {
            let tools = self.tools(stopped.ffmpeg.clone());
            handoff::finish_recording(
                &tools,
                folder,
                stopped.language,
                stopped.frames,
                &stopped.prefix,
            )
        };
        let saved = Saved {
            folder: folder.clone(),
            recorded_s: stopped.frames / u64::from(SAMPLE_RATE),
            parts: 1,
        };
        (notice, Some(saved))
    }

    /// Saves a continued part into its meeting folder and hands the meeting off with
    /// `--continue` (continue-recording FR-9). The meeting stays the last recording only once
    /// the part is converted; a moved meeting is saved as a new one (D17).
    fn save_part(&self, stopped: &Stopped, part: &Part) -> (Notice, Option<Saved>) {
        let folder = &stopped.folder;
        if !part.wav(folder).is_file() || !folder.join(handoff::M4A).is_file() {
            return self.save_moved(stopped, part);
        }
        let total = part.base_frames + stopped.frames;
        let notice = if stopped.disk_full {
            Notice::retry(notice::DISK_FULL, folder)
        } else {
            let tools = Tools {
                spawn: self.inner.deps.spawn_continue,
                ..self.tools(stopped.ffmpeg.clone())
            };
            handoff::finish_part(
                &tools,
                folder,
                part.n,
                stopped.language,
                total,
                &stopped.prefix,
            )
        };
        // The part's WAV is converted, or kept in the folder for a retry: the link is not needed.
        let _ = fs::remove_file(&part.link);
        let converted = folder.join(handoff::part_m4a(part.n)).is_file();
        let saved = converted.then(|| Saved {
            folder: folder.clone(),
            recorded_s: total / u64::from(SAMPLE_RATE),
            parts: part.n,
        });
        (notice, saved)
    }

    /// The meeting folder was moved, or its `recording.m4a` is gone, while the part recorded:
    /// the part's link becomes a new meeting, saved as a Start's would be (D17). Nothing recorded
    /// is lost; if the new meeting cannot be made, the link stays where it is.
    fn save_moved(&self, stopped: &Stopped, part: &Part) -> (Notice, Option<Saved>) {
        tracing::warn!("recorder: the continued meeting was moved; saving the part as a new one");
        let name = stopped
            .folder
            .file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
        let failed = |e: anyhow::Error| {
            let message = format!(
                "Could not add to {name}: it was moved, and saving it as a new meeting failed: {e:#}"
            );
            (Notice::new(notice::Kind::Error, message), None)
        };
        let Some(recordings) = part.link.parent() else {
            return failed(anyhow::anyhow!("the recordings folder is unknown"));
        };
        let now_s = u64::try_from(stopped.started.now_ms.div_euclid(1000)).unwrap_or(0);
        let new = match folder::new_folder(recordings, self.inner.deps.local_minute, now_s) {
            Ok(new) => new,
            Err(e) => return failed(e),
        };
        // Held from here, as `Saving` held the moved folder, so recovery skips the new one.
        if let State::Saving { folder, .. } = &mut *self.state() {
            *folder = Some(new.clone());
        }
        // The copy left in a folder still at its path (`recording.m4a` deleted) is the same file.
        let left = part.wav(&stopped.folder);
        let same = same_file(&left, &part.link);
        if let Err(e) = fs::rename(&part.link, new.join(handoff::WAV)) {
            let _ = fs::remove_dir(&new);
            return failed(anyhow::Error::from(e).context("cannot move the part's audio"));
        }
        if same {
            let _ = fs::remove_file(&left);
        }
        let (notice, saved) = self.save(
            &Stopped {
                folder: new,
                ..stopped.clone()
            },
            None,
        );
        let moved = Notice::new(
            notice::Kind::Error,
            format!("Could not add to {name}: it was moved; saved as a new meeting."),
        );
        // A hand-off that failed says what to do next; anything else becomes the moved notice.
        let notice = if notice.kind == notice::Kind::Error {
            notice
        } else {
            moved
        };
        (notice, saved)
    }

    fn tools(&self, ffmpeg: PathBuf) -> Tools {
        let deps = &self.inner.deps;
        Tools {
            ffmpeg,
            // Read now, so a `meeting install` during the recording counts.
            meeting_path: self.inner.env.config().ok().and_then(|c| c.meeting_path),
            spawn: deps.spawn,
            check_tool: deps.check_tool,
        }
    }

    fn finish_saving(&self, notice: Notice) {
        *self.state() = State::Idle;
        *lock(&self.inner.notice) = Some(notice);
        self.changed();
    }

    /// The recorded seconds while recording (not paused): what the menu-bar timer shows. Never
    /// reads the store, unlike `status`.
    pub fn recording_elapsed_s(&self) -> Option<u64> {
        match &*self.state() {
            State::Active(s) if !s.shared.paused.load(Ordering::Relaxed) => {
                Some(s.shared.frames.load(Ordering::Relaxed) / u64::from(SAMPLE_RATE))
            }
            _ => None,
        }
    }

    fn is_saving(&self) -> bool {
        matches!(*self.state(), State::Saving { .. })
    }

    fn wait_saved(&self) {
        while self.is_saving() {
            thread::sleep(Duration::from_millis(50));
        }
    }

    /// Whether the app may quit now (FR-5). Idle: yes. Saving: once the save is done. Recording
    /// or paused: only if the user confirms, and then after the recording is saved and handed
    /// off. Blocks while saving; the spawned job runs on in its own process group.
    pub(crate) fn quit(&self) -> bool {
        let recording = matches!(*self.state(), State::Active(_));
        if recording {
            if !(self.inner.deps.confirm_quit)() {
                return false;
            }
            // `None`: an automatic stop got there first while the alert was up.
            if let Some(saving) = self.stop(Reason::Quit) {
                let _ = saving.join();
            }
        }
        self.wait_saved();
        true
    }

    /// `idle` → `saving` until the guard drops; `None` when not idle.
    pub fn begin_saving(&self) -> Option<SavingGuard> {
        {
            let mut state = self.state();
            if !matches!(*state, State::Idle) {
                return None;
            }
            *state = State::Saving {
                folder: None,
                frames: 0,
                language: None,
            };
        }
        self.changed();
        Some(SavingGuard {
            recorder: self.clone(),
        })
    }
}

impl Session {
    /// Undoes a session that never became active: nothing is kept.
    fn abandon(self) {
        self.stream.stop();
        let _ = self.ctl.try_send(Msg::Ended);
        let _ = self.writer.finish();
        match &self.part {
            // The meeting folder holds the earlier parts: only this part's files go.
            Some(part) => part.discard(&self.folder),
            None => {
                let _ = fs::remove_dir_all(&self.folder);
            }
        }
    }
}

/// What a stopped session leaves for its save.
#[derive(Clone)]
struct Stopped {
    folder: PathBuf,
    language: Language,
    started: Clock,
    ffmpeg: PathBuf,
    /// This session's frames (a part's alone).
    frames: u64,
    disk_full: bool,
    prefix: String,
}

/// The last recording a save leaves (continue-recording FR-4): its meeting totals.
struct Saved {
    folder: PathBuf,
    recorded_s: u64,
    parts: u32,
}

/// The first part number from the stored `parts` + 1 (at least 2) whose WAV, m4a and link are
/// all free, so nothing a part leaves behind is ever overwritten or removed.
fn next_part(folder: &Path, recordings: &Path, last: &LastRecording) -> Part {
    let name = folder
        .file_name()
        .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
    let mut n = last.parts.saturating_add(1).max(2);
    loop {
        let link = recordings.join(handoff::part_link(&name, n));
        let taken = [
            folder.join(handoff::part_wav(n)),
            folder.join(handoff::part_m4a(n)),
            link.clone(),
        ]
        .iter()
        .any(|p| p.symlink_metadata().is_ok());
        if !taken {
            return Part {
                n,
                base_frames: last.recorded_s * u64::from(SAMPLE_RATE),
                link,
            };
        }
        n += 1;
    }
}

/// Whether `a` and `b` name the same file (hard links of one inode).
pub(super) fn same_file(a: &Path, b: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (fs::metadata(a), fs::metadata(b)) {
        (Ok(a), Ok(b)) => (a.dev(), a.ino()) == (b.dev(), b.ino()),
        _ => false,
    }
}

/// The capture check, then its request: Start's permission refusals.
fn permitted(deps: &Deps) -> Result<(), Refused> {
    (deps.capture.check)()
        .or_else(|_| (deps.capture.request)())
        .map_err(denied)
}

/// Waits for the session's first automatic stop, or for the session to end.
fn watch(recorder: Weak<Inner>, id: u64, rx: Receiver<Msg>) {
    if let Ok(Msg::Stop(reason)) = rx.recv()
        && let Some(inner) = recorder.upgrade()
    {
        // The saving thread is left to finish on its own.
        let _ = Recorder { inner }.stop_session(Some(id), reason);
    }
}

/// Runs on the capture's real-time thread: no lock, no I/O, no blocking send. A chunk the
/// writer cannot take is dropped and counted; the gap is not filled.
fn samples_callback(
    shared: Arc<Shared>,
    tx: SyncSender<Chunk>,
    cap_frames: u64,
    ctl: SyncSender<Msg>,
) -> capture::Samples {
    Box::new(move |chunk: &[i16]| {
        if shared.paused.load(Ordering::Relaxed) || shared.capped.load(Ordering::Relaxed) {
            return;
        }
        if tx.try_send(Chunk::Samples(chunk.to_vec())).is_err() {
            shared.dropped.fetch_add(1, Ordering::Relaxed);
            return;
        }
        let len = chunk.len() as u64;
        let total = shared.frames.fetch_add(len, Ordering::Relaxed) + len;
        if total >= cap_frames && !shared.capped.swap(true, Ordering::Relaxed) {
            let _ = ctl.try_send(Msg::Stop(Reason::Cap));
        }
    })
}

/// Runs on the capture thread, which `Stream::stop` waits for: it only queues the stop.
fn events_callback(ctl: SyncSender<Msg>) -> capture::Events {
    Box::new(move |event| match event {
        Event::Failed(why) => {
            let _ = ctl.try_send(Msg::Stop(Reason::CaptureFailed(why)));
        }
        // Capture followed the new device; the gap is allowed (FR-5).
        Event::DeviceChanged => {}
    })
}

fn write_failure(kind: ErrorKind) -> Reason {
    if kind == ErrorKind::StorageFull {
        Reason::DiskFull
    } else {
        Reason::CaptureFailed(format!("cannot write audio: {kind}"))
    }
}

fn denied(d: Denied) -> Refused {
    let code = match d {
        Denied::Microphone => "permission_mic",
        Denied::SystemAudio => "permission_audio",
    };
    Refused::new(code, d)
}

/// FR-11: an event starting more than 7 days from now cannot be recorded.
fn within_horizon(event: &CalendarEvent, now_s: i64) -> Result<(), Refused> {
    match calendar::window::unix_of_iso(&event.start) {
        None => Err(Refused::new("unknown_event", UNKNOWN_EVENT)),
        Some(start) if start - now_s > EVENT_HORIZON_S => Err(Refused::new(
            "event_too_far",
            "Events more than 7 days away cannot be recorded yet",
        )),
        Some(_) => Ok(()),
    }
}

const UNKNOWN_EVENT: &str = "That event is no longer in your calendar";

/// The calendar's answer for the chosen event, as `start` takes it.
fn chosen_event(
    found: Result<Option<CalendarEvent>, CalendarError>,
) -> Result<Option<CalendarEvent>, Refused> {
    match found {
        Ok(Some(event)) => Ok(Some(event)),
        Ok(None) => Err(Refused::new("unknown_event", UNKNOWN_EVENT)),
        Err(CalendarError::Denied) => {
            Err(Refused::new("calendar_denied", calendar::DENIED_MESSAGE))
        }
        Err(CalendarError::Failed(why)) => Err(Refused::new("unknown_event", why)),
    }
}

fn refused(r: Refused) -> Value {
    reply::err(r.code, r.message)
}

/// Runs `f` off the main and async threads: permission prompts, joins and ffmpeg block.
async fn blocking<R: Runtime>(
    app: AppHandle<R>,
    f: impl FnOnce(&App) -> Value + Send + 'static,
) -> Value {
    // `launch` manages the state before it opens the window, so no command can run without it.
    let ran = tauri::async_runtime::spawn_blocking(move || f(&app.state::<App>())).await;
    ran.unwrap_or_else(|_| reply::err("failed", "a recorder command failed unexpectedly"))
}

/// Called once at startup: recovers what a crash left, off the main thread (launch stays fast).
pub fn on_launch<R: Runtime>(app: &AppHandle<R>) {
    let recorder = app.state::<App>().recorder.clone();
    let spawned = thread::Builder::new()
        .name("recorder-recover".into())
        .spawn(move || recorder.recover_all());
    if spawned.is_err() {
        tracing::warn!("recorder: recovery not started");
    }
}

/// Called once at startup; registers for the Mac going to sleep.
pub fn on_sleep<R: Runtime>(app: &AppHandle<R>) {
    sleep::register(app.state::<App>().recorder.clone());
}

/// Called when the app is asked to quit. `false` keeps it running.
pub fn on_exit_requested<R: Runtime>(app: &AppHandle<R>) -> bool {
    app.state::<App>().recorder.quit()
}

#[tauri::command(rename_all = "snake_case")]
pub async fn recorder_status<R: Runtime>(app: AppHandle<R>) -> Value {
    blocking(app, |app| {
        let status = app.recorder.status();
        serde_json::to_value(status).map_or_else(
            |_| reply::err("failed", "the status could not be read"),
            reply::ok,
        )
    })
    .await
}

/// Clears the start error (FR-5); already clear is fine.
#[tauri::command(rename_all = "snake_case")]
pub async fn recorder_dismiss_start_error<R: Runtime>(app: AppHandle<R>) -> Value {
    blocking(app, |app| {
        app.recorder.dismiss_start_error();
        reply::ok(json!({}))
    })
    .await
}

/// Clears the recorder's notice (continue-recording FR-1); already clear is fine.
#[tauri::command(rename_all = "snake_case")]
pub async fn recorder_dismiss_notice<R: Runtime>(app: AppHandle<R>) -> Value {
    blocking(app, |app| {
        app.recorder.dismiss_notice();
        reply::ok(json!({}))
    })
    .await
}

#[tauri::command(rename_all = "snake_case")]
pub async fn recorder_start<R: Runtime>(
    app: AppHandle<R>,
    language: Language,
    event_occurrence_id: Option<String>,
) -> Value {
    blocking(app, move |app| {
        let event = || match event_occurrence_id {
            None => Ok(None),
            Some(id) => chosen_event(app.calendar.event_for_context(&id)),
        };
        match app.recorder.start(language, event) {
            Ok(folder) => reply::ok(json!({"folder": folder.display().to_string()})),
            Err(r) => refused(r),
        }
    })
    .await
}

/// Continues the last recording (continue-recording FR-6). Registered only in builds with
/// `FEATURE_CONTINUE_RECORDING=1` (FR-14).
#[tauri::command(rename_all = "snake_case")]
pub async fn recorder_continue<R: Runtime>(app: AppHandle<R>) -> Value {
    blocking(app, |app| match app.recorder.continue_recording() {
        Ok(folder) => reply::ok(json!({"folder": folder.display().to_string()})),
        Err(r) => {
            tracing::info!("recorder: continue refused ({})", r.code);
            refused(r)
        }
    })
    .await
}

#[tauri::command(rename_all = "snake_case")]
pub async fn recorder_pause<R: Runtime>(app: AppHandle<R>) -> Value {
    blocking(app, |app| match app.recorder.pause() {
        Ok(()) => reply::ok(json!({"state": "paused"})),
        Err(r) => refused(r),
    })
    .await
}

#[tauri::command(rename_all = "snake_case")]
pub async fn recorder_resume<R: Runtime>(app: AppHandle<R>) -> Value {
    blocking(app, |app| match app.recorder.resume() {
        Ok(()) => reply::ok(json!({"state": "recording"})),
        Err(r) => refused(r),
    })
    .await
}

#[tauri::command(rename_all = "snake_case")]
pub async fn recorder_stop<R: Runtime>(app: AppHandle<R>) -> Value {
    blocking(app, |app| match app.recorder.stop(Reason::User) {
        Some(_saving) => reply::ok(json!({"state": "saving"})),
        None => reply::err("not_recording", "There is no recording to stop"),
    })
    .await
}

#[tauri::command(rename_all = "snake_case")]
pub async fn recorder_retry<R: Runtime>(app: AppHandle<R>, folder: String) -> Value {
    blocking(app, move |app| match app.recorder.retry(&folder) {
        Ok(_saving) => reply::ok(json!({"state": "saving"})),
        Err(r) => refused(r),
    })
    .await
}

#[cfg(test)]
pub(crate) mod tests {
    use std::cell::{Cell, RefCell};
    use std::os::unix::fs::PermissionsExt;
    use std::sync::atomic::AtomicUsize;
    use std::time::Instant;

    use meeting::config::Config;

    use super::*;
    use crate::capture::{Events, Samples};
    use crate::recorder::continuable::tests::store_last;
    use crate::recorder::convert::tests::{ffmpeg, probe, tone_wav};
    use crate::recorder::handoff::tests::{
        clock, continues_under, event, fake_continue_spawn, fake_spawn, spawns_under, tool,
    };
    use crate::recorder::handoff::{CONTEXT, M4A, WAV};
    use crate::recorder::notice::Kind;
    use crate::recorder::wav::tests::temp;

    pub(crate) const SECOND: usize = 48_000;
    const STAMP: &str = "2026-11-13_0600";

    type Start = fn(Samples, Events) -> anyhow::Result<Stream>;

    thread_local! {
        /// The callbacks the fake capture was started with; `start` runs on the test's thread.
        static CALLBACKS: RefCell<Option<(Samples, Events)>> = const { RefCell::new(None) };
        static GRANTED: Cell<bool> = const { Cell::new(true) };
    }

    fn granted() -> Result<(), Denied> {
        Ok(())
    }

    fn mic_denied() -> Result<(), Denied> {
        Err(Denied::Microphone)
    }

    fn mic_as_set() -> Result<(), Denied> {
        if GRANTED.get() { Ok(()) } else { mic_denied() }
    }

    fn fake_start(samples: Samples, events: Events) -> anyhow::Result<Stream> {
        CALLBACKS.with(|c| *c.borrow_mut() = Some((samples, events)));
        Ok(Stream::new(Box::new(|| {})))
    }

    fn start_audio_denied(_: Samples, _: Events) -> anyhow::Result<Stream> {
        Err(anyhow::anyhow!("create process tap failed").context(Denied::SystemAudio))
    }

    fn start_mic_denied(_: Samples, _: Events) -> anyhow::Result<Stream> {
        Err(anyhow::Error::new(Denied::Microphone))
    }

    fn start_broken(_: Samples, _: Events) -> anyhow::Result<Stream> {
        Err(anyhow::anyhow!("the default input device is gone"))
    }

    fn capture(
        check: fn() -> Result<(), Denied>,
        request: fn() -> Result<(), Denied>,
        start: Start,
    ) -> Capture {
        Capture {
            check,
            request,
            start,
        }
    }

    pub(crate) fn ok_capture() -> Capture {
        capture(granted, granted, fake_start)
    }

    pub(crate) fn mic_denied_capture() -> Capture {
        capture(mic_denied, mic_denied, fake_start)
    }

    /// Feeds `frames` of samples to the running fake capture, in 100 ms chunks.
    pub(crate) fn push(frames: usize) {
        let chunk = vec![1_000i16; 4_800];
        CALLBACKS.with(|c| {
            let mut c = c.borrow_mut();
            let (samples, _) = c.as_mut().expect("the capture was started");
            let mut left = frames;
            while left > 0 {
                let n = left.min(chunk.len());
                samples(&chunk[..n]);
                left -= n;
            }
        });
    }

    fn emit(event: Event) {
        CALLBACKS.with(|c| (c.borrow_mut().as_mut().expect("the capture was started").1)(event));
    }

    fn minute(_: u64) -> anyhow::Result<String> {
        Ok(STAMP.into())
    }

    pub(crate) fn write_config(env: &Env, ffmpeg: PathBuf) {
        Config {
            recordings_dir: env.home.join("Recordings"),
            vault_dir: env.home.join("Vault"),
            vault_folder: "Meetings".into(),
            ffmpeg_path: ffmpeg,
            claude_path: "/usr/local/bin/claude".into(),
            extension_id: "abcdefghijklmnopabcdefghijklmnop".into(),
            meeting_path: Some(tool(&env.home)),
        }
        .save_to(&env.config_path())
        .unwrap();
    }

    pub(crate) struct Rig {
        pub(crate) recorder: Recorder,
        pub(crate) home: PathBuf,
    }

    impl Drop for Rig {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.home);
        }
    }

    impl Rig {
        pub(crate) fn env(&self) -> Env {
            Env::at(self.home.clone())
        }

        fn folder(&self, suffix: &str) -> PathBuf {
            self.home
                .join("Recordings")
                .join(format!("meeting-{STAMP}{suffix}"))
        }

        /// Entries in the recordings folder: what a refused start must not leave.
        fn leftovers(&self) -> usize {
            fs::read_dir(self.home.join("Recordings")).map_or(0, |d| d.count())
        }

        pub(crate) fn start(&self) -> Result<PathBuf, Refused> {
            self.recorder.start(Language::En, || Ok(None))
        }

        pub(crate) fn stop(&self, reason: Reason) -> Status {
            self.recorder
                .stop(reason)
                .expect("a recording was running")
                .join()
                .unwrap();
            self.recorder.status()
        }

        pub(crate) fn state(&self) -> &'static str {
            self.recorder.status().state
        }

        /// Waits for an automatic stop to finish saving.
        pub(crate) fn wait_idle(&self) -> Status {
            let deadline = Instant::now() + Duration::from_secs(60);
            loop {
                let status = self.recorder.status();
                if status.state == "idle" {
                    return status;
                }
                assert!(Instant::now() < deadline, "still {}", status.state);
                thread::sleep(Duration::from_millis(20));
            }
        }
    }

    pub(crate) fn rig(name: &str, capture: Capture) -> Rig {
        rig_with(name, capture, 3 * 3600)
    }

    fn rig_with(name: &str, capture: Capture, cap_s: u64) -> Rig {
        rig_full(name, capture, cap_s, wav::open, || true)
    }

    /// A rig whose quit confirmation answers with `confirm_quit`.
    pub(crate) fn rig_quit(name: &str, confirm_quit: fn() -> bool) -> Rig {
        rig_full(name, ok_capture(), 3 * 3600, wav::open, confirm_quit)
    }

    fn rig_full(
        name: &str,
        capture: Capture,
        cap_s: u64,
        open_wav: fn(&std::path::Path) -> anyhow::Result<Box<dyn wav::Sink>>,
        confirm_quit: fn() -> bool,
    ) -> Rig {
        rig_from(name, deps(capture, cap_s, open_wav, confirm_quit))
    }

    /// A rig built without `FEATURE_CONTINUE_RECORDING`; every other rig has it on.
    pub(crate) fn rig_unflagged(name: &str) -> Rig {
        let flagged = deps(ok_capture(), 3 * 3600, wav::open, || true);
        rig_from(
            name,
            Deps {
                continue_enabled: false,
                ..flagged
            },
        )
    }

    fn deps(
        capture: Capture,
        cap_s: u64,
        open_wav: fn(&std::path::Path) -> anyhow::Result<Box<dyn wav::Sink>>,
        confirm_quit: fn() -> bool,
    ) -> Deps {
        Deps {
            capture,
            clock,
            local_minute: minute,
            open_wav,
            spawn: fake_spawn,
            spawn_continue: fake_continue_spawn,
            check_tool: meeting::spawn::check_tool,
            cap_s,
            flush_every: Duration::from_millis(20),
            confirm_quit,
            continue_enabled: true,
        }
    }

    fn rig_from(name: &str, deps: Deps) -> Rig {
        let home = temp(&format!("recorder-{name}"));
        let env = Env::at(home.clone());
        write_config(&env, ffmpeg());
        let recorder = Recorder::with(&env, deps);
        Rig { recorder, home }
    }

    #[test]
    fn start_creates_the_folder_with_the_wav_and_the_context() {
        let rig = rig("start", ok_capture());
        let folder = rig.start().unwrap();
        assert_eq!(folder, rig.folder(""));
        assert!(folder.join(WAV).is_file());
        let raw: Value = serde_json::from_slice(&fs::read(folder.join(CONTEXT)).unwrap()).unwrap();
        assert_eq!(raw["platform"], "desktop");
        assert!(meeting::context::check(&raw).is_ok());
        let status = rig.recorder.status();
        assert_eq!(
            (status.state, status.language, status.folder.as_deref()),
            ("recording", Language::En, folder.to_str())
        );
        assert_eq!(rig.stop(Reason::User).state, "idle");
    }

    #[test]
    fn the_idle_status_has_the_contract_shape() {
        let rig = rig("status", ok_capture());
        assert_eq!(
            serde_json::to_value(rig.recorder.status()).unwrap(),
            json!({
                "state": "idle",
                "elapsed_s": 0,
                "language": "bn",
                "folder": null,
                "notice": null,
                "start_error": null,
                "continuable": null,
                "part": null
            })
        );
    }

    #[test]
    fn starts_in_the_same_minute_get_a_suffix() {
        let rig = rig("suffix", ok_capture());
        for suffix in ["", "-2", "-3"] {
            assert_eq!(rig.start().unwrap(), rig.folder(suffix));
            rig.stop(Reason::User);
        }
    }

    #[test]
    fn a_denied_microphone_refuses_and_leaves_nothing() {
        let rig = rig("mic", capture(mic_denied, mic_denied, fake_start));
        assert_eq!(
            rig.start().unwrap_err(),
            Refused::new(
                "permission_mic",
                "Allow Meeting in System Settings › Privacy & Security › Microphone"
            )
        );
        assert_eq!(rig.leftovers(), 0);
        assert_eq!(rig.state(), "idle");

        let prompted = self::rig("mic-prompt", capture(mic_denied, granted, fake_start));
        assert!(prompted.start().is_ok());
        prompted.stop(Reason::User);
    }

    #[test]
    fn start_works_once_permission_is_granted_without_a_restart() {
        GRANTED.set(false);
        let rig = rig("mic-later", capture(mic_as_set, mic_as_set, fake_start));
        assert_eq!(rig.start().unwrap_err().code, "permission_mic");
        GRANTED.set(true);
        assert!(rig.start().is_ok());
        rig.stop(Reason::User);
    }

    #[test]
    fn a_failed_capture_start_maps_to_its_code_and_removes_the_folder() {
        let cases: [(Start, &str, String); 3] = [
            (
                start_audio_denied,
                "permission_audio",
                Denied::SystemAudio.to_string(),
            ),
            (
                start_mic_denied,
                "permission_mic",
                Denied::Microphone.to_string(),
            ),
            (
                start_broken,
                "capture_failed",
                "the default input device is gone".into(),
            ),
        ];
        for (start, code, message) in cases {
            let rig = rig(&format!("capture-{code}"), capture(granted, granted, start));
            assert_eq!(rig.start().unwrap_err(), Refused::new(code, message));
            assert_eq!(rig.leftovers(), 0, "{code}");
            assert_eq!(rig.state(), "idle");
        }
    }

    #[test]
    fn a_second_start_is_refused_while_recording_paused_or_saving() {
        let rig = rig("busy", ok_capture());
        rig.start().unwrap();
        assert_eq!(rig.start().unwrap_err().code, "already_recording");
        rig.recorder.pause().unwrap();
        assert_eq!(rig.start().unwrap_err().code, "already_recording");
        rig.stop(Reason::User);
        let saving = rig.recorder.begin_saving().unwrap();
        assert_eq!(rig.state(), "saving");
        assert_eq!(rig.start().unwrap_err().code, "already_recording");
        assert!(rig.recorder.begin_saving().is_none());
        drop(saving);
        assert_eq!(rig.state(), "idle");
        assert_eq!(rig.leftovers(), 1);
    }

    #[test]
    fn a_missing_config_or_ffmpeg_is_not_configured() {
        let rig = rig("config", ok_capture());
        fs::remove_file(rig.env().config_path()).unwrap();
        let refused = rig.start().unwrap_err();
        assert_eq!(refused.code, "not_configured");
        assert!(
            refused.message.contains("meeting install"),
            "{}",
            refused.message
        );
        write_config(&rig.env(), rig.home.join("no-ffmpeg"));
        let refused = rig.start().unwrap_err();
        assert_eq!(refused.code, "not_configured");
        assert!(
            refused.message.contains("meeting doctor"),
            "{}",
            refused.message
        );
        assert_eq!(rig.leftovers(), 0);
    }

    #[test]
    fn an_event_refusal_or_a_far_event_creates_no_folder() {
        let rig = rig("event-refused", ok_capture());
        for code in ["unknown_event", "calendar_denied"] {
            let refused = rig
                .recorder
                .start(Language::Bn, || Err(Refused::new(code, "x")))
                .unwrap_err();
            assert_eq!(refused.code, code);
        }
        // The clock reads 2026-11-13T00:00:00Z: one second past 7 days.
        let far = event("Far", "2026-11-20T06:00:01+06:00");
        let refused = rig
            .recorder
            .start(Language::Bn, || Ok(Some(far)))
            .unwrap_err();
        assert_eq!(refused.code, "event_too_far");
        let malformed = event("Odd", "next week");
        let refused = rig
            .recorder
            .start(Language::Bn, || Ok(Some(malformed)))
            .unwrap_err();
        assert_eq!(refused.code, "unknown_event");
        assert_eq!(rig.leftovers(), 0);
        assert_eq!(rig.state(), "idle");
    }

    #[test]
    fn a_chosen_event_is_in_context_json_from_the_start() {
        let rig = rig("event", ok_capture());
        let chosen = event("Weekly sync", "2026-11-20T06:00:00+06:00");
        let folder = rig
            .recorder
            .start(Language::Bn, || Ok(Some(chosen.clone())))
            .unwrap();
        // What a crash right now would leave for recovery.
        let ctx = meeting::context::load(&folder.join(CONTEXT)).unwrap();
        assert_eq!(ctx.calendar_event, Some(chosen));
        rig.stop(Reason::User);
    }

    #[test]
    fn the_calendar_answer_maps_to_the_contract_codes() {
        let found = event("Sync", "2026-11-13T06:00:00+06:00");
        assert_eq!(chosen_event(Ok(Some(found.clone()))), Ok(Some(found)));
        assert_eq!(chosen_event(Ok(None)).unwrap_err().code, "unknown_event");
        assert_eq!(
            chosen_event(Err(CalendarError::Denied)).unwrap_err(),
            Refused::new("calendar_denied", calendar::DENIED_MESSAGE)
        );
        assert_eq!(
            chosen_event(Err(CalendarError::Failed("the calendar worker stopped"))).unwrap_err(),
            Refused::new("unknown_event", "the calendar worker stopped")
        );
    }

    #[test]
    fn the_language_is_remembered_and_defaults_to_bengali() {
        let rig = rig("language", ok_capture());
        assert_eq!(rig.recorder.status().language, Language::Bn);
        rig.start().unwrap();
        rig.stop(Reason::User);
        assert_eq!(rig.recorder.status().language, Language::En);

        let broken = self::rig("language-broken", ok_capture());
        let store = broken.env().store_path;
        fs::create_dir_all(store.parent().unwrap()).unwrap();
        fs::write(&store, "this is not a database, not at all").unwrap();
        assert!(broken.start().is_ok());
        broken.stop(Reason::User);
        assert_eq!(broken.recorder.status().language, Language::Bn);
    }

    #[test]
    fn paused_time_is_not_recorded() {
        let rig = rig("pause", ok_capture());
        let folder = rig.start().unwrap();
        push(60 * SECOND);
        rig.recorder.pause().unwrap();
        assert_eq!(rig.state(), "paused");
        push(60 * SECOND);
        rig.recorder.resume().unwrap();
        assert_eq!(rig.state(), "recording");
        push(60 * SECOND);
        assert_eq!(rig.recorder.status().elapsed_s, 120);
        let status = rig.stop(Reason::User);
        assert_eq!(status.notice.unwrap().kind, Kind::Saved);
        let info = probe(&folder.join(M4A));
        assert!(
            info.contains("Duration: 00:02:00") || info.contains("Duration: 00:01:59"),
            "{info}"
        );
        assert!(!folder.join(WAV).exists());
        assert_eq!(
            spawns_under(&folder),
            [(
                rig.home.join("meeting"),
                folder.join(M4A),
                Some(Language::En)
            )]
        );
    }

    #[test]
    fn controls_in_the_wrong_state_are_refused() {
        let rig = rig("controls", ok_capture());
        assert_eq!(rig.recorder.pause().unwrap_err().code, "not_recording");
        assert_eq!(rig.recorder.resume().unwrap_err().code, "not_recording");
        assert!(rig.recorder.stop(Reason::User).is_none());
        rig.start().unwrap();
        assert_eq!(rig.recorder.resume().unwrap_err().code, "not_recording");
        rig.recorder.pause().unwrap();
        assert_eq!(rig.recorder.pause().unwrap_err().code, "not_recording");
        rig.recorder.resume().unwrap();
        rig.stop(Reason::User);
        assert!(rig.recorder.stop(Reason::User).is_none());
    }

    #[test]
    fn a_full_queue_drops_and_counts_the_chunk() {
        let shared = Arc::new(Shared::default());
        let (tx, rx) = mpsc::sync_channel(1);
        let (ctl, _ctl) = mpsc::sync_channel(4);
        let mut samples = samples_callback(shared.clone(), tx, u64::MAX, ctl);
        samples(&[1, 2]);
        samples(&[3]);
        samples(&[4]);
        assert_eq!(shared.frames.load(Ordering::Relaxed), 2);
        assert_eq!(shared.dropped.load(Ordering::Relaxed), 2);
        assert!(matches!(rx.try_recv(), Ok(Chunk::Samples(v)) if v == [1, 2]));
    }

    #[test]
    fn five_seconds_is_saved_but_not_processed() {
        let rig = rig("short", ok_capture());
        let folder = rig.start().unwrap();
        push(5 * SECOND);
        let notice = rig.stop(Reason::User).notice.unwrap();
        assert_eq!(notice.kind, Kind::TooShort);
        assert!(folder.join(M4A).is_file());
        assert!(!folder.join(WAV).exists());
        assert!(spawns_under(&folder).is_empty());
    }

    #[test]
    fn the_cap_stops_and_saves_counting_recorded_time_only() {
        let rig = rig_with("cap", ok_capture(), 12);
        let folder = rig.start().unwrap();
        push(11 * SECOND);
        rig.recorder.pause().unwrap();
        push(5 * SECOND);
        thread::sleep(Duration::from_millis(200));
        assert_eq!(rig.state(), "paused");
        rig.recorder.resume().unwrap();
        push(SECOND);
        let status = rig.wait_idle();
        assert_eq!(
            status.notice.unwrap().message,
            "Stopped at 3 hours. Saved, sent for processing."
        );
        assert_eq!(spawns_under(&folder).len(), 1);
        assert!(probe(&folder.join(M4A)).contains("Duration: 00:00:12"));
    }

    #[test]
    fn a_capture_failure_stops_and_saves_and_a_device_change_does_not() {
        let rig = rig("failed", ok_capture());
        let folder = rig.start().unwrap();
        push(10 * SECOND);
        emit(Event::DeviceChanged);
        thread::sleep(Duration::from_millis(200));
        assert_eq!(rig.state(), "recording");
        emit(Event::Failed("Microphone permission was revoked".into()));
        let status = rig.wait_idle();
        assert_eq!(
            status.notice.unwrap().message,
            "Recording stopped: Microphone permission was revoked. Saved, sent for processing."
        );
        assert_eq!(spawns_under(&folder).len(), 1);
    }

    #[test]
    fn a_full_disk_keeps_the_wav_and_converts_nothing() {
        let rig = rig("disk-full", ok_capture());
        let folder = rig.start().unwrap();
        push(2 * SECOND);
        let status = rig.stop(Reason::DiskFull);
        assert_eq!(
            status.notice,
            Some(Notice::retry(notice::DISK_FULL, &folder))
        );
        assert_eq!(wav::repair(&folder.join(WAV)).unwrap(), 2 * SECOND as u64);
        assert!(!folder.join(M4A).exists());
        assert!(spawns_under(&folder).is_empty());
        assert_eq!(status.state, "idle");
    }

    /// The WAV file, failing as a full disk does once it holds 2 s of audio.
    struct SmallDisk {
        file: fs::File,
        room: usize,
    }

    impl std::io::Write for SmallDisk {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            if buf.len() > self.room {
                return Err(std::io::Error::from(ErrorKind::StorageFull));
            }
            self.room -= buf.len();
            self.file.write(buf)
        }

        fn flush(&mut self) -> std::io::Result<()> {
            self.file.flush()
        }
    }

    impl std::io::Seek for SmallDisk {
        fn seek(&mut self, pos: std::io::SeekFrom) -> std::io::Result<u64> {
            self.file.seek(pos)
        }
    }

    fn open_small_disk(path: &std::path::Path) -> anyhow::Result<Box<dyn wav::Sink>> {
        let file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)?;
        Ok(Box::new(SmallDisk {
            file,
            room: 44 + 4 * SECOND,
        }))
    }

    #[test]
    fn a_full_disk_in_the_writer_stops_on_its_own_and_keeps_the_wav() {
        let rig = rig_full(
            "writer-full",
            ok_capture(),
            3 * 3600,
            open_small_disk,
            || true,
        );
        let folder = rig.start().unwrap();
        push(5 * SECOND);
        let status = rig.wait_idle();
        assert_eq!(
            status.notice,
            Some(Notice::retry(notice::DISK_FULL, &folder))
        );
        let kept = wav::repair(&folder.join(WAV)).unwrap();
        assert!(kept > 0 && kept <= 2 * SECOND as u64, "{kept}");
        assert!(!folder.join(M4A).exists());
        assert!(spawns_under(&folder).is_empty());
    }

    #[test]
    fn stop_answers_saving_at_once_and_idle_after_the_conversion() {
        let rig = rig("saving", ok_capture());
        let slow = rig.home.join("ffmpeg");
        fs::write(&slow, "#!/bin/sh\nsleep 1\necho slow >&2\nexit 1\n").unwrap();
        fs::set_permissions(&slow, fs::Permissions::from_mode(0o755)).unwrap();
        write_config(&rig.env(), slow);
        let folder = rig.start().unwrap();
        push(SECOND);
        let saving = rig.recorder.stop(Reason::User).unwrap();
        let status = rig.recorder.status();
        assert_eq!(status.state, "saving");
        assert_eq!(status.folder.as_deref(), folder.to_str());
        assert_eq!(status.elapsed_s, 1);
        assert_eq!(rig.start().unwrap_err().code, "already_recording");
        saving.join().unwrap();
        assert_eq!(rig.state(), "idle");
    }

    #[test]
    fn a_late_automatic_stop_never_ends_a_newer_recording() {
        let rig = rig("stale-stop", ok_capture());
        rig.start().unwrap();
        rig.stop(Reason::User);
        rig.start().unwrap();
        // The first session's id was 1.
        assert!(rig.recorder.stop_session(Some(1), Reason::Cap).is_none());
        assert_eq!(rig.state(), "recording");
        rig.stop(Reason::User);
    }

    #[test]
    fn a_write_failure_becomes_the_right_stop_reason() {
        assert_eq!(write_failure(ErrorKind::StorageFull), Reason::DiskFull);
        assert_eq!(
            write_failure(ErrorKind::PermissionDenied),
            Reason::CaptureFailed("cannot write audio: permission denied".into())
        );
    }

    #[test]
    fn sleep_and_quit_stops_save_with_their_message() {
        for (reason, message) in [
            (
                Reason::Sleep,
                "Stopped because the Mac went to sleep. Under 10 seconds, so it was saved but not processed.",
            ),
            (Reason::Quit, notice::TOO_SHORT),
        ] {
            let rig = rig(&format!("stop-{}", reason.kind()), ok_capture());
            let folder = rig.start().unwrap();
            push(SECOND);
            assert_eq!(rig.stop(reason).notice.unwrap().message, message);
            assert!(folder.join(M4A).is_file());
        }
    }

    #[test]
    fn a_failed_conversion_keeps_the_wav_and_offers_a_retry() {
        let rig = rig("ffmpeg-fails", ok_capture());
        let broken = rig.home.join("ffmpeg");
        fs::write(&broken, "#!/bin/sh\necho boom >&2\nexit 1\n").unwrap();
        fs::set_permissions(&broken, fs::Permissions::from_mode(0o755)).unwrap();
        write_config(&rig.env(), broken);
        let folder = rig.start().unwrap();
        push(SECOND);
        let status = rig.stop(Reason::User);
        assert_eq!(status.state, "idle");
        assert_eq!(
            status.notice,
            Some(Notice::retry("ffmpeg failed: boom", &folder))
        );
        assert!(folder.join(WAV).is_file());
        assert!(!folder.join(M4A).exists());
    }

    #[test]
    fn every_change_is_announced() {
        let rig = rig("on-change", ok_capture());
        let count = Arc::new(AtomicUsize::new(0));
        let seen = count.clone();
        rig.recorder.on_change(move || {
            seen.fetch_add(1, Ordering::SeqCst);
        });
        rig.start().unwrap();
        rig.recorder.pause().unwrap();
        rig.recorder.resume().unwrap();
        // Saving, then idle with the notice.
        rig.stop(Reason::User);
        assert_eq!(count.load(Ordering::SeqCst), 5);
    }

    #[test]
    fn every_listener_hears_every_change() {
        let rig = rig("listeners", ok_capture());
        let counts = [Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0))];
        for count in &counts {
            let seen = count.clone();
            let recorder = rig.recorder.clone();
            rig.recorder.on_change(move || {
                // A listener may read the status: `changed` holds no lock while calling it.
                let _ = recorder.status();
                seen.fetch_add(1, Ordering::SeqCst);
            });
        }
        rig.start().unwrap();
        assert_eq!(counts.each_ref().map(|c| c.load(Ordering::SeqCst)), [1, 1]);
        // Saving, then idle with the notice.
        rig.stop(Reason::User);
        assert_eq!(counts.each_ref().map(|c| c.load(Ordering::SeqCst)), [3, 3]);
    }

    fn start_error(rig: &Rig) -> Option<StartError> {
        rig.recorder.status().start_error
    }

    /// Counts the recorder's announcements.
    fn counter(rig: &Rig) -> Arc<AtomicUsize> {
        let count = Arc::new(AtomicUsize::new(0));
        let seen = count.clone();
        rig.recorder.on_change(move || {
            seen.fetch_add(1, Ordering::SeqCst);
        });
        count
    }

    #[test]
    fn a_refused_ad_hoc_start_becomes_the_start_error() {
        let cases: [(&str, Capture); 4] = [
            (
                "permission_mic",
                capture(mic_denied, mic_denied, fake_start),
            ),
            (
                "permission_audio",
                capture(granted, granted, start_audio_denied),
            ),
            ("capture_failed", capture(granted, granted, start_broken)),
            ("not_configured", ok_capture()),
        ];
        for (code, capture) in cases {
            let rig = rig(&format!("start-error-{code}"), capture);
            if code == "not_configured" {
                write_config(&rig.env(), rig.home.join("no-ffmpeg"));
            }
            let count = counter(&rig);
            let refused = rig.recorder.start_ad_hoc(None).unwrap_err();
            assert_eq!(refused.code, code);
            assert_eq!(
                start_error(&rig),
                Some(StartError {
                    code,
                    message: refused.message
                }),
                "{code}"
            );
            assert_eq!(count.load(Ordering::SeqCst), 1, "{code}");
        }
        let rig = rig(
            "start-error-text",
            capture(mic_denied, mic_denied, fake_start),
        );
        rig.recorder.start_ad_hoc(None).unwrap_err();
        assert_eq!(
            start_error(&rig).unwrap().message,
            "Allow Meeting in System Settings › Privacy & Security › Microphone"
        );
    }

    #[test]
    fn already_recording_is_not_a_start_error() {
        let rig = rig("start-error-busy", ok_capture());
        rig.start().unwrap();
        let count = counter(&rig);
        let refused = rig.recorder.start_ad_hoc(None).unwrap_err();
        assert_eq!(refused.code, "already_recording");
        assert_eq!(start_error(&rig), None);
        assert_eq!(count.load(Ordering::SeqCst), 0);
        rig.stop(Reason::User);
    }

    #[test]
    fn a_refused_window_start_is_not_a_start_error() {
        let rig = rig(
            "start-error-window",
            capture(mic_denied, mic_denied, fake_start),
        );
        let count = counter(&rig);
        assert_eq!(rig.start().unwrap_err().code, "permission_mic");
        assert_eq!(start_error(&rig), None);
        assert_eq!(count.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn any_successful_start_clears_the_start_error() {
        GRANTED.set(false);
        let rig = rig(
            "start-error-cleared",
            capture(mic_as_set, mic_as_set, fake_start),
        );
        let count = counter(&rig);
        for ad_hoc in [false, true] {
            rig.recorder.start_ad_hoc(None).unwrap_err();
            assert!(start_error(&rig).is_some());
            GRANTED.set(true);
            let before = count.load(Ordering::SeqCst);
            if ad_hoc {
                rig.recorder.start_ad_hoc(None).unwrap();
            } else {
                rig.start().unwrap();
            }
            assert_eq!(start_error(&rig), None);
            // One announcement for the start, which also clears it.
            assert_eq!(count.load(Ordering::SeqCst), before + 1);
            rig.stop(Reason::User);
            GRANTED.set(false);
        }
        GRANTED.set(true);
    }

    #[test]
    fn dismiss_clears_the_start_error_once() {
        let rig = rig(
            "start-error-dismiss",
            capture(mic_denied, mic_denied, fake_start),
        );
        let count = counter(&rig);
        rig.recorder.dismiss_start_error();
        assert_eq!(count.load(Ordering::SeqCst), 0, "nothing to dismiss");
        rig.recorder.start_ad_hoc(None).unwrap_err();
        rig.recorder.dismiss_start_error();
        assert_eq!(start_error(&rig), None);
        assert_eq!(count.load(Ordering::SeqCst), 2);
        rig.recorder.dismiss_start_error();
        assert_eq!(count.load(Ordering::SeqCst), 2, "already clear");
    }

    #[test]
    fn dismiss_clears_the_notice_once_and_announces() {
        let rig = rig("notice-dismiss", ok_capture());
        let count = counter(&rig);
        rig.recorder.dismiss_notice();
        assert_eq!(count.load(Ordering::SeqCst), 0, "nothing to dismiss");
        rig.start().unwrap();
        push(11 * SECOND);
        // Start, saving, then idle with the notice.
        assert_eq!(rig.stop(Reason::User).notice.unwrap().kind, Kind::Saved);
        assert_eq!(count.load(Ordering::SeqCst), 3);
        rig.recorder.dismiss_notice();
        assert_eq!(rig.recorder.status().notice, None);
        assert_eq!(count.load(Ordering::SeqCst), 4);
        rig.recorder.dismiss_notice();
        assert_eq!(count.load(Ordering::SeqCst), 4, "already clear");
    }

    #[test]
    fn every_listener_hears_a_dismissal_in_order() {
        let rig = rig("notice-dismiss-listeners", ok_capture());
        rig.start().unwrap();
        rig.stop(Reason::User);
        let heard = Arc::new(Mutex::new(Vec::new()));
        for n in 0..2 {
            let heard = heard.clone();
            let recorder = rig.recorder.clone();
            rig.recorder.on_change(move || {
                // `changed` holds no lock while calling a listener.
                assert_eq!(recorder.status().notice, None);
                lock(&heard).push(n);
            });
        }
        rig.recorder.dismiss_notice();
        assert_eq!(*lock(&heard), [0, 1]);
    }

    #[test]
    fn a_dismissed_notice_stays_dismissed_across_polls() {
        let rig = rig("notice-dismiss-polls", ok_capture());
        rig.start().unwrap();
        assert!(rig.stop(Reason::User).notice.is_some());
        rig.recorder.dismiss_notice();
        for _ in 0..3 {
            assert_eq!(rig.recorder.status().notice, None);
        }
    }

    #[test]
    fn a_new_notice_shows_after_a_dismissal() {
        let rig = rig("notice-dismiss-next", ok_capture());
        rig.start().unwrap();
        rig.stop(Reason::User);
        rig.recorder.dismiss_notice();
        rig.start().unwrap();
        push(11 * SECOND);
        assert_eq!(rig.stop(Reason::User).notice.unwrap().kind, Kind::Saved);
    }

    #[test]
    fn an_ad_hoc_start_uses_the_last_language_unless_given_one() {
        let rig = rig("start-ad-hoc-language", ok_capture());
        rig.recorder.start_ad_hoc(None).unwrap();
        assert_eq!(rig.recorder.status().language, Language::Bn);
        rig.stop(Reason::User);
        rig.recorder.start_ad_hoc(Some(Language::En)).unwrap();
        assert_eq!(rig.recorder.status().language, Language::En);
        rig.stop(Reason::User);
        assert_eq!(rig.recorder.status().language, Language::En);
        rig.recorder.start_ad_hoc(None).unwrap();
        assert_eq!(rig.recorder.status().language, Language::En);
        rig.stop(Reason::User);
    }

    static ASKED: AtomicUsize = AtomicUsize::new(0);

    fn cancel() -> bool {
        ASKED.fetch_add(1, Ordering::SeqCst);
        false
    }

    fn confirm() -> bool {
        ASKED.fetch_add(1, Ordering::SeqCst);
        true
    }

    fn never_asked() -> bool {
        panic!("idle and saving quit without asking")
    }

    #[test]
    fn quitting_when_idle_or_saving_does_not_ask() {
        let rig = rig_quit("quit-idle", never_asked);
        assert!(rig.recorder.quit());

        let slow = rig.home.join("ffmpeg");
        fs::write(&slow, "#!/bin/sh\nsleep 1\nexit 1\n").unwrap();
        fs::set_permissions(&slow, fs::Permissions::from_mode(0o755)).unwrap();
        write_config(&rig.env(), slow);
        rig.start().unwrap();
        push(SECOND);
        let saving = rig.recorder.stop(Reason::User).unwrap();
        assert_eq!(rig.state(), "saving");
        assert!(rig.recorder.quit());
        // `quit` waited for the save, so the notice is already there.
        assert_eq!(rig.state(), "idle");
        assert!(rig.recorder.status().notice.is_some());
        saving.join().unwrap();
    }

    #[test]
    fn cancelling_the_quit_keeps_recording() {
        let rig = rig_quit("quit-cancel", cancel);
        rig.start().unwrap();
        let asked = ASKED.load(Ordering::SeqCst);
        assert!(!rig.recorder.quit());
        assert!(ASKED.load(Ordering::SeqCst) > asked);
        assert_eq!(rig.state(), "recording");
        rig.recorder.pause().unwrap();
        assert!(!rig.recorder.quit());
        assert_eq!(rig.state(), "paused");
        rig.stop(Reason::User);
    }

    #[test]
    fn confirming_the_quit_saves_and_hands_off_first() {
        let rig = rig_quit("quit-confirm", confirm);
        let folder = rig.start().unwrap();
        push(12 * SECOND);
        assert!(rig.recorder.quit());
        let status = rig.recorder.status();
        assert_eq!(status.state, "idle");
        assert_eq!(status.notice.unwrap().kind, Kind::Saved);
        assert!(folder.join(M4A).is_file());
        assert_eq!(spawns_under(&folder).len(), 1);
    }

    #[test]
    fn the_timer_reads_only_while_recording() {
        let rig = rig("timer", ok_capture());
        assert_eq!(rig.recorder.recording_elapsed_s(), None);
        rig.start().unwrap();
        push(3 * SECOND);
        assert_eq!(rig.recorder.recording_elapsed_s(), Some(3));
        rig.recorder.pause().unwrap();
        assert_eq!(rig.recorder.recording_elapsed_s(), None);
        rig.stop(Reason::User);
    }

    #[test]
    fn the_real_clock_reads_now_and_a_valid_zone() {
        let c = real_clock();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        assert!((now - c.now_ms).abs() < 5_000);
        assert!((-720..=840).contains(&c.utc_offset_min));
        if let Some(zone) = &c.time_zone {
            assert!(meeting::context::time_zone_ok(zone), "{zone}");
        }
    }

    const EARLIER: &str = "meeting-2026-11-13_0500";

    fn stored_last(rig: &Rig) -> LastRecording {
        let raw = rig
            .env()
            .store()
            .unwrap()
            .setting("last_recording")
            .unwrap();
        serde_json::from_str(&raw.unwrap()).unwrap()
    }

    /// An earlier meeting (`recording.m4a` and `context.json`) stored as the last recording with
    /// `recorded_s` and `parts`, in Bengali (Start in these tests is English).
    fn earlier(rig: &Rig, recorded_s: u64, parts: u32) -> PathBuf {
        let folder = rig.home.join("Recordings").join(EARLIER);
        fs::create_dir_all(&folder).unwrap();
        tone_wav(&folder.join(WAV), 1);
        convert::to_m4a(&ffmpeg(), &folder.join(WAV), &folder.join(M4A)).unwrap();
        fs::remove_file(folder.join(WAV)).unwrap();
        handoff::write_context(&folder, &clock(), None);
        store_last(
            rig,
            &LastRecording {
                folder: folder.display().to_string(),
                recorded_s,
                language: Language::Bn,
                stopped_at: clock().now_ms,
                parts,
            },
        );
        folder
    }

    fn link(rig: &Rig, n: u32) -> PathBuf {
        rig.home
            .join("Recordings")
            .join(format!(".continuing-{EARLIER}-part-{n}.partial.wav"))
    }

    /// The folder's file names and contents: what a continue must leave as it was.
    fn snapshot(folder: &std::path::Path) -> Vec<(String, Vec<u8>)> {
        let mut files: Vec<_> = fs::read_dir(folder)
            .unwrap()
            .map(|e| {
                let e = e.unwrap();
                (
                    e.file_name().to_string_lossy().into_owned(),
                    fs::read(e.path()).unwrap(),
                )
            })
            .collect();
        files.sort();
        files
    }

    fn mode(path: &std::path::Path) -> u32 {
        fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn continue_records_a_part_into_the_meeting_folder_and_hands_it_off() {
        use std::os::unix::fs::MetadataExt;
        let rig = rig("continue", ok_capture());
        let folder = earlier(&rig, 42, 1);
        let before = snapshot(&folder);
        assert_eq!(rig.recorder.continue_recording().unwrap(), folder);
        // No new meeting folder: the meeting and the part's link only.
        assert_eq!(rig.leftovers(), 2);
        let (wav, link) = (folder.join(handoff::part_wav(2)), link(&rig, 2));
        assert_eq!((mode(&wav), mode(&link)), (0o600, 0o600));
        let (w, l) = (fs::metadata(&wav).unwrap(), fs::metadata(&link).unwrap());
        assert_eq!((w.dev(), w.ino()), (l.dev(), l.ino()));
        for (name, bytes) in &before {
            assert_eq!(&fs::read(folder.join(name)).unwrap(), bytes, "{name}");
        }

        let status = rig.recorder.status();
        assert_eq!(
            (status.state, status.part, status.elapsed_s, status.language),
            ("recording", Some(2), 42, Language::Bn)
        );
        assert_eq!(status.folder.as_deref(), folder.to_str());
        assert_eq!(status.continuable, None);
        push(3 * SECOND);
        assert_eq!(rig.recorder.status().elapsed_s, 45);
        assert_eq!(rig.recorder.recording_elapsed_s(), Some(45));
        rig.recorder.pause().unwrap();
        push(5 * SECOND);
        let paused = rig.recorder.status();
        assert_eq!(
            (paused.state, paused.elapsed_s, paused.part),
            ("paused", 45, Some(2))
        );
        rig.recorder.resume().unwrap();

        let status = rig.stop(Reason::User);
        assert_eq!(status.notice, Some(Notice::new(Kind::Saved, notice::SAVED)));
        assert_eq!(status.part, None);
        assert!(folder.join(handoff::part_m4a(2)).is_file());
        assert!(!wav.exists());
        assert!(!link.exists());
        for (name, bytes) in &before {
            assert_eq!(&fs::read(folder.join(name)).unwrap(), bytes, "{name}");
        }
        assert_eq!(
            continues_under(&folder),
            [(
                rig.home.join("meeting"),
                folder.join(M4A),
                Some(Language::Bn)
            )]
        );
        assert!(spawns_under(&folder).is_empty());
        let last = stored_last(&rig);
        assert_eq!(
            (last.recorded_s, last.parts, last.language),
            (45, 2, Language::Bn)
        );
        // The part's language is the one saved for the next start and for recovery (D9).
        assert_eq!(rig.recorder.status().language, Language::Bn);
    }

    #[test]
    fn continue_a_second_time_records_part_three() {
        let rig = rig("continue-again", ok_capture());
        let folder = earlier(&rig, 20, 1);
        rig.recorder.continue_recording().unwrap();
        push(SECOND);
        rig.stop(Reason::User);
        assert_eq!(stored_last(&rig).parts, 2);
        rig.recorder.continue_recording().unwrap();
        let status = rig.recorder.status();
        assert_eq!((status.part, status.elapsed_s), (Some(3), 21));
        assert!(folder.join(handoff::part_wav(3)).is_file());
        rig.stop(Reason::User);
        assert_eq!(stored_last(&rig).parts, 3);
    }

    #[test]
    fn continue_skips_a_part_number_already_used() {
        for leftover in [handoff::part_wav(2), handoff::part_m4a(2)] {
            let rig = rig(&format!("continue-taken-{leftover}"), ok_capture());
            let folder = earlier(&rig, 20, 1);
            fs::write(folder.join(&leftover), b"kept").unwrap();
            rig.recorder.continue_recording().unwrap();
            assert_eq!(rig.recorder.status().part, Some(3), "{leftover}");
            rig.stop(Reason::User);
            assert_eq!(fs::read(folder.join(&leftover)).unwrap(), b"kept");
            assert!(folder.join(handoff::part_m4a(3)).is_file());
            assert_eq!(stored_last(&rig).parts, 3);
        }
    }

    #[test]
    fn continue_a_new_start_and_idle_report_no_part() {
        let rig = rig("continue-start", ok_capture());
        assert_eq!(rig.recorder.status().part, None);
        rig.start().unwrap();
        assert_eq!(rig.recorder.status().part, None);
        rig.stop(Reason::User);
    }

    #[test]
    fn continue_stops_at_the_cap_on_the_meeting_total() {
        let rig = rig("continue-cap", ok_capture());
        let folder = earlier(&rig, 10_740, 1);
        rig.recorder.continue_recording().unwrap();
        push(60 * SECOND);
        let status = rig.wait_idle();
        assert_eq!(
            status.notice.unwrap().message,
            "Stopped at 3 hours. Saved, sent for processing."
        );
        assert!(probe(&folder.join(handoff::part_m4a(2))).contains("Duration: 00:01:00"));
        assert_eq!(continues_under(&folder).len(), 1);
        assert_eq!(stored_last(&rig).recorded_s, 3 * 3600);
    }

    #[test]
    fn continue_hands_off_once_the_meeting_total_reaches_ten_seconds() {
        let rig = rig("continue-short", ok_capture());
        let folder = earlier(&rig, 3, 1);
        rig.recorder.continue_recording().unwrap();
        push(4 * SECOND);
        assert_eq!(rig.stop(Reason::User).notice.unwrap().kind, Kind::TooShort);
        assert!(folder.join(handoff::part_m4a(2)).is_file());
        assert!(continues_under(&folder).is_empty());

        let rig = self::rig("continue-short-total", ok_capture());
        let folder = earlier(&rig, 8, 1);
        rig.recorder.continue_recording().unwrap();
        push(5 * SECOND);
        assert_eq!(rig.stop(Reason::User).notice.unwrap().kind, Kind::Saved);
        assert_eq!(continues_under(&folder).len(), 1);
    }

    #[test]
    fn continue_with_nothing_to_continue_is_refused_and_creates_nothing() {
        let nothing = Refused::new(
            "nothing_to_continue",
            "Nothing to continue: the last recording stopped more than 2 hours ago, or was moved.",
        );
        let rig = rig("continue-nothing", ok_capture());
        assert_eq!(rig.recorder.continue_recording().unwrap_err(), nothing);
        assert_eq!((rig.leftovers(), rig.state()), (0, "idle"));

        let folder = earlier(&rig, 20, 1);
        let before = snapshot(&folder);
        let mut last = stored_last(&rig);
        last.stopped_at -= 2 * 3600 * 1000 + 1;
        store_last(&rig, &last);
        assert_eq!(rig.recorder.continue_recording().unwrap_err(), nothing);
        let mut last = stored_last(&rig);
        last.stopped_at = clock().now_ms;
        last.recorded_s = 3 * 3600;
        store_last(&rig, &last);
        assert_eq!(rig.recorder.continue_recording().unwrap_err(), nothing);
        assert_eq!(snapshot(&folder), before);
        assert_eq!(rig.leftovers(), 1);

        fs::remove_file(folder.join(M4A)).unwrap();
        earlier(&rig, 20, 1);
        fs::remove_file(folder.join(M4A)).unwrap();
        assert_eq!(rig.recorder.continue_recording().unwrap_err(), nothing);
        earlier(&rig, 20, 1);
        fs::remove_dir_all(&folder).unwrap();
        assert_eq!(rig.recorder.continue_recording().unwrap_err(), nothing);
        assert_eq!((rig.leftovers(), rig.state()), (0, "idle"));
    }

    #[test]
    fn continue_refuses_nothing_to_continue_before_any_permission() {
        let rig = rig("continue-order", mic_denied_capture());
        assert_eq!(
            rig.recorder.continue_recording().unwrap_err().code,
            "nothing_to_continue"
        );
        let folder = earlier(&rig, 20, 1);
        let before = snapshot(&folder);
        assert_eq!(
            rig.recorder.continue_recording().unwrap_err(),
            Refused::new(
                "permission_mic",
                "Allow Meeting in System Settings › Privacy & Security › Microphone"
            )
        );
        assert_eq!(snapshot(&folder), before);
        assert_eq!(rig.leftovers(), 1);
        assert_eq!(rig.state(), "idle");
    }

    #[test]
    fn continue_runs_starts_configuration_checks() {
        let rig = rig("continue-config", ok_capture());
        earlier(&rig, 20, 1);
        write_config(&rig.env(), rig.home.join("no-ffmpeg"));
        let refused = rig.recorder.continue_recording().unwrap_err();
        assert_eq!(refused.code, "not_configured");
        assert!(
            refused.message.contains("meeting doctor"),
            "{}",
            refused.message
        );
        assert_eq!(rig.leftovers(), 1);
    }

    #[test]
    fn continue_failures_never_touch_the_meeting() {
        let cases: [(Start, &str); 3] = [
            (start_audio_denied, "permission_audio"),
            (start_mic_denied, "permission_mic"),
            (start_broken, "capture_failed"),
        ];
        for (start, code) in cases {
            let rig = rig(
                &format!("continue-fail-{code}"),
                capture(granted, granted, start),
            );
            let folder = earlier(&rig, 20, 1);
            let before = snapshot(&folder);
            assert_eq!(rig.recorder.continue_recording().unwrap_err().code, code);
            assert_eq!(snapshot(&folder), before, "{code}");
            assert_eq!(rig.leftovers(), 1, "{code}");
            assert_eq!(rig.state(), "idle");
        }
    }

    #[test]
    fn continue_refuses_a_link_that_cannot_be_made_and_keeps_the_meeting() {
        let rig = rig("continue-no-link", ok_capture());
        let folder = earlier(&rig, 20, 1);
        let before = snapshot(&folder);
        let recordings = rig.home.join("Recordings");
        fs::set_permissions(&recordings, fs::Permissions::from_mode(0o555)).unwrap();
        let refused = rig.recorder.continue_recording();
        fs::set_permissions(&recordings, fs::Permissions::from_mode(0o755)).unwrap();
        let refused = refused.unwrap_err();
        assert_eq!(refused.code, "capture_failed");
        assert!(
            refused.message.starts_with("cannot write audio: "),
            "{}",
            refused.message
        );
        assert_eq!(snapshot(&folder), before);
        assert_eq!(rig.leftovers(), 1);
        assert_eq!(rig.state(), "idle");
    }

    #[test]
    fn continue_abandoned_removes_only_the_part() {
        let rig = rig("continue-abandon", ok_capture());
        let folder = earlier(&rig, 20, 1);
        let before = snapshot(&folder);
        let (session, _rx) = rig.recorder.open_part().unwrap();
        assert!(folder.join(handoff::part_wav(2)).is_file());
        session.abandon();
        *rig.recorder.state() = State::Idle;
        assert_eq!(snapshot(&folder), before);
        assert_eq!(rig.leftovers(), 1);
    }

    #[test]
    fn continue_and_start_never_record_together() {
        let rig = rig("continue-busy", ok_capture());
        earlier(&rig, 20, 1);
        rig.recorder.continue_recording().unwrap();
        assert_eq!(
            rig.recorder.continue_recording().unwrap_err().code,
            "already_recording"
        );
        assert_eq!(rig.start().unwrap_err().code, "already_recording");
        rig.recorder.pause().unwrap();
        assert_eq!(
            rig.recorder.continue_recording().unwrap_err().code,
            "already_recording"
        );
        rig.stop(Reason::User);

        rig.start().unwrap();
        assert_eq!(
            rig.recorder.continue_recording().unwrap_err().code,
            "already_recording"
        );
        rig.stop(Reason::User);
        let saving = rig.recorder.begin_saving().unwrap();
        assert_eq!(
            rig.recorder.continue_recording().unwrap_err().code,
            "already_recording"
        );
        drop(saving);
    }

    #[test]
    fn continue_two_quick_calls_record_once() {
        let rig = rig("continue-race", ok_capture());
        earlier(&rig, 20, 1);
        let gate = Arc::new(std::sync::Barrier::new(2));
        let (recorder, other_gate) = (rig.recorder.clone(), gate.clone());
        let other = thread::spawn(move || {
            other_gate.wait();
            recorder
                .continue_recording()
                .map(|_| ())
                .map_err(|r| r.code)
        });
        gate.wait();
        let here = rig
            .recorder
            .continue_recording()
            .map(|_| ())
            .map_err(|r| r.code);
        let mut codes = [here, other.join().unwrap()];
        codes.sort();
        assert_eq!(codes, [Ok(()), Err("already_recording")]);
        assert_eq!(rig.state(), "recording");
        rig.stop(Reason::User);
    }

    #[test]
    fn continue_conversion_failure_keeps_the_part_wav_and_the_meeting() {
        let rig = rig("continue-ffmpeg", ok_capture());
        let folder = earlier(&rig, 20, 1);
        let m4a = fs::read(folder.join(M4A)).unwrap();
        let broken = rig.home.join("ffmpeg");
        fs::write(&broken, "#!/bin/sh\necho boom >&2\nexit 1\n").unwrap();
        fs::set_permissions(&broken, fs::Permissions::from_mode(0o755)).unwrap();
        write_config(&rig.env(), broken);
        rig.recorder.continue_recording().unwrap();
        push(SECOND);
        let status = rig.stop(Reason::User);
        assert_eq!(status.state, "idle");
        assert_eq!(
            status.notice,
            Some(Notice::retry("ffmpeg failed: boom", &folder))
        );
        assert!(folder.join(handoff::part_wav(2)).is_file());
        assert!(!link(&rig, 2).exists());
        assert_eq!(fs::read(folder.join(M4A)).unwrap(), m4a);
        assert!(continues_under(&folder).is_empty());
        // The earlier meeting stays the last recording, unchanged.
        let last = stored_last(&rig);
        assert_eq!((last.recorded_s, last.parts), (20, 1));
    }

    #[test]
    fn continue_spawn_failure_keeps_the_part_m4a() {
        let rig = rig_from(
            "continue-spawn",
            Deps {
                spawn_continue: handoff::tests::failing_spawn,
                ..deps(ok_capture(), 3 * 3600, wav::open, || true)
            },
        );
        let folder = earlier(&rig, 20, 1);
        rig.recorder.continue_recording().unwrap();
        push(SECOND);
        let notice = rig.stop(Reason::User).notice.unwrap();
        assert_eq!(
            notice.message,
            "Saved, but processing could not be started: fork refused"
        );
        assert!(folder.join(handoff::part_m4a(2)).is_file());
    }

    #[test]
    fn continue_disk_full_keeps_the_part_wav() {
        let rig = rig("continue-disk-full", ok_capture());
        let folder = earlier(&rig, 20, 1);
        rig.recorder.continue_recording().unwrap();
        push(2 * SECOND);
        let status = rig.stop(Reason::DiskFull);
        assert_eq!(
            status.notice,
            Some(Notice::retry(notice::DISK_FULL, &folder))
        );
        assert!(folder.join(handoff::part_wav(2)).is_file());
        assert!(!link(&rig, 2).exists());
        assert!(continues_under(&folder).is_empty());
    }

    /// Asserts the part was saved as a new meeting, sent without `--continue`.
    fn saved_as_new(rig: &Rig, status: Status) -> PathBuf {
        assert_eq!(
            status.notice,
            Some(Notice::new(
                Kind::Error,
                format!("Could not add to {EARLIER}: it was moved; saved as a new meeting.")
            ))
        );
        let new = rig.folder("");
        assert!(new.join(M4A).is_file());
        assert!(new.join(CONTEXT).is_file());
        assert!(!new.join(WAV).exists());
        assert!(probe(&new.join(M4A)).contains("Duration: 00:00:12"));
        assert!(!link(rig, 2).exists());
        assert_eq!(
            spawns_under(&new),
            [(rig.home.join("meeting"), new.join(M4A), Some(Language::Bn))]
        );
        assert!(continues_under(&rig.home).is_empty());
        let last = stored_last(rig);
        assert_eq!(
            (last.folder.as_str(), last.recorded_s, last.parts),
            (new.to_str().unwrap(), 12, 1)
        );
        new
    }

    #[test]
    fn continue_saves_a_part_of_a_moved_meeting_as_a_new_one() {
        let rig = rig("continue-moved", ok_capture());
        let folder = earlier(&rig, 42, 1);
        rig.recorder.continue_recording().unwrap();
        push(12 * SECOND);
        let archive = rig.home.join("Recordings").join("Archive");
        fs::create_dir_all(&archive).unwrap();
        fs::rename(&folder, archive.join(EARLIER)).unwrap();
        let saving = rig.recorder.stop(Reason::User).unwrap();
        saving.join().unwrap();
        saved_as_new(&rig, rig.recorder.status());
        assert!(archive.join(EARLIER).join(M4A).is_file());
    }

    #[test]
    fn continue_saves_a_part_as_a_new_meeting_when_the_recording_is_gone() {
        let rig = rig("continue-m4a-gone", ok_capture());
        let folder = earlier(&rig, 42, 1);
        rig.recorder.continue_recording().unwrap();
        push(12 * SECOND);
        fs::remove_file(folder.join(M4A)).unwrap();
        let status = rig.stop(Reason::User);
        saved_as_new(&rig, status);
        // The part's copy left in the old folder was the same file.
        assert!(!folder.join(handoff::part_wav(2)).exists());
        assert!(!folder.join(handoff::part_m4a(2)).exists());
    }

    fn broken_minute(_: u64) -> anyhow::Result<String> {
        anyhow::bail!("/bin/date failed")
    }

    #[test]
    fn continue_keeps_the_link_when_a_moved_part_cannot_become_a_new_meeting() {
        let rig = rig_from(
            "continue-moved-fails",
            Deps {
                local_minute: broken_minute,
                ..deps(ok_capture(), 3 * 3600, wav::open, || true)
            },
        );
        let folder = earlier(&rig, 42, 1);
        rig.recorder.continue_recording().unwrap();
        push(12 * SECOND);
        let archive = rig.home.join("Recordings").join("Archive");
        fs::create_dir_all(&archive).unwrap();
        fs::rename(&folder, archive.join(EARLIER)).unwrap();
        let status = rig.stop(Reason::User);
        let notice = status.notice.unwrap();
        assert_eq!(notice.kind, Kind::Error);
        assert!(
            notice.message.starts_with(&format!(
                "Could not add to {EARLIER}: it was moved, and saving it as a new meeting failed: "
            )),
            "{}",
            notice.message
        );
        assert_eq!(status.state, "idle");
        // Nothing recorded is lost: the link stays for launch recovery.
        assert_eq!(wav::repair(&link(&rig, 2)).unwrap(), 12 * SECOND as u64);
        assert!(spawns_under(&rig.home).is_empty());
        assert_eq!(stored_last(&rig).recorded_s, 42);
    }

    #[test]
    fn continue_holds_the_folder_from_launch_recovery() {
        let rig = rig("continue-recover", ok_capture());
        let folder = earlier(&rig, 20, 1);
        rig.recorder.continue_recording().unwrap();
        // The folder recovery skips: the meeting's, not a new one.
        assert_eq!(rig.recorder.session_folder(), Some(folder.clone()));
        push(SECOND);
        // The part's WAV is still being written by the writer thread: its name is compared, not
        // its bytes.
        let settled = || {
            let mut files = snapshot(&folder);
            for (name, bytes) in &mut files {
                if *name == handoff::part_wav(2) {
                    bytes.clear();
                }
            }
            files
        };
        let before = settled();
        rig.recorder.recover_all();
        assert_eq!(settled(), before);
        assert_eq!(rig.state(), "recording");
        let status = rig.stop(Reason::User);
        // Saved as the part by the stop, not by recovery.
        assert_eq!(status.notice.unwrap().kind, Kind::Saved);
        assert!(folder.join(handoff::part_m4a(2)).is_file());
    }

    #[test]
    fn continue_flag_off_has_nothing_to_continue_and_no_part() {
        let rig = rig_unflagged("continue-off");
        let folder = earlier(&rig, 20, 1);
        assert_eq!(
            rig.recorder.continue_recording().unwrap_err().code,
            "nothing_to_continue"
        );
        assert!(!folder.join(handoff::part_wav(2)).exists());
        let status = serde_json::to_value(rig.recorder.status()).unwrap();
        assert_eq!(status["part"], Value::Null);
    }
}
