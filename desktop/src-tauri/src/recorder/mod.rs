//! Recording (PRD desktop-app FR-1 to FR-6): start, pause, resume, stop and the automatic stops
//! (3 hours of recorded time, capture failure, full disk), the WAV written while recording, and
//! the hand-off to `meeting process`. One lock guards the state, held only for short transitions:
//! the Rust analogue of the extension's `serial()`. Sleep, quit, crash recovery and retry: #149.

pub mod convert;
pub mod folder;
pub mod handoff;
pub mod notice;
pub mod wav;

use std::fmt::Display;
use std::fs;
use std::io::ErrorKind;
use std::mem;
use std::panic::{self, AssertUnwindSafe};
use std::path::PathBuf;
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
use crate::reply;
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
    pub check_tool: CheckToolFn,
    pub cap_s: u64,
    pub flush_every: Duration,
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
}

enum State {
    Idle,
    /// `start` is checking permissions and opening the session; a second start is refused.
    Starting,
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
    on_change: Mutex<Option<OnChange>>,
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
                check_tool: meeting::spawn::check_tool,
                cap_s: CAP_S,
                flush_every: FLUSH_EVERY,
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
                on_change: Mutex::new(None),
            }),
        }
    }

    /// `f` runs after every change of state or notice, on whichever thread made it.
    pub fn on_change(&self, f: impl Fn() + Send + Sync + 'static) {
        *lock(&self.inner.on_change) = Some(Arc::new(f));
    }

    fn changed(&self) {
        let f = lock(&self.inner.on_change).clone();
        if let Some(f) = f {
            f();
        }
    }

    fn state(&self) -> MutexGuard<'_, State> {
        lock(&self.inner.state)
    }

    pub fn status(&self) -> Status {
        let (state, frames, language, folder) = match &*self.state() {
            State::Idle | State::Starting => ("idle", 0, None, None),
            State::Active(s) => {
                let paused = s.shared.paused.load(Ordering::Relaxed);
                (
                    if paused { "paused" } else { "recording" },
                    s.shared.frames.load(Ordering::Relaxed),
                    Some(s.language),
                    Some(s.folder.clone()),
                )
            }
            State::Saving {
                folder,
                frames,
                language,
            } => ("saving", *frames, *language, folder.clone()),
        };
        Status {
            state,
            elapsed_s: frames / u64::from(SAMPLE_RATE),
            language: language.unwrap_or_else(|| self.saved_language()),
            folder: folder.map(|f| f.display().to_string()),
            notice: lock(&self.inner.notice).clone(),
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
        {
            let mut state = self.state();
            if !matches!(*state, State::Idle) {
                return Err(Refused::new(
                    "already_recording",
                    "A recording is already running; stop it first",
                ));
            }
            *state = State::Starting;
        }
        // A panic in an injected call must not leave the recorder stuck in `Starting`.
        let opened = panic::catch_unwind(AssertUnwindSafe(|| self.open(language, event)))
            .unwrap_or_else(|_| {
                Err(Refused::new(
                    "capture_failed",
                    "Could not start recording; try again",
                ))
            });
        let mut state = self.state();
        let (session, rx) = match opened {
            Ok(opened) => opened,
            Err(refused) => {
                *state = State::Idle;
                return Err(refused);
            }
        };
        let (id, folder) = (session.id, session.folder.clone());
        // Cleared before the session is visible, so a fast automatic stop's notice survives.
        *lock(&self.inner.notice) = None;
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
        tracing::info!("recorder: started");
        self.save_language(language);
        self.changed();
        Ok(folder)
    }

    /// The checks in the order the contract gives, then the folder, the WAV, `context.json` and
    /// the capture stream.
    fn open(
        &self,
        language: Language,
        event: impl FnOnce() -> Result<Option<CalendarEvent>, Refused>,
    ) -> Result<(Session, Receiver<Msg>), Refused> {
        let deps = &self.inner.deps;
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
        let started = (deps.clock)();
        let event = event()?;
        if let Some(event) = &event {
            within_horizon(event, started.now_ms.div_euclid(1000))?;
        }
        (deps.capture.check)()
            .or_else(|_| (deps.capture.request)())
            .map_err(denied)?;
        let now_s = u64::try_from(started.now_ms.div_euclid(1000)).unwrap_or(0);
        let folder = folder::new_folder(&config.recordings_dir, deps.local_minute, now_s)
            .map_err(|e| Refused::new("capture_failed", format!("{e:#}")))?;
        let opened = self.open_in(folder.clone(), language, started, event, config.ffmpeg_path);
        if opened.is_err() {
            let _ = fs::remove_dir_all(&folder);
        }
        opened
    }

    fn open_in(
        &self,
        folder: PathBuf,
        language: Language,
        started: Clock,
        event: Option<CalendarEvent>,
        ffmpeg: PathBuf,
    ) -> Result<(Session, Receiver<Msg>), Refused> {
        let deps = &self.inner.deps;
        let shared = Arc::new(Shared::default());
        let (ctl, rx) = mpsc::sync_channel(4);
        let on_error = {
            let ctl = ctl.clone();
            Box::new(move |kind| {
                let _ = ctl.try_send(Msg::Stop(write_failure(kind)));
            })
        };
        let sink = (deps.open_wav)(&folder.join(handoff::WAV))
            .map_err(|e| Refused::new("capture_failed", format!("{e:#}")))?;
        let writer = WavWriter::start(
            sink,
            deps.flush_every,
            QUEUE_CHUNKS,
            shared.dropped.clone(),
            on_error,
        )
        .map_err(|e| Refused::new("capture_failed", format!("cannot write audio: {e}")))?;
        handoff::write_context(&folder, &started, event.clone());
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
            ..
        } = *session;
        stream.stop();
        let _ = ctl.try_send(Msg::Ended);
        let (frames, disk_full) = match writer.finish() {
            Ok(frames) => (frames, reason == Reason::DiskFull),
            Err(kind) => {
                tracing::warn!("recorder: the WAV writer failed: {kind}");
                let frames = wav::repair(&folder.join(handoff::WAV)).unwrap_or(0);
                (
                    frames,
                    kind == ErrorKind::StorageFull || reason == Reason::DiskFull,
                )
            }
        };
        let recorder = self.clone();
        Some(thread::spawn(move || {
            let saved = panic::catch_unwind(AssertUnwindSafe(|| {
                handoff::ensure_context(&folder, &started, event);
                if disk_full {
                    Notice::retry(notice::DISK_FULL, &folder)
                } else {
                    let tools = recorder.tools(ffmpeg);
                    handoff::finish_recording(&tools, &folder, language, frames, &reason.prefix())
                }
            }));
            // Never stuck in `saving`: the WAV, if still there, can be retried.
            let notice = saved
                .unwrap_or_else(|_| Notice::retry("Saving failed unexpectedly; retry", &folder));
            recorder.finish_saving(notice);
        }))
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
        let _ = fs::remove_dir_all(&self.folder);
    }
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

/// Called once at startup.
pub fn on_launch<R: Runtime>(_app: &AppHandle<R>) {}

/// Called once at startup; registers for the Mac going to sleep.
pub fn on_sleep<R: Runtime>(_app: &AppHandle<R>) {}

/// Called when the app is asked to quit. `false` keeps it running.
pub fn on_exit_requested<R: Runtime>(_app: &AppHandle<R>) -> bool {
    true
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
pub fn recorder_retry() -> Value {
    reply::not_implemented("recorder_retry")
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::os::unix::fs::PermissionsExt;
    use std::sync::atomic::AtomicUsize;
    use std::time::Instant;

    use meeting::config::Config;

    use super::*;
    use crate::capture::{Events, Samples};
    use crate::recorder::convert::tests::{ffmpeg, probe};
    use crate::recorder::handoff::tests::{clock, event, fake_spawn, spawns_under, tool};
    use crate::recorder::handoff::{CONTEXT, M4A, WAV};
    use crate::recorder::notice::Kind;
    use crate::recorder::wav::tests::temp;

    const SECOND: usize = 48_000;
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

    fn ok_capture() -> Capture {
        capture(granted, granted, fake_start)
    }

    /// Feeds `frames` of samples to the running fake capture, in 100 ms chunks.
    fn push(frames: usize) {
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

    fn write_config(env: &Env, ffmpeg: PathBuf) {
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

    struct Rig {
        recorder: Recorder,
        home: PathBuf,
    }

    impl Drop for Rig {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.home);
        }
    }

    impl Rig {
        fn env(&self) -> Env {
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

        fn start(&self) -> Result<PathBuf, Refused> {
            self.recorder.start(Language::En, || Ok(None))
        }

        fn stop(&self, reason: Reason) -> Status {
            self.recorder
                .stop(reason)
                .expect("a recording was running")
                .join()
                .unwrap();
            self.recorder.status()
        }

        fn state(&self) -> &'static str {
            self.recorder.status().state
        }

        /// Waits for an automatic stop to finish saving.
        fn wait_idle(&self) -> Status {
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

    fn rig(name: &str, capture: Capture) -> Rig {
        rig_with(name, capture, 3 * 3600)
    }

    fn rig_with(name: &str, capture: Capture, cap_s: u64) -> Rig {
        rig_full(name, capture, cap_s, wav::open)
    }

    fn rig_full(
        name: &str,
        capture: Capture,
        cap_s: u64,
        open_wav: fn(&std::path::Path) -> anyhow::Result<Box<dyn wav::Sink>>,
    ) -> Rig {
        let home = temp(&format!("recorder-{name}"));
        let env = Env::at(home.clone());
        write_config(&env, ffmpeg());
        let recorder = Recorder::with(
            &env,
            Deps {
                capture,
                clock,
                local_minute: minute,
                open_wav,
                spawn: fake_spawn,
                check_tool: meeting::spawn::check_tool,
                cap_s,
                flush_every: Duration::from_millis(20),
            },
        );
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
            json!({"state": "idle", "elapsed_s": 0, "language": "bn", "folder": null, "notice": null})
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
        let rig = rig_full("writer-full", ok_capture(), 3 * 3600, open_small_disk);
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
}
