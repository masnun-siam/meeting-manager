//! `meeting process`: extract → transcribe → summarise → write for one recording, in the
//! foreground. Every stage change is saved to the job file and the `meta.json` run first.

use std::fs;
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context, anyhow};

use crate::Language;
use crate::calendar::{self, Meeting, MeetingAttendee, Recording};
use crate::calendar_fetch::{self, Policy};
use crate::config::{Config, check_recording};
use crate::context_block::{self, Input};
use crate::deepgram::Deepgram;
use crate::jobs::{Job, JobStore, Stage, new_job_id, now_utc, write_atomic};
use crate::lock::Lock;
use crate::meta::{Meta, Run, Speaker, recorded_at_for};
use crate::store::{self, EventLink, Row, RunResult, RunStart, Source, Status, Store};
use crate::vault::{self, CalendarOutcome, Note};
use crate::{context, extract, ical, plan, summarise, summary_md, transcript};

const NO_SPEECH: &str = "No speech detected";

pub struct Pipeline {
    pub config: Config,
    pub deepgram: Deepgram,
    pub jobs: JobStore,
    /// Called only when Deepgram is, so a resume never needs the Keychain.
    pub key: fn() -> anyhow::Result<String>,
    pub claude_timeout: Duration,
    /// `FEATURE_MEETING_CONTEXT`: off, `context.json` and the calendar are never read.
    pub context: bool,
    /// `FEATURE_DESKTOP_APP`: off, no store is touched.
    pub desktop: bool,
    /// The notes database; opened only when `desktop`.
    pub store_path: PathBuf,
    /// The secret iCal address. Called only for a context with a meeting ID.
    pub calendar: fn() -> anyhow::Result<Option<String>>,
    pub calendar_policy: Policy,
}

/// What the meeting context adds to the summary and the note; empty without one.
#[derive(Default)]
struct Extra {
    block: Option<String>,
    meeting_title: Option<String>,
    attendees: Vec<String>,
    calendar: Option<CalendarOutcome>,
}

pub enum Ending {
    Done { title: String },
    Failed { stage: Stage, error: anyhow::Error },
}

/// The files a recording's run reads and writes, all in the recording's folder.
struct Paths {
    recording: PathBuf,
    audio: PathBuf,
    raw: PathBuf,
    transcript: PathBuf,
    summary: PathBuf,
    context: PathBuf,
}

impl Paths {
    fn new(recording: PathBuf) -> Paths {
        let next_to = |name| recording.with_file_name(name);
        Paths {
            audio: next_to("audio.ogg"),
            raw: next_to("deepgram.json"),
            transcript: next_to("transcript.md"),
            summary: next_to("summary.md"),
            context: next_to("context.json"),
            recording,
        }
    }
}

/// The notes store for this run. `run` is set once the recording's lock is held, so a run
/// refused before that never touches the row another run owns.
struct Track {
    store: Store,
    run: Option<RunInfo>,
}

/// What every store write of a run repeats.
struct RunInfo {
    source: Source,
    recorded_at: String,
    language: Option<Language>,
    event: Option<EventLink>,
}

/// What a run has recorded so far; `meta` stays `None` until the stage plan is known.
struct State<'a> {
    jobs: &'a JobStore,
    job: Job,
    meta: Option<Meta>,
    /// `None` with the desktop flag off, or when the store cannot be opened.
    track: Option<Track>,
    stage_started: Instant,
    out: &'a mut dyn Write,
}

impl State<'_> {
    fn meta(&mut self) -> &mut Meta {
        self.meta
            .as_mut()
            .expect("meta is set before the first stage")
    }

    /// Saves the job and, once there is one, the `meta.json` run for it.
    fn persist(&mut self) -> anyhow::Result<()> {
        self.jobs.write(&self.job)?;
        let run = Run {
            job_id: self.job.job_id.clone(),
            started_at: self.job.started_at.clone(),
            ended_at: matches!(self.job.stage, Stage::Done | Stage::Failed).then(now_utc),
            stage: self.job.stage,
            error: self.job.error.clone(),
        };
        let path = Meta::path_for(&self.job.recording);
        if let Some(meta) = &mut self.meta {
            meta.upsert_run(run);
            meta.save(&path)?;
        }
        Ok(())
    }

    fn enter(&mut self, stage: Stage) -> anyhow::Result<()> {
        if self.job.stage != Stage::Queued {
            tracing::info!(
                "{} took {:?}",
                self.job.stage.name(),
                self.stage_started.elapsed()
            );
        }
        self.stage_started = Instant::now();
        self.job.stage = stage;
        self.job.updated_at = now_utc();
        self.persist()?;
        // A closed stdout must not fail the job.
        let _ = writeln!(self.out, "{}", stage.name());
        Ok(())
    }

    fn fail(&mut self, error: &anyhow::Error) {
        let stage = self.job.stage.name();
        let reason = format!("{error:#}");
        let reason = reason.lines().next().unwrap_or_default();
        self.job.error = Some(format!("{stage}: {reason}"));
        self.job.stage = Stage::Failed;
        self.job.updated_at = now_utc();
        if let Err(e) = self.persist() {
            tracing::error!("cannot record the failure: {e:#}");
        }
        self.merge(Status::Failed, |r| {
            r.failed_stage = Some(stage.into());
            r.error = Some(reason.into());
        });
    }

    /// Stores the run's outcome in the notes store and returns the merged row. `fill` adds what
    /// this outcome knows; a field left `None` keeps its stored value. A store failure is
    /// logged by kind and never fails the run (FR-13).
    fn merge(&self, status: Status, fill: impl FnOnce(&mut RunResult)) -> Option<Row> {
        let track = self.track.as_ref()?;
        let run = track.run.as_ref()?;
        let meta = self.meta.as_ref();
        let mut result = RunResult {
            status,
            failed_stage: None,
            error: None,
            title: None,
            body_md: None,
            speakers: vec![],
            language: meta.map(|m| m.language).or(run.language),
            duration_s: meta
                .and_then(|m| m.duration_s)
                .map(|secs| secs.round() as i64),
            transcript: None,
            event: run.event.clone(),
            source: run.source,
            recorded_at: run.recorded_at.clone(),
            attendees: None,
        };
        fill(&mut result);
        track
            .store
            .merge_run(&self.job.recording, &result)
            .inspect_err(|e| tracing::warn!("store merge failed: {}", store::error_kind(e)))
            .ok()
    }

    fn finish(&mut self, title: &str, summary: Option<PathBuf>) -> anyhow::Result<()> {
        self.job.stage = Stage::Done;
        self.job.title = Some(title.into());
        self.job.summary_path = summary;
        self.job.updated_at = now_utc();
        self.persist()?;
        let shown = self
            .job
            .summary_path
            .as_deref()
            .map_or(title.into(), |p| p.display().to_string());
        let _ = writeln!(self.out, "done: {shown}");
        Ok(())
    }
}

impl Pipeline {
    /// Runs the pipeline. It does not notify; the caller does, so tests stay quiet.
    pub fn run(
        &self,
        recording: &Path,
        retranscribe: bool,
        language: Option<Language>,
        job_id: Option<String>,
        out: &mut impl Write,
    ) -> Ending {
        let recording = match check_recording(&self.config.recordings_dir, recording) {
            Ok(recording) => recording,
            Err(reason) => {
                return Ending::Failed {
                    stage: Stage::Queued,
                    error: anyhow!(reason),
                };
            }
        };
        let recording = recording.as_path();
        let job = match self.start_job(recording, job_id) {
            Ok(job) => job,
            Err(error) => {
                return Ending::Failed {
                    stage: Stage::Queued,
                    error,
                };
            }
        };
        let mut state = State {
            jobs: &self.jobs,
            job,
            meta: None,
            track: self.open_store(),
            stage_started: Instant::now(),
            out,
        };
        match self.execute(&mut state, recording, retranscribe, language) {
            Ok(title) => Ending::Done { title },
            Err(error) => {
                let stage = state.job.stage;
                state.fail(&error);
                Ending::Failed { stage, error }
            }
        }
    }

    fn open_store(&self) -> Option<Track> {
        if !self.desktop {
            return None;
        }
        Store::open(&self.store_path)
            .inspect_err(|e| tracing::warn!("store open failed: {}", store::error_kind(e)))
            .ok()
            .map(|store| Track { store, run: None })
    }

    /// The host's queued job (keeping its `started_at`), or a new one.
    fn start_job(&self, recording: &Path, job_id: Option<String>) -> anyhow::Result<Job> {
        let existing = match &job_id {
            Some(id) => self.jobs.read(id)?,
            None => None,
        };
        let now = now_utc();
        let mut job = existing.unwrap_or_else(|| Job {
            job_id: job_id.unwrap_or_else(new_job_id),
            recording: recording.into(),
            title: None,
            stage: Stage::Queued,
            error: None,
            started_at: now.clone(),
            updated_at: now.clone(),
            summary_path: None,
        });
        job.stage = Stage::Queued;
        job.error = None;
        job.updated_at = now;
        self.jobs.write(&job)?;
        Ok(job)
    }

    fn execute(
        &self,
        st: &mut State,
        recording: &Path,
        retranscribe: bool,
        language: Option<Language>,
    ) -> anyhow::Result<String> {
        let recording = fs::canonicalize(recording)
            .map_err(|_| anyhow!("recording not found: {}", recording.display()))?;
        st.job.recording = recording.clone();
        let p = Paths::new(recording);
        let _lock = Lock::acquire(&Lock::path_for(&p.recording))?;

        let stored = Meta::load(&Meta::path_for(&p.recording))?;
        let ctx = self.load_context(&p);
        if let Some(track) = &mut st.track {
            track.run = start_tracking(&track.store, &p, stored.as_ref(), ctx.as_ref(), language);
        }
        let files = plan::Files {
            webm: modified(&p.recording),
            audio: modified(&p.audio),
            raw: modified(&p.raw),
            transcript: modified(&p.transcript),
        };
        let plan = plan::plan(
            &files,
            stored.as_ref().map(|m| m.language),
            retranscribe,
            language,
        )?;
        let mut meta = match stored {
            Some(meta) => meta,
            None => Meta {
                recording: p.recording.clone(),
                language: plan.language,
                recorded_at: recorded_at_for(&p.recording)?,
                duration_s: None,
                title: None,
                speakers: vec![],
                runs: vec![],
            },
        };
        meta.language = plan.language;
        st.meta = Some(meta);
        // Saved before any stage, so a retry without `--language` finds the language.
        st.persist()?;

        if plan.delete_transcript_and_raw {
            remove(&p.transcript)?;
            remove(&p.raw)?;
        }
        if plan.extract {
            st.enter(Stage::Extracting)?;
            extract::extract(&self.config.ffmpeg_path, &p.recording, &p.audio)?;
        }
        if plan.transcribe || plan.render {
            st.enter(Stage::Transcribing)?;
            if plan.transcribe {
                let key = (self.key)()?;
                let raw = self.deepgram.transcribe(&p.audio, plan.language, &key)?;
                write_atomic(&p.raw, &raw)?;
            }
            if plan.render {
                let raw =
                    fs::read(&p.raw).with_context(|| format!("cannot read {}", p.raw.display()))?;
                let parsed = transcript::parse(&raw)?;
                write_atomic(
                    &p.transcript,
                    transcript::render(&parsed, plan.language).as_bytes(),
                )?;
                st.meta().duration_s = parsed.duration_s;
            }
        }

        let text = fs::read_to_string(&p.transcript)
            .with_context(|| format!("cannot read {}", p.transcript.display()))?;
        let (title, summary) = if transcript::has_speech(&text) {
            let title = self.summarise_and_write(st, &p, &text, plan.language, ctx.as_ref())?;
            (title, Some(p.summary.clone()))
        } else {
            st.merge(Status::NoSpeech, |r| {
                r.title = Some(NO_SPEECH.into());
                r.transcript = Some(transcript::utterance_lines(&text));
            });
            (NO_SPEECH.to_string(), None)
        };

        remove(&p.audio)?;
        remove(&p.raw)?;
        st.finish(&title, summary)?;
        Ok(title)
    }

    /// `context.json`, read once per run. With the desktop flag off its `calendar_event` is
    /// dropped here, so nothing of the event reaches Claude, the note or a store.
    fn load_context(&self, p: &Paths) -> Option<context::Context> {
        if !(self.context || self.desktop) {
            return None;
        }
        let mut ctx = context::load(&p.context)?;
        if !self.desktop {
            ctx.calendar_event = None;
            ctx.truncated.retain(|field| field != "calendar_event");
        }
        Some(ctx)
    }

    /// Returns the title the note was written with: Claude's, or the one edited in the app.
    fn summarise_and_write(
        &self,
        st: &mut State,
        p: &Paths,
        text: &str,
        language: Language,
        ctx: Option<&context::Context>,
    ) -> anyhow::Result<String> {
        st.enter(Stage::Summarising)?;
        let extra = self.context_extra(ctx, st.meta());
        let summary = summarise::summarise(
            &self.config.claude_path,
            text,
            extra.block.as_deref(),
            self.claude_timeout,
        )?;

        st.enter(Stage::Writing)?;
        let body = summary_md::render(&summary);
        write_atomic(&p.summary, body.as_bytes())?;
        let merged = st.merge(Status::Done, |r| {
            r.title = Some(summary.title.clone());
            r.body_md = Some(summary_md::body_without_title(&body).to_string());
            r.speakers = summary
                .speakers
                .iter()
                .map(|s| (s.label.clone(), s.name.clone(), s.evidence.clone()))
                .collect();
            r.transcript = Some(transcript::utterance_lines(text));
        });
        // The note is written from the merged row, so a rerun keeps what was edited in the app
        // (FR-19); without a row it is Claude's output, as with the flag off.
        let (title, body, speakers) = match &merged {
            Some(Row {
                title: Some(title),
                body_md: Some(body_md),
                my_notes,
                speakers,
                ..
            }) => (
                title.clone(),
                vault::compose_body(title, body_md, my_notes),
                speakers.iter().map(store::shown_name).collect(),
            ),
            _ => (
                summary.title.clone(),
                body,
                summary_md::speaker_names(&summary),
            ),
        };
        let note = vault::write_note(
            &self.config.vault_dir,
            &self.config.vault_folder,
            &Note {
                recorded_at: &st.meta().recorded_at,
                title: &title,
                language,
                speakers: &speakers,
                recording: &p.recording,
                transcript: &p.transcript,
                body: &body,
                meeting_title: extra.meeting_title.as_deref(),
                attendees: &extra.attendees,
                calendar: extra.calendar,
            },
        )?;
        if let (Some(row), Some(track)) = (&merged, &st.track)
            && let Err(e) = track.store.set_vault_note_path(row.id, &note)
        {
            tracing::warn!("store note path failed: {}", store::error_kind(&e));
        }
        // `meta.json` is Claude's record, like `summary.md`.
        let meta = st.meta();
        meta.title = Some(summary.title);
        meta.speakers = summary
            .speakers
            .into_iter()
            .map(|s| Speaker {
                label: s.label,
                name: s.name,
                evidence: s.evidence,
            })
            .collect();
        Ok(title)
    }

    /// Rebuilt on every run, so a rerun picks up a changed invite. An event picked in the
    /// desktop app is the calendar match: the Keychain and the calendar are left alone (FR-12).
    fn context_extra(&self, context: Option<&context::Context>, meta: &Meta) -> Extra {
        let Some(context) = context.filter(|_| self.context) else {
            return Extra::default();
        };
        let (meeting, calendar) = match (&context.calendar_event, &context.meeting_id) {
            (Some(event), _) => {
                tracing::info!("calendar matched by calendar_event");
                (Some(meeting_of(event)), Some(CalendarOutcome::Matched))
            }
            (None, Some(id)) => self.look_up(context, id),
            (None, None) => (None, None),
        };
        let block = context_block::build(&Input {
            context: Some(context),
            meeting: meeting.as_ref(),
            recorded_at: &meta.recorded_at,
            duration_s: meta.duration_s,
        });
        let (meeting_title, attendees) = vault::note_people(Some(context), meeting.as_ref());
        Extra {
            block,
            meeting_title,
            attendees,
            calendar,
        }
    }

    /// The calendar event for this meeting, and the outcome; no outcome without a stored
    /// address. Logs only the kind of failure, never the address or a value.
    fn look_up(
        &self,
        context: &context::Context,
        meeting_id: &str,
    ) -> (Option<Meeting>, Option<CalendarOutcome>) {
        let failed = |kind: &dyn std::fmt::Display| {
            tracing::warn!("calendar {kind}");
            (None, Some(CalendarOutcome::FetchFailed))
        };
        let address = match (self.calendar)() {
            Ok(Some(address)) => address,
            Ok(None) => return (None, None),
            Err(_) => return failed(&"keychain"),
        };
        let text = match calendar_fetch::fetch(&address, &self.calendar_policy) {
            Ok(text) => text,
            Err(kind) => return failed(&kind),
        };
        let Ok(parsed) = ical::parse(&text) else {
            return failed(&"not iCalendar");
        };
        let recording = Recording {
            meeting_id: meeting_id.into(),
            started_at_ms: context.started_at_ms,
            utc_offset_min: context.utc_offset_min,
            time_zone: context.time_zone.clone(),
        };
        match calendar::pick(&parsed, &recording) {
            Some(meeting) => {
                tracing::info!("calendar matched");
                (Some(meeting), Some(CalendarOutcome::Matched))
            }
            None => {
                tracing::info!("calendar no_match");
                (None, Some(CalendarOutcome::NoMatch))
            }
        }
    }
}

/// Marks the recording's row as processing. `None` only when the recording's time cannot be
/// read, which ends the run a few lines later anyway.
fn start_tracking(
    store: &Store,
    p: &Paths,
    stored: Option<&Meta>,
    ctx: Option<&context::Context>,
    language: Option<Language>,
) -> Option<RunInfo> {
    let recorded_at = match stored {
        Some(meta) => meta.recorded_at.clone(),
        None => recorded_at_for(&p.recording).ok()?,
    };
    let run = RunInfo {
        source: source_of(&p.recording, ctx),
        recorded_at,
        language: language.or(stored.map(|m| m.language)),
        event: ctx.and_then(|c| c.calendar_event.as_ref()).map(event_link),
    };
    let start = RunStart {
        recording: &p.recording,
        source: run.source,
        recorded_at: &run.recorded_at,
        language: run.language,
        attendees: None,
    };
    if let Err(e) = store.start_run(&start) {
        tracing::warn!("store start failed: {}", store::error_kind(&e));
    }
    Some(run)
}

/// The row's `source` by the desktop PRD's rule; the app's backfill applies the same one.
pub fn source_of(recording: &Path, ctx: Option<&context::Context>) -> Source {
    if recording.extension().is_some_and(|ext| ext == "webm") {
        Source::Browser
    } else if ctx.is_some_and(|c| c.platform == context::Platform::Desktop) {
        Source::Desktop
    } else {
        Source::Import
    }
}

/// The store's link to the calendar event; also used by the app's backfill (D40).
pub fn event_link(event: &context::CalendarEvent) -> EventLink {
    EventLink {
        occurrence_id: event.occurrence_id.clone(),
        series_id: event.series_id.clone(),
        title: event.title.clone(),
        start: event.start.clone(),
        end: event.end.clone(),
        calendar: event.calendar.clone(),
    }
}

/// The event as a calendar match. The calendar's name stays behind (D31).
fn meeting_of(event: &context::CalendarEvent) -> Meeting {
    Meeting {
        title: Some(event.title.clone()).filter(|title| !title.is_empty()),
        description: event.description.clone().unwrap_or_default(),
        organizer: event.organizer.clone(),
        attendees: event
            .attendees
            .iter()
            .map(|a| MeetingAttendee {
                name: a.name.clone(),
                response: a.response,
            })
            .collect(),
    }
}

fn modified(path: &Path) -> Option<SystemTime> {
    fs::metadata(path).and_then(|m| m.modified()).ok()
}

fn remove(path: &Path) -> anyhow::Result<()> {
    match fs::remove_file(path) {
        Err(e) if e.kind() != ErrorKind::NotFound => {
            Err(e).with_context(|| format!("cannot remove {}", path.display()))
        }
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{Edit, SpeakerRow};
    use crate::test_support::{VALID, envelope, mock, mock_raw, stall, stub, temp};
    use serde_json::{Value, json};
    use std::cell::{Cell, RefCell};
    use std::process::Command;
    use std::sync::mpsc::Receiver;

    const FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/short.webm");
    // ffmpeg -i crates/meeting/tests/fixtures/short.webm -vn -c:a aac crates/meeting/tests/fixtures/short.m4a
    const M4A: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/short.m4a");
    const EN: &str = include_str!("../tests/fixtures/deepgram/en_two_speakers.json");
    const EMPTY: &str = include_str!("../tests/fixtures/deepgram/empty.json");
    const NAME: &str = "meeting-2026-10-19_1430";
    const GOOGLE: &str = include_str!("../tests/fixtures/calendar/google.ics");

    thread_local! {
        /// What the fake Keychain holds for this test's thread, and how often it was read.
        static ADDRESS: RefCell<Option<String>> = const { RefCell::new(None) };
        static READS: Cell<usize> = const { Cell::new(0) };
    }

    fn stored_address() -> anyhow::Result<Option<String>> {
        READS.set(READS.get() + 1);
        Ok(ADDRESS.with_borrow(Clone::clone))
    }

    fn locked_keychain() -> anyhow::Result<Option<String>> {
        READS.set(READS.get() + 1);
        Err(anyhow!("cannot read the Keychain: locked"))
    }

    fn reads() -> usize {
        READS.get()
    }

    /// Serves `responses` as the calendar, one per request; keep the receiver alive.
    fn serve_calendar(responses: Vec<String>) -> Receiver<String> {
        let (base, requests) = mock_raw(responses);
        ADDRESS.set(Some(format!("{base}/basic.ics")));
        requests
    }

    fn ok(body: &str) -> String {
        format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
    }

    /// A Meet call recorded during the 2026-10-21 09:00 New York instance of `GOOGLE`.
    fn meet() -> Value {
        json!({"version": 1, "platform": "meet", "meeting_id": "abc-defg-hij",
            "meeting_title": "Meet title", "started_at_ms": 1_792_587_600_000_i64,
            "utc_offset_min": -240, "time_zone": "America/New_York",
            "participants": [{"name": "Rahim", "self": false}]})
    }

    struct Env {
        rec: PathBuf,
        tools: PathBuf,
        vault: PathBuf,
        pipeline: Pipeline,
        requests: Receiver<String>,
    }

    fn ffmpeg() -> PathBuf {
        let out = Command::new("/usr/bin/which")
            .arg("ffmpeg")
            .output()
            .unwrap();
        assert!(out.status.success(), "install ffmpeg (brew install ffmpeg)");
        String::from_utf8(out.stdout).unwrap().trim().into()
    }

    /// A temp dir holding the recording, a vault, a jobs dir, a stub `claude` that prints
    /// `VALID`, and a Deepgram mock answering `responses`.
    fn setup(name: &str, responses: Vec<(u16, &'static str)>) -> Env {
        let root = temp(name).canonicalize().unwrap();
        let (rec, tools, vault) = (root.join("rec"), root.join("tools"), root.join("vault"));
        for dir in [&rec, &tools, &vault] {
            fs::create_dir_all(dir).unwrap();
        }
        fs::create_dir_all(rec.join(NAME)).unwrap();
        fs::copy(FIXTURE, rec.join(NAME).join("recording.webm")).unwrap();
        let claude = stub(&tools, &format!("cat <<'EOF'\n{}\nEOF", envelope(VALID)));
        let (base_url, requests) = mock(responses);
        let pipeline = Pipeline {
            config: Config {
                recordings_dir: rec.clone(),
                vault_dir: vault.clone(),
                vault_folder: "Meetings".into(),
                ffmpeg_path: ffmpeg(),
                claude_path: claude,
                extension_id: "x".into(),
                meeting_path: None,
            },
            deepgram: Deepgram {
                base_url,
                retry_delays: vec![],
            },
            jobs: JobStore {
                dir: root.join("jobs"),
            },
            key: || Ok("test-key".into()),
            claude_timeout: Duration::from_secs(10),
            context: false,
            desktop: false,
            store_path: root.join("store/notes.sqlite"),
            calendar: stored_address,
            calendar_policy: Policy {
                https_only: false,
                max_redirects: 3,
                timeout: Duration::from_millis(300),
                max_bytes: 8 * 1024,
            },
        };
        Env {
            rec,
            tools,
            vault,
            pipeline,
            requests,
        }
    }

    impl Env {
        fn file(&self, name: &str) -> PathBuf {
            self.rec.join(NAME).join(name)
        }

        fn run(&self, language: Option<Language>, job_id: Option<&str>) -> (Ending, String) {
            let mut out = Vec::new();
            let ending = self.pipeline.run(
                &self.file("recording.webm"),
                false,
                language,
                job_id.map(String::from),
                &mut out,
            );
            (ending, String::from_utf8(out).unwrap())
        }

        fn job(&self) -> Job {
            let jobs = self.pipeline.jobs.list(10).unwrap();
            assert_eq!(jobs.len(), 1);
            jobs.into_iter().next().unwrap()
        }

        fn meta(&self) -> Meta {
            Meta::load(&self.file("meta.json")).unwrap().unwrap()
        }

        fn names(&self) -> Vec<String> {
            let mut names: Vec<String> = fs::read_dir(self.rec.join(NAME))
                .unwrap()
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect();
            names.sort();
            names
        }

        fn notes(&self) -> usize {
            fs::read_dir(self.vault.join("Meetings")).map_or(0, |d| d.count())
        }

        fn requests(&self) -> Vec<String> {
            self.requests.try_iter().collect()
        }

        /// Turns the gate on and writes `context` beside the recording.
        fn with_context(&mut self, context: &Value) {
            self.pipeline.context = true;
            self.write_context(context);
        }

        fn write_context(&self, context: &Value) {
            fs::write(self.file("context.json"), context.to_string()).unwrap();
        }

        fn with_desktop(&mut self) {
            self.pipeline.desktop = true;
        }

        fn store(&self) -> Store {
            Store::open(&self.pipeline.store_path).unwrap()
        }

        /// The recording's row in the notes store.
        fn row(&self) -> Row {
            self.find_row().expect("the recording has a row")
        }

        fn find_row(&self) -> Option<Row> {
            let recording = self.file("recording.webm");
            self.store().get_by_path(&recording).unwrap()
        }

        fn all_jobs_done(&self) -> bool {
            let jobs = self.pipeline.jobs.list(10).unwrap();
            jobs.iter().all(|j| j.stage == Stage::Done)
        }

        fn note(&self) -> String {
            let mut notes = fs::read_dir(self.vault.join("Meetings")).unwrap();
            let note = notes.next().unwrap().unwrap().path();
            assert!(notes.next().is_none());
            fs::read_to_string(note).unwrap()
        }

        /// What `claude` got: its arguments and its stdin.
        fn sent(&self) -> (String, String) {
            let read = |name| fs::read_to_string(self.tools.join(name)).unwrap();
            (read("args.txt"), read("stdin.txt"))
        }

        /// A done run with the gate off and no context: what `claude` got and the note.
        fn baseline(&self) -> ((String, String), String) {
            done(self.run(Some(Language::En), None).0);
            (self.sent(), self.note())
        }
    }

    fn done(ending: Ending) -> String {
        match ending {
            Ending::Done { title } => title,
            Ending::Failed { stage, error } => panic!("failed at {stage:?}: {error:#}"),
        }
    }

    fn failed(ending: Ending) -> (Stage, String) {
        match ending {
            Ending::Failed { stage, error } => (stage, format!("{error:#}")),
            Ending::Done { title } => panic!("expected a failure, got done: {title}"),
        }
    }

    #[test]
    fn an_imported_m4a_runs_to_done() {
        let env = setup("pipe-import", vec![(200, EN)]);
        let source = env.rec.parent().unwrap().join("call.m4a");
        fs::copy(M4A, &source).unwrap();
        let path = crate::import::import(
            &env.rec,
            &source,
            Some("2026-10-18 09:30"),
            crate::import::local_minute,
        )
        .unwrap();
        let mut out = Vec::new();
        let ending = env
            .pipeline
            .run(&path, false, Some(Language::En), None, &mut out);
        assert_eq!(done(ending), "Standup");

        let folder = env.rec.join("meeting-2026-10-18_0930");
        assert_eq!(path, folder.join("recording.m4a"));
        let mut names: Vec<String> = fs::read_dir(&folder)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(
            names,
            ["meta.json", "recording.m4a", "summary.md", "transcript.md"]
        );
        assert_eq!(env.job().stage, Stage::Done);
        assert_eq!(
            Meta::load(&folder.join("meta.json"))
                .unwrap()
                .unwrap()
                .recorded_at,
            "2026-10-18T09:30"
        );
        assert_eq!(env.notes(), 1);
        assert_eq!(fs::read(&source).unwrap(), fs::read(M4A).unwrap());
    }

    #[test]
    fn success_leaves_the_outputs_and_deletes_the_intermediates() {
        let env = setup("pipe-ok", vec![(200, EN)]);
        let (ending, out) = env.run(Some(Language::En), None);
        assert_eq!(done(ending), "Standup");

        assert_eq!(
            env.names(),
            ["meta.json", "recording.webm", "summary.md", "transcript.md",]
        );
        assert_eq!(env.notes(), 1);
        assert_eq!(env.requests().len(), 1);

        let job = env.job();
        assert_eq!(job.stage, Stage::Done);
        assert_eq!(job.title.as_deref(), Some("Standup"));
        assert_eq!(job.summary_path, Some(env.file("summary.md")));
        let meta = env.meta();
        assert_eq!(meta.language, Language::En);
        assert_eq!(meta.recorded_at, "2026-10-19T14:30");
        assert!(meta.duration_s.is_some());
        assert_eq!(meta.title.as_deref(), Some("Standup"));
        assert_eq!(meta.speakers.len(), 1);
        assert_eq!(meta.runs.len(), 1);
        assert_eq!(meta.runs[0].job_id, job.job_id);
        assert_eq!(meta.runs[0].stage, Stage::Done);
        assert!(meta.runs[0].ended_at.is_some());

        assert_eq!(
            out,
            format!(
                "extracting\ntranscribing\nsummarising\nwriting\ndone: {}\n",
                env.file("summary.md").display()
            )
        );
    }

    fn assert_failed(env: &Env, stage: Stage, reason: &str) {
        let job = env.job();
        assert_eq!(job.stage, Stage::Failed);
        let error = job.error.unwrap();
        assert!(
            error.starts_with(&format!("{}: ", stage.name())) && error.contains(reason),
            "{error}"
        );
        assert!(!env.file("lock").exists());
        let meta = env.meta();
        assert_eq!(meta.runs[0].stage, Stage::Failed);
        assert!(meta.runs[0].ended_at.is_some());
    }

    #[test]
    fn ffmpeg_failure_fails_at_extracting() {
        let mut env = setup("pipe-extract", vec![]);
        env.pipeline.config.ffmpeg_path = "/usr/bin/false".into();
        let (stage, _) = failed(env.run(Some(Language::En), None).0);
        assert_eq!(stage, Stage::Extracting);
        assert_failed(&env, Stage::Extracting, "");
        assert!(!env.file("audio.ogg").exists());
    }

    #[test]
    fn deepgram_401_fails_at_transcribing_and_keeps_the_audio() {
        let env = setup("pipe-401", vec![(401, "{}")]);
        let (stage, _) = failed(env.run(Some(Language::En), None).0);
        assert_eq!(stage, Stage::Transcribing);
        assert_failed(&env, Stage::Transcribing, "rejected the API key");
        assert!(env.file("audio.ogg").exists());
        assert!(!env.file("deepgram.json").exists());
    }

    #[test]
    fn claude_failure_fails_at_summarising_and_keeps_audio_and_raw() {
        let env = setup("pipe-claude", vec![(200, EN)]);
        stub(&env.tools, "exit 1");
        let (stage, _) = failed(env.run(Some(Language::En), None).0);
        assert_eq!(stage, Stage::Summarising);
        assert_failed(&env, Stage::Summarising, "");
        assert!(env.file("audio.ogg").exists());
        assert!(env.file("deepgram.json").exists());
        assert_eq!(env.notes(), 0);
    }

    #[test]
    fn missing_vault_fails_at_writing() {
        let mut env = setup("pipe-vault", vec![(200, EN)]);
        env.pipeline.config.vault_dir = env.vault.join("missing");
        let (stage, _) = failed(env.run(Some(Language::En), None).0);
        assert_eq!(stage, Stage::Writing);
        assert_failed(&env, Stage::Writing, "does not exist");
        assert!(env.file("audio.ogg").exists());
        assert!(env.file("deepgram.json").exists());
    }

    #[test]
    fn a_rerun_after_a_summarising_failure_resumes_without_deepgram_or_ffmpeg() {
        let mut env = setup("pipe-resume", vec![(200, EN)]);
        stub(&env.tools, "exit 1");
        failed(env.run(Some(Language::En), None).0);
        assert_eq!(env.requests().len(), 1);

        stub(
            &env.tools,
            &format!("cat <<'EOF'\n{}\nEOF", envelope(VALID)),
        );
        env.pipeline.config.ffmpeg_path = "/usr/bin/false".into();
        done(env.run(None, None).0);
        assert!(env.requests().is_empty());
        assert!(!env.file("audio.ogg").exists());
        assert!(!env.file("deepgram.json").exists());
        assert_eq!(env.notes(), 1);
        assert_eq!(env.meta().runs.len(), 2);
    }

    #[test]
    fn a_rerun_on_done_rewrites_the_summary_without_deepgram() {
        let env = setup("pipe-rerun", vec![(200, EN)]);
        done(env.run(Some(Language::En), None).0);
        env.requests();
        fs::remove_file(env.file("summary.md")).unwrap();

        done(env.run(None, None).0);
        assert!(env.requests().is_empty());
        assert!(env.file("summary.md").exists());
        assert_eq!(env.notes(), 1);
    }

    #[test]
    fn language_override_transcribes_again_and_stores_the_language() {
        let env = setup("pipe-lang", vec![(200, EN), (200, EN)]);
        done(env.run(Some(Language::Bn), None).0);
        assert_eq!(env.meta().language, Language::Bn);
        env.requests();

        done(env.run(Some(Language::En), None).0);
        let requests = env.requests();
        assert_eq!(requests.len(), 1);
        assert!(requests[0].contains("language=en"));
        assert_eq!(env.meta().language, Language::En);
    }

    #[test]
    fn an_empty_transcript_is_done_without_claude_or_a_note() {
        let env = setup("pipe-empty", vec![(200, EMPTY)]);
        let (ending, out) = env.run(Some(Language::En), None);
        assert_eq!(done(ending), NO_SPEECH);

        let job = env.job();
        assert_eq!(job.stage, Stage::Done);
        assert_eq!(job.title.as_deref(), Some(NO_SPEECH));
        assert_eq!(job.summary_path, None);
        assert!(
            fs::read_to_string(env.file("transcript.md"))
                .unwrap()
                .contains(NO_SPEECH)
        );
        assert!(!env.file("summary.md").exists());
        assert!(!env.tools.join("args.txt").exists());
        assert_eq!(env.notes(), 0);
        assert_eq!(env.meta().title, None);
        assert!(out.ends_with(&format!("done: {NO_SPEECH}\n")));
    }

    #[test]
    fn a_held_lock_refuses_the_run_and_stays() {
        let env = setup("pipe-lock", vec![]);
        let pid = std::process::id().to_string();
        fs::write(env.file("lock"), &pid).unwrap();
        let (stage, error) = failed(env.run(Some(Language::En), None).0);
        assert_eq!(stage, Stage::Queued);
        assert!(error.contains("already processing"), "{error}");
        assert_eq!(fs::read_to_string(env.file("lock")).unwrap(), pid);
        assert!(!env.file("meta.json").exists());
    }

    #[test]
    fn no_language_anywhere_fails_with_the_plan_error() {
        let env = setup("pipe-nolang", vec![]);
        let (_, error) = failed(env.run(None, None).0);
        assert!(error.contains("no language stored for this recording; pass --language bn|en"));
        assert!(env.job().error.unwrap().contains("--language bn|en"));
        assert!(!env.file("meta.json").exists());
        assert!(!env.file("lock").exists());
    }

    #[test]
    fn a_missing_recording_fails_without_a_lock() {
        let env = setup("pipe-missing", vec![]);
        fs::remove_file(env.file("recording.webm")).unwrap();
        let (stage, error) = failed(env.run(Some(Language::En), None).0);
        assert_eq!(stage, Stage::Queued);
        assert!(error.contains("No such file"), "{error}");
        assert!(env.pipeline.jobs.list(10).unwrap().is_empty());
    }

    #[test]
    fn a_flat_recording_is_refused_without_a_job() {
        let env = setup("pipe-flat", vec![]);
        let flat = env.rec.join(format!("{NAME}.webm"));
        fs::copy(env.file("recording.webm"), &flat).unwrap();
        let mut out = Vec::new();
        let ending = env
            .pipeline
            .run(&flat, false, Some(Language::En), None, &mut out);
        let (stage, error) = failed(ending);
        assert_eq!(stage, Stage::Queued);
        assert!(error.contains("is not in its own folder"), "{error}");
        assert!(env.pipeline.jobs.list(10).unwrap().is_empty());
    }

    #[test]
    fn an_archived_recording_is_processed() {
        let env = setup("pipe-archived", vec![(200, EN)]);
        let archived = env.rec.join("Archive").join(NAME);
        fs::create_dir_all(env.rec.join("Archive")).unwrap();
        fs::rename(env.rec.join(NAME), &archived).unwrap();
        let mut out = Vec::new();
        let ending = env.pipeline.run(
            &archived.join("recording.webm"),
            false,
            Some(Language::En),
            None,
            &mut out,
        );
        assert_eq!(done(ending), "Standup");
    }

    #[test]
    fn a_host_job_is_updated_in_place_and_keeps_started_at() {
        let env = setup("pipe-hostjob", vec![(200, EN)]);
        let queued = Job {
            job_id: "host-1".into(),
            recording: env.file("recording.webm"),
            title: None,
            stage: Stage::Queued,
            error: None,
            started_at: "2026-10-19T14:30:00Z".into(),
            updated_at: "2026-10-19T14:30:00Z".into(),
            summary_path: None,
        };
        env.pipeline.jobs.write(&queued).unwrap();

        done(env.run(Some(Language::En), Some("host-1")).0);
        let job = env.job();
        assert_eq!(job.job_id, "host-1");
        assert_eq!(job.started_at, "2026-10-19T14:30:00Z");
        assert_eq!(job.stage, Stage::Done);
        assert_eq!(env.meta().runs[0].started_at, "2026-10-19T14:30:00Z");
    }

    #[test]
    fn a_matched_invite_reaches_claude_and_the_note() {
        let mut env = setup("pipe-cal-match", vec![(200, EN)]);
        env.with_context(&meet());
        let _calendar = serve_calendar(vec![ok(GOOGLE)]);
        done(env.run(Some(Language::En), None).0);

        let (_, stdin) = env.sent();
        assert!(stdin.contains("BEGIN MEETING CONTEXT"), "{stdin}");
        assert!(stdin.contains("Plans & goals"), "{stdin}");
        let note = env.note();
        assert!(note.contains("calendar: \"matched\""), "{note}");
        assert!(
            note.contains("meeting_title: \"Weekly sync, team; all\""),
            "{note}"
        );
        assert!(note.contains("\"Doe, John\""), "{note}");
        assert!(!note.contains("Room 4"), "{note}");
        assert_eq!(reads(), 1);
    }

    #[test]
    fn no_matching_event_records_no_match_and_keeps_the_meet_block() {
        let mut env = setup("pipe-cal-nomatch", vec![(200, EN)]);
        let mut context = meet();
        context["meeting_id"] = json!("xyz-abcd-efg");
        env.with_context(&context);
        let _calendar = serve_calendar(vec![ok(GOOGLE)]);
        done(env.run(Some(Language::En), None).0);

        let (_, stdin) = env.sent();
        assert!(
            stdin.contains("Meet title") && !stdin.contains("Plans"),
            "{stdin}"
        );
        let note = env.note();
        assert!(note.contains("calendar: \"no_match\""), "{note}");
        assert!(note.contains("meeting_title: \"Meet title\""), "{note}");
    }

    #[test]
    fn every_calendar_failure_records_fetch_failed_and_finishes() {
        let status =
            |code| format!("HTTP/1.1 {code} X\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        let redirect = "HTTP/1.1 302 Found\r\nLocation: {base}/next.ics\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
        let over_cap = format!("BEGIN:VCALENDAR\r\n{}", "X".repeat(10 * 1024));
        let cases: Vec<(&str, Option<Vec<String>>)> = vec![
            ("non-2xx", Some(vec![status(404)])),
            ("not iCalendar", Some(vec![ok("<html>sign in</html>")])),
            (
                "parse error",
                Some(vec![ok("BEGIN:VCALENDAR\r\nVERSION:2.0\r\n")]),
            ),
            ("four redirects", Some(vec![redirect.into(); 4])),
            ("over the cap", Some(vec![ok(&over_cap)])),
            ("stalled", None),
        ];
        for (case, responses) in cases {
            let mut env = setup(
                &format!("pipe-cal-{}", case.replace(' ', "-")),
                vec![(200, EN)],
            );
            env.with_context(&meet());
            let calendar = match responses {
                Some(responses) => Some(serve_calendar(responses)),
                None => {
                    ADDRESS.set(Some(format!("{}/basic.ics", stall())));
                    None
                }
            };
            done(env.run(Some(Language::En), None).0);
            assert_eq!(env.job().stage, Stage::Done, "{case}");
            let note = env.note();
            assert!(
                note.contains("calendar: \"fetch_failed\""),
                "{case}: {note}"
            );
            assert!(
                note.contains("meeting_title: \"Meet title\""),
                "{case}: {note}"
            );
            if let Some(calendar) = calendar {
                assert!(calendar.try_iter().next().is_some(), "{case}: not fetched");
            }
        }
    }

    #[test]
    fn a_keychain_error_records_fetch_failed_and_finishes() {
        let mut env = setup("pipe-cal-keychain", vec![(200, EN)]);
        env.with_context(&meet());
        env.pipeline.calendar = locked_keychain;
        done(env.run(Some(Language::En), None).0);
        assert!(env.note().contains("calendar: \"fetch_failed\""));
        assert_eq!(reads(), 1);
    }

    #[test]
    fn no_stored_address_fetches_nothing_and_records_no_outcome() {
        let mut env = setup("pipe-cal-unset", vec![(200, EN)]);
        env.with_context(&meet());
        done(env.run(Some(Language::En), None).0);
        assert!(!env.note().contains("calendar:"));
        assert_eq!(reads(), 1);
    }

    #[test]
    fn without_context_or_gate_nothing_changes_and_the_keychain_is_not_read() {
        let mut env = setup("pipe-cal-off", vec![(200, EN)]);
        let _calendar = serve_calendar(vec![ok(GOOGLE)]);
        let baseline = env.baseline();

        // Gate on, no `context.json` (older recordings, imports).
        env.pipeline.context = true;
        done(env.run(None, None).0);
        assert_eq!((env.sent(), env.note()), baseline);

        // Gate off, `context.json` present.
        env.with_context(&meet());
        env.pipeline.context = false;
        done(env.run(None, None).0);
        assert_eq!((env.sent(), env.note()), baseline);

        // Gate on, `context.json` that fails a rule.
        let mut bad = meet();
        bad["version"] = json!(2);
        env.with_context(&bad);
        done(env.run(None, None).0);
        assert_eq!((env.sent(), env.note()), baseline);
        assert_eq!(reads(), 0);
    }

    #[test]
    fn a_context_without_a_meeting_id_never_reads_the_keychain() {
        let mut env = setup("pipe-cal-teams", vec![(200, EN)]);
        let calendar = serve_calendar(vec![ok(GOOGLE)]);
        let (baseline, _) = env.baseline();
        env.with_context(&json!({"version": 1, "platform": "teams",
            "started_at_ms": 1_792_587_600_000_i64, "utc_offset_min": -240,
            "participants": [{"name": "Rahim", "self": false}]}));
        done(env.run(None, None).0);

        assert_eq!(reads(), 0);
        assert!(calendar.try_iter().next().is_none());
        let (_, stdin) = env.sent();
        assert_ne!(stdin, baseline.1);
        assert!(stdin.contains("Rahim"), "{stdin}");
        assert!(!env.note().contains("calendar:"));
    }

    #[test]
    fn a_zoom_call_without_a_match_sends_what_it_sent_before() {
        let mut env = setup("pipe-cal-zoom", vec![(200, EN)]);
        let _calendar = serve_calendar(vec![ok(GOOGLE)]);
        let (baseline, _) = env.baseline();
        env.with_context(
            &json!({"version": 1, "platform": "zoom", "meeting_id": "123456789",
            "started_at_ms": 1_792_587_600_000_i64, "utc_offset_min": -240}),
        );
        done(env.run(None, None).0);

        assert_eq!(env.sent(), baseline);
        assert!(env.note().contains("calendar: \"no_match\""));
    }

    #[test]
    fn a_rerun_fetches_the_calendar_again() {
        let mut env = setup("pipe-cal-rerun", vec![(200, EN)]);
        env.with_context(&meet());
        let renamed = GOOGLE.replace("SUMMARY:Weekly", "SUMMARY:Renamed");
        assert_ne!(renamed, GOOGLE);
        let calendar = serve_calendar(vec![ok(GOOGLE), ok(&renamed)]);
        done(env.run(Some(Language::En), None).0);
        assert!(
            env.note()
                .contains("meeting_title: \"Weekly sync, team; all\"")
        );

        done(env.run(None, None).0);
        let note = env.note();
        assert!(
            note.contains("meeting_title: \"Renamed sync, team; all\""),
            "{note}"
        );
        assert!(note.contains("calendar: \"matched\""), "{note}");
        assert_eq!(reads(), 2);
        assert_eq!(calendar.try_iter().count(), 2);
    }

    const CALENDAR_NAME: &str = "Zebra calendar";

    /// A desktop recording started from a calendar event.
    fn desktop_event() -> Value {
        json!({"version": 1, "platform": "desktop", "url": null, "meeting_id": null,
            "tab_title": null, "started_at_ms": 1_792_587_600_000_i64, "utc_offset_min": 360,
            "participants": [], "speaking": [],
            "calendar_event": {"occurrence_id": "occ-1", "series_id": "ser-1",
                "title": "Quarterly review", "start": "2026-10-19T14:30:00+06:00",
                "end": "2026-10-19T15:00:00+06:00", "organizer": "Karim",
                "calendar": CALENDAR_NAME,
                "attendees": [{"name": "Rahim", "response": "accepted"},
                    {"name": "Nadia", "response": "declined"}],
                "description": "Bring the roadmap"}})
    }

    fn event_link() -> EventLink {
        EventLink {
            occurrence_id: "occ-1".into(),
            series_id: "ser-1".into(),
            title: "Quarterly review".into(),
            start: "2026-10-19T14:30:00+06:00".into(),
            end: "2026-10-19T15:00:00+06:00".into(),
            calendar: Some(CALENDAR_NAME.into()),
        }
    }

    /// A stub `claude` answering with another title and summary.
    fn stub_changed(env: &Env) {
        let changed = VALID
            .replace("Standup", "Changed")
            .replace("We met.", "We spoke.");
        assert_ne!(changed, VALID);
        stub(
            &env.tools,
            &format!("cat <<'EOF'\n{}\nEOF", envelope(&changed)),
        );
    }

    #[test]
    fn desktop_run_writes_a_done_row_with_body_without_the_title() {
        let mut env = setup("pipe-store-done", vec![(200, EN)]);
        env.with_desktop();
        let (ending, out) = env.run(Some(Language::En), None);
        assert_eq!(done(ending), "Standup");

        let row = env.row();
        assert_eq!(row.status, Status::Done);
        assert_eq!((row.failed_stage, row.error), (None, None));
        assert_eq!(row.title.as_deref(), Some("Standup"));
        let summary = fs::read_to_string(env.file("summary.md")).unwrap();
        let body = row.body_md.unwrap();
        assert!(body.starts_with("We met.\n"), "{body}");
        assert_eq!(format!("# Standup\n\n{body}"), summary);
        assert_eq!(
            row.speakers,
            [SpeakerRow {
                label: "Speaker 1".into(),
                name: Some("Rahim".into()),
                evidence: "Thanks, Rahim.".into(),
                name_edited: false,
            }]
        );
        assert_eq!(row.language, Some(Language::En));
        assert_eq!(row.duration_s, Some(13));
        assert_eq!(
            row.transcript.as_deref(),
            Some(
                "[00:00:01] Speaker 0: Hi, thanks for joining.\n[00:00:04] Speaker 1: Thanks, Rahim.\n"
            )
        );
        assert_eq!(row.source, Source::Browser);
        assert_eq!(row.recorded_at, "2026-10-19T14:30");
        assert_eq!(row.event, None);
        assert_eq!(row.my_notes, "");
        let note = env.vault.join("Meetings/2026-10-19 1430 Standup.md");
        assert_eq!(row.vault_note_path, Some(note));
        assert_eq!(env.store().paths().unwrap().len(), 1);
        // Nothing but the stage lines reaches stdout.
        assert_eq!(
            out,
            format!(
                "extracting\ntranscribing\nsummarising\nwriting\ndone: {}\n",
                env.file("summary.md").display()
            )
        );
    }

    #[test]
    fn source_follows_extension_then_context_platform() {
        let desktop: context::Context =
            serde_json::from_value(json!({"version": 1, "platform": "desktop",
                "started_at_ms": 0, "utc_offset_min": 0}))
            .unwrap();
        let other = context::Context {
            platform: context::Platform::Other,
            ..desktop.clone()
        };
        let source = |name: &str, ctx| source_of(Path::new(name), ctx);
        assert_eq!(source("/r/m/recording.webm", None), Source::Browser);
        assert_eq!(
            source("/r/m/recording.webm", Some(&desktop)),
            Source::Browser
        );
        assert_eq!(
            source("/r/m/recording.m4a", Some(&desktop)),
            Source::Desktop
        );
        assert_eq!(source("/r/m/recording.m4a", Some(&other)), Source::Import);
        assert_eq!(source("/r/m/recording.m4a", None), Source::Import);

        // End to end, with the context flag off: an imported file beside a desktop context.
        let mut env = setup("pipe-store-source", vec![(200, EN)]);
        env.with_desktop();
        let source = env.rec.parent().unwrap().join("call.m4a");
        fs::copy(M4A, &source).unwrap();
        let path = crate::import::import(
            &env.rec,
            &source,
            Some("2026-10-18 09:30"),
            crate::import::local_minute,
        )
        .unwrap();
        fs::write(
            path.with_file_name("context.json"),
            desktop_event().to_string(),
        )
        .unwrap();
        let mut out = Vec::new();
        done(
            env.pipeline
                .run(&path, false, Some(Language::En), None, &mut out),
        );
        let row = env.store().get_by_path(&path).unwrap().unwrap();
        assert_eq!(row.source, Source::Desktop);
        assert_eq!(row.recorded_at, "2026-10-18T09:30");
    }

    #[test]
    fn the_row_is_processing_while_claude_runs() {
        let mut env = setup("pipe-store-inflight", vec![(200, EN)]);
        env.with_desktop();
        // An earlier failure, so the test sees the run clear it.
        stub(&env.tools, "exit 1");
        failed(env.run(Some(Language::En), None).0);
        assert_eq!(env.row().status, Status::Failed);
        fs::remove_file(env.tools.join("stdin.txt")).unwrap();

        let go = env.tools.join("go");
        stub(
            &env.tools,
            &format!(
                "while [ ! -f \"{}\" ]; do sleep 0.05; done\ncat <<'EOF'\n{}\nEOF",
                go.display(),
                envelope(VALID)
            ),
        );
        let (pipeline, recording) = (&env.pipeline, env.file("recording.webm"));
        let (ending, during) = std::thread::scope(|scope| {
            let run = scope.spawn(|| {
                let mut out = Vec::new();
                pipeline.run(&recording, false, None, None, &mut out)
            });
            let deadline = Instant::now() + Duration::from_secs(5);
            while !env.tools.join("stdin.txt").exists() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(20));
            }
            let during = env.find_row();
            // Released before any assertion, so a failure cannot leave the run blocked.
            fs::write(&go, "").unwrap();
            (run.join().unwrap(), during)
        });
        let during = during.expect("a row while the run is in flight");
        assert_eq!(during.status, Status::Processing);
        assert_eq!((during.failed_stage, during.error), (None, None));
        done(ending);
        assert_eq!(env.row().status, Status::Done);
    }

    #[test]
    fn no_speech_row_has_the_title_and_no_body() {
        let mut env = setup("pipe-store-empty", vec![(200, EMPTY)]);
        env.with_desktop();
        assert_eq!(done(env.run(Some(Language::En), None).0), NO_SPEECH);
        let row = env.row();
        assert_eq!(row.status, Status::NoSpeech);
        assert_eq!(row.title.as_deref(), Some(NO_SPEECH));
        assert_eq!(row.body_md, None);
        assert_eq!(row.transcript.as_deref(), Some(""));
        assert_eq!(row.language, Some(Language::En));
        assert_eq!(row.vault_note_path, None);
        assert_eq!(env.notes(), 0);
    }

    #[test]
    fn a_failed_run_records_stage_and_reason_and_keeps_earlier_content() {
        let mut env = setup("pipe-store-failed", vec![(200, EN)]);
        env.with_desktop();
        done(env.run(Some(Language::En), None).0);
        let first = env.row();

        stub(&env.tools, "exit 1");
        let (stage, error) = failed(env.run(None, None).0);
        assert_eq!(stage, Stage::Summarising);
        let row = env.row();
        assert_eq!(row.status, Status::Failed);
        assert_eq!(row.failed_stage.as_deref(), Some("summarising"));
        let reason = row.error.clone().unwrap();
        assert_eq!(Some(reason.as_str()), error.lines().next());
        assert!(
            !reason.is_empty() && !reason.starts_with("summarising"),
            "{reason}"
        );
        assert_eq!(
            (row.title, row.body_md, row.speakers, row.transcript),
            (first.title, first.body_md, first.speakers, first.transcript)
        );
        assert_eq!(row.vault_note_path, first.vault_note_path);
    }

    #[test]
    fn each_failing_stage_is_recorded() {
        let mut env = setup("pipe-store-extract", vec![]);
        env.with_desktop();
        env.pipeline.config.ffmpeg_path = "/usr/bin/false".into();
        failed(env.run(Some(Language::En), None).0);
        let row = env.row();
        assert_eq!(row.status, Status::Failed);
        assert_eq!(row.failed_stage.as_deref(), Some("extracting"));
        assert_eq!((row.title, row.body_md), (None, None));

        let mut env = setup("pipe-store-vault", vec![(200, EN)]);
        env.with_desktop();
        env.pipeline.config.vault_dir = env.vault.join("missing");
        failed(env.run(Some(Language::En), None).0);
        let row = env.row();
        assert_eq!(row.status, Status::Failed);
        assert_eq!(row.failed_stage.as_deref(), Some("writing"));
        assert!(row.error.unwrap().contains("does not exist"));
        assert_eq!(row.vault_note_path, None);

        // Before any stage, once the lock is held: no language on a first run.
        let mut env = setup("pipe-store-nolang", vec![]);
        env.with_desktop();
        failed(env.run(None, None).0);
        let row = env.row();
        assert_eq!(row.status, Status::Failed);
        assert_eq!(row.failed_stage.as_deref(), Some("queued"));
        assert_eq!(row.language, None);
    }

    #[test]
    fn a_held_lock_leaves_the_row_alone() {
        let mut env = setup("pipe-store-lock", vec![(200, EN)]);
        env.with_desktop();
        done(env.run(Some(Language::En), None).0);
        let before = env.row();

        fs::write(env.file("lock"), std::process::id().to_string()).unwrap();
        let (_, error) = failed(env.run(None, None).0);
        assert!(error.contains("already processing"), "{error}");
        assert_eq!(env.row(), before);
    }

    #[test]
    fn a_bad_path_writes_no_row() {
        let mut env = setup("pipe-store-flat", vec![]);
        env.with_desktop();
        let flat = env.rec.join(format!("{NAME}.webm"));
        fs::copy(env.file("recording.webm"), &flat).unwrap();
        let mut out = Vec::new();
        failed(
            env.pipeline
                .run(&flat, false, Some(Language::En), None, &mut out),
        );
        fs::remove_file(env.file("recording.webm")).unwrap();
        failed(env.run(Some(Language::En), None).0);
        assert!(env.store().paths().unwrap().is_empty());
    }

    #[test]
    fn a_cli_rerun_keeps_edits_and_my_notes() {
        let mut env = setup("pipe-store-edits", vec![(200, EN)]);
        env.with_desktop();
        done(env.run(Some(Language::En), None).0);
        let id = env.row().id;
        let store = env.store();
        for edit in [
            Edit::Title("My title".into()),
            Edit::MyNotes("keep".into()),
            Edit::Speaker {
                label: "Speaker 1".into(),
                name: "Rahim Uddin".into(),
            },
        ] {
            store.edit(id, edit).unwrap();
        }
        drop(store);

        stub_changed(&env);
        assert_eq!(done(env.run(None, None).0), "My title");

        let row = env.row();
        assert_eq!(row.title.as_deref(), Some("My title"));
        assert_eq!(row.my_notes, "keep");
        assert_eq!(row.speakers[0].name.as_deref(), Some("Rahim Uddin"));
        let body = row.body_md.unwrap();
        assert!(body.starts_with("We spoke.\n"), "{body}");
        // D35: the rename is applied again to Claude's fresh text.
        assert!(
            body.contains("Rahim Uddin") && !body.contains("(inferred)"),
            "{body}"
        );

        let note_path = env.vault.join("Meetings/2026-10-19 1430 My title.md");
        assert_eq!(row.vault_note_path, Some(note_path.clone()));
        assert_eq!(env.notes(), 1);
        let note = fs::read_to_string(note_path).unwrap();
        assert!(note.starts_with("---\ndate: 2026-10-19\n"), "{note}");
        assert!(note.contains("speakers: [\"Rahim Uddin\"]\n"), "{note}");
        assert!(
            note.contains(&format!("---\n\n# My title\n\n{body}")),
            "{note}"
        );
        assert!(note.ends_with("\n\n## My notes\n\nkeep\n"), "{note}");

        // The folder keeps Claude's unedited output.
        let summary = fs::read_to_string(env.file("summary.md")).unwrap();
        assert!(summary.starts_with("# Changed\n\nWe spoke.\n"), "{summary}");
        assert!(summary.contains("Rahim (inferred)"), "{summary}");
        assert_eq!(env.meta().title.as_deref(), Some("Changed"));
        assert_eq!(env.meta().speakers[0].name.as_deref(), Some("Rahim"));
    }

    #[test]
    fn replace_after_clear_edits_takes_claudes_title() {
        let mut env = setup("pipe-store-replace", vec![(200, EN)]);
        env.with_desktop();
        done(env.run(Some(Language::En), None).0);
        let id = env.row().id;
        let store = env.store();
        store.edit(id, Edit::Title("My title".into())).unwrap();
        store.edit(id, Edit::MyNotes("keep".into())).unwrap();
        store.clear_edits(id).unwrap();
        drop(store);

        stub_changed(&env);
        assert_eq!(done(env.run(None, None).0), "Changed");
        let row = env.row();
        assert_eq!(row.title.as_deref(), Some("Changed"));
        assert_eq!(row.my_notes, "keep");
        assert_eq!(env.notes(), 1);
        let note = env.note();
        assert!(note.contains("\n# Changed\n\nWe spoke.\n"), "{note}");
        assert!(note.ends_with("\n\n## My notes\n\nkeep\n"), "{note}");
    }

    #[test]
    fn desktop_on_without_edits_writes_the_baseline_note() {
        let mut env = setup("pipe-store-identical", vec![(200, EN)]);
        let baseline = env.baseline();
        assert!(!env.pipeline.store_path.parent().unwrap().exists());

        env.with_desktop();
        done(env.run(None, None).0);
        assert_eq!((env.sent(), env.note()), baseline);
        assert_eq!(env.notes(), 1);
        assert_eq!(env.row().status, Status::Done);
    }

    #[test]
    fn a_desktop_context_with_the_desktop_flag_off_changes_nothing() {
        let mut env = setup("pipe-store-off", vec![(200, EN)]);
        let _calendar = serve_calendar(vec![ok(GOOGLE)]);
        let baseline = env.baseline();

        // Both flags off.
        env.write_context(&desktop_event());
        done(env.run(None, None).0);
        assert_eq!((env.sent(), env.note()), baseline);

        // Context flag on: the event is not a match, and a cut event is not announced.
        let mut cut = desktop_event();
        cut["calendar_event"]["title"] = json!("t".repeat(301));
        for context in [desktop_event(), cut] {
            env.with_context(&context);
            done(env.run(None, None).0);
            assert_eq!((env.sent(), env.note()), baseline);
        }
        assert_eq!(reads(), 0);
        assert!(!env.pipeline.store_path.parent().unwrap().exists());
    }

    #[test]
    fn an_unopenable_store_never_fails_the_run() {
        let mut env = setup("pipe-store-unopenable", vec![(200, EN)]);
        let baseline = env.baseline();
        let blocker = env.rec.parent().unwrap().join("blocker");
        fs::write(&blocker, "a file where the directory should be").unwrap();
        env.pipeline.store_path = blocker.join("notes.sqlite");
        env.with_desktop();

        done(env.run(None, None).0);
        assert!(env.all_jobs_done());
        assert_eq!((env.sent(), env.note()), baseline);
    }

    #[test]
    fn a_store_that_rejects_writes_never_fails_the_run() {
        let mut env = setup("pipe-store-rejects", vec![(200, EN)]);
        let baseline = env.baseline();
        env.with_desktop();
        drop(env.store());
        rusqlite::Connection::open(&env.pipeline.store_path)
            .unwrap()
            .execute_batch("DROP TABLE meetings")
            .unwrap();

        done(env.run(None, None).0);
        assert!(env.all_jobs_done());
        assert_eq!((env.sent(), env.note()), baseline);

        // A failing run still ends as a failure of its own stage, not of the store.
        stub(&env.tools, "exit 1");
        assert_eq!(failed(env.run(None, None).0).0, Stage::Summarising);
    }

    #[test]
    fn a_calendar_event_is_the_match_without_a_fetch() {
        let mut env = setup("pipe-event-match", vec![(200, EN)]);
        env.with_desktop();
        env.with_context(&desktop_event());
        let calendar = serve_calendar(vec![ok(GOOGLE)]);
        done(env.run(Some(Language::En), None).0);

        assert_eq!(reads(), 0);
        assert!(calendar.try_iter().next().is_none());
        let (_, stdin) = env.sent();
        for line in [
            "platform: desktop\n",
            "calendar_title: \"Quarterly review\"\n",
            "organizer: \"Karim\"\n",
            "- \"Rahim\" accepted\n",
            "- \"Nadia\" declined\n",
            "<<<DESCRIPTION\nBring the roadmap\nDESCRIPTION>>>\n",
        ] {
            assert!(stdin.contains(line), "{line:?} missing from {stdin}");
        }
        let note = env.note();
        assert!(note.contains("calendar: \"matched\"\n"), "{note}");
        assert!(
            note.contains("meeting_title: \"Quarterly review\"\n"),
            "{note}"
        );
        assert!(note.contains("attendees: [\"Rahim\"]\n"), "{note}");

        // D31: the calendar's name is stored and goes nowhere else.
        assert_eq!(env.row().event, Some(event_link()));
        assert!(!stdin.contains(CALENDAR_NAME) && !note.contains(CALENDAR_NAME));
    }

    #[test]
    fn the_event_link_is_stored_even_with_the_context_flag_off() {
        let mut env = setup("pipe-event-link", vec![(200, EN)]);
        let baseline = env.baseline();
        env.with_desktop();
        env.write_context(&desktop_event());
        done(env.run(None, None).0);

        assert_eq!(env.row().event, Some(event_link()));
        assert_eq!((env.sent(), env.note()), baseline);
        assert_eq!(reads(), 0);
    }

    #[test]
    fn a_rejected_calendar_event_runs_without_it() {
        let mut env = setup("pipe-event-rejected", vec![(200, EN)]);
        env.with_desktop();
        let mut context = desktop_event();
        context["calendar_event"]["series_id"] = json!("x".repeat(256));
        env.with_context(&context);
        done(env.run(Some(Language::En), None).0);

        let row = env.row();
        assert_eq!((row.status, row.event), (Status::Done, None));
        assert!(!env.note().contains("calendar:"));
    }

    #[test]
    fn an_event_wins_over_a_meeting_id_and_a_cut_one_is_announced() {
        let mut env = setup("pipe-event-meet", vec![(200, EN)]);
        env.with_desktop();
        let mut context = meet();
        context["calendar_event"] = desktop_event()["calendar_event"].clone();
        context["calendar_event"]["description"] = json!("d".repeat(20_001));
        env.with_context(&context);
        let calendar = serve_calendar(vec![ok(GOOGLE)]);
        done(env.run(Some(Language::En), None).0);

        assert_eq!(reads(), 0);
        assert!(calendar.try_iter().next().is_none());
        let (_, stdin) = env.sent();
        assert!(
            stdin.contains("calendar_title: \"Quarterly review\""),
            "{stdin}"
        );
        assert!(stdin.contains("partial: calendar_event\n"), "{stdin}");
        assert!(!stdin.contains("Plans & goals"), "{stdin}");
    }
}
