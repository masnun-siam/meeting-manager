//! `meeting process`: extract → transcribe → summarise → write for one recording, in the
//! foreground. Every stage change is saved to the job file and the `meta.json` run first.

use std::fs;
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context, anyhow};

use crate::Language;
use crate::calendar::{self, Meeting, Recording};
use crate::calendar_fetch::{self, Policy};
use crate::config::{Config, check_recording};
use crate::context_block::{self, Input};
use crate::deepgram::Deepgram;
use crate::jobs::{Job, JobStore, Stage, new_job_id, now_utc, write_atomic};
use crate::lock::Lock;
use crate::meta::{Meta, Run, Speaker, recorded_at_for};
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

/// What a run has recorded so far; `meta` stays `None` until the stage plan is known.
struct State<'a> {
    jobs: &'a JobStore,
    job: Job,
    meta: Option<Meta>,
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
        self.job.error = Some(format!(
            "{stage}: {}",
            reason.lines().next().unwrap_or_default()
        ));
        self.job.stage = Stage::Failed;
        self.job.updated_at = now_utc();
        if let Err(e) = self.persist() {
            tracing::error!("cannot record the failure: {e:#}");
        }
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
            let title = self.summarise_and_write(st, &p, &text, plan.language)?;
            (title, Some(p.summary.clone()))
        } else {
            (NO_SPEECH.to_string(), None)
        };

        remove(&p.audio)?;
        remove(&p.raw)?;
        st.finish(&title, summary)?;
        Ok(title)
    }

    fn summarise_and_write(
        &self,
        st: &mut State,
        p: &Paths,
        text: &str,
        language: Language,
    ) -> anyhow::Result<String> {
        st.enter(Stage::Summarising)?;
        let extra = self.context_extra(p, st.meta());
        let summary = summarise::summarise(
            &self.config.claude_path,
            text,
            extra.block.as_deref(),
            self.claude_timeout,
        )?;

        st.enter(Stage::Writing)?;
        let body = summary_md::render(&summary);
        write_atomic(&p.summary, body.as_bytes())?;
        let meta = st.meta();
        vault::write_note(
            &self.config.vault_dir,
            &self.config.vault_folder,
            &Note {
                recorded_at: &meta.recorded_at,
                title: &summary.title,
                language,
                speakers: &summary_md::speaker_names(&summary),
                recording: &p.recording,
                transcript: &p.transcript,
                body: &body,
                meeting_title: extra.meeting_title.as_deref(),
                attendees: &extra.attendees,
                calendar: extra.calendar,
            },
        )?;
        meta.title = Some(summary.title.clone());
        meta.speakers = summary
            .speakers
            .into_iter()
            .map(|s| Speaker {
                label: s.label,
                name: s.name,
                evidence: s.evidence,
            })
            .collect();
        Ok(summary.title)
    }

    /// Rebuilt on every run, so a rerun picks up a changed invite.
    fn context_extra(&self, p: &Paths, meta: &Meta) -> Extra {
        if !self.context {
            return Extra::default();
        }
        let Some(context) = context::load(&p.context) else {
            return Extra::default();
        };
        let (meeting, calendar) = match &context.meeting_id {
            Some(id) => self.look_up(&context, id),
            None => (None, None),
        };
        let block = context_block::build(&Input {
            context: Some(&context),
            meeting: meeting.as_ref(),
            recorded_at: &meta.recorded_at,
            duration_s: meta.duration_s,
        });
        let (meeting_title, attendees) = vault::note_people(Some(&context), meeting.as_ref());
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
            fs::write(self.file("context.json"), context.to_string()).unwrap();
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
}
