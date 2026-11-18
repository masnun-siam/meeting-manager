//! `meeting-manager://` links (quick-record FR-1 to FR-5, FR-12): a pure parser, a dispatcher
//! that applies the PRD's state table to the recorder, and the cold-launch decision (FR-4, D18)
//! of whether the main window shows. Link text is never logged (FR-12): only the action and the
//! outcome or ignore reason.

use std::mem;
use std::sync::mpsc::{self, Sender};
use std::sync::{Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use meeting::Language;
use tauri::{AppHandle, Manager, Runtime, Url};

use crate::App;
use crate::recorder::{Reason, Recorder};

const SCHEME: &str = "meeting-manager";
const HOST: &str = "record";
/// How long a launch waits for a link before showing the main window (FR-4: under 1 s). On a
/// bundled cold `open` macOS delivered the link before `setup` every time (#235's PR), so
/// this is only a margin for a slow launch.
pub const LAUNCH_GRACE: Duration = Duration::from_millis(100);

/// What a link asks for.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Action {
    Start(Option<Language>),
    Stop,
    Pause,
    Resume,
    Toggle(Option<Language>),
}

impl Action {
    /// Safe to log: no language value.
    fn kind(self) -> &'static str {
        match self {
            Action::Start(_) => "start",
            Action::Stop => "stop",
            Action::Pause => "pause",
            Action::Resume => "resume",
            Action::Toggle(_) => "toggle",
        }
    }
}

/// Why a link was ignored (FR-12).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Ignored {
    UnknownRoute,
    BadLanguage,
    UnknownParam,
}

impl Ignored {
    pub fn kind(&self) -> &'static str {
        match self {
            Ignored::UnknownRoute => "unknown_route",
            Ignored::BadLanguage => "bad_language",
            Ignored::UnknownParam => "unknown_param",
        }
    }
}

/// What a link did.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Outcome {
    Started,
    /// A start the recorder refused, with its code; never `already_recording`.
    Refused(&'static str),
    Stopped,
    Paused,
    Resumed,
    /// The state table's "—": nothing changed, no error.
    Nothing,
}

impl Outcome {
    fn kind(self) -> &'static str {
        match self {
            Outcome::Started => "started",
            Outcome::Refused(code) => code,
            Outcome::Stopped => "stopped",
            Outcome::Paused => "paused",
            Outcome::Resumed => "resumed",
            Outcome::Nothing => "nothing",
        }
    }
}

/// The link's action, or why it is ignored. `Url` lowercases the scheme but not the host of a
/// non-special scheme, so host and path are lowercased here.
pub fn parse(url: &Url) -> Result<Action, Ignored> {
    let host = url.host_str().map(str::to_ascii_lowercase);
    let routed = url.scheme() == SCHEME
        && host.as_deref() == Some(HOST)
        && url.username().is_empty()
        && url.password().is_none()
        && url.port().is_none()
        && url.fragment().is_none();
    let path = url.path().to_ascii_lowercase();
    let route = match path.strip_suffix('/').unwrap_or(&path) {
        "/start" if routed => "start",
        "/stop" if routed => "stop",
        "/pause" if routed => "pause",
        "/resume" if routed => "resume",
        "/toggle" if routed => "toggle",
        _ => return Err(Ignored::UnknownRoute),
    };
    let takes_language = matches!(route, "start" | "toggle");
    let mut language = None;
    for (key, value) in url.query_pairs() {
        if key != "language" || !takes_language || language.is_some() {
            return Err(Ignored::UnknownParam);
        }
        language = Some(value);
    }
    let language = match language.as_deref() {
        None => None,
        Some("bn") => Some(Language::Bn),
        Some("en") => Some(Language::En),
        Some(_) => return Err(Ignored::BadLanguage),
    };
    Ok(match route {
        "start" => Action::Start(language),
        "toggle" => Action::Toggle(language),
        "stop" => Action::Stop,
        "pause" => Action::Pause,
        _ => Action::Resume,
    })
}

/// Applies `action` to the recorder as the PRD's state table says. `Starting` reports `idle`,
/// so a start then meets `already_recording`, which is swallowed like `not_recording`.
pub fn dispatch(recorder: &Recorder, action: Action) -> Outcome {
    let state = recorder.status().state;
    match (action, state) {
        (Action::Start(language) | Action::Toggle(language), "idle") => {
            match recorder.start_ad_hoc(language) {
                Ok(_) => Outcome::Started,
                Err(r) if r.code == "already_recording" => Outcome::Nothing,
                Err(r) => Outcome::Refused(r.code),
            }
        }
        (Action::Stop | Action::Toggle(_), "recording" | "paused") => {
            // The saving thread finishes on its own.
            match recorder.stop(Reason::User) {
                Some(_saving) => Outcome::Stopped,
                None => Outcome::Nothing,
            }
        }
        (Action::Pause, "recording") => match recorder.pause() {
            Ok(()) => Outcome::Paused,
            Err(_) => Outcome::Nothing,
        },
        (Action::Resume, "paused") => match recorder.resume() {
            Ok(()) => Outcome::Resumed,
            Err(_) => Outcome::Nothing,
        },
        _ => Outcome::Nothing,
    }
}

/// FR-5: a refused link start shows the main window, where the start error is.
fn shows_window(outcome: Outcome) -> bool {
    matches!(outcome, Outcome::Refused(_))
}

/// FR-4: a launch shows the main window unless a link caused it.
fn show_at_launch(link_arrived: bool) -> bool {
    !link_arrived
}

fn ignored_line(reason: Ignored) -> String {
    format!("link ignored: {}", reason.kind())
}

fn handled_line(action: Action, outcome: Outcome) -> String {
    format!("link: {} → {}", action.kind(), outcome.kind())
}

/// Where the launch decision stands.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Decision {
    Waiting,
    LinkArrived,
    Decided,
}

/// Managed state: whether a link arrived before the launch decision, and the actions of links
/// macOS delivered before `setup` (a cold launch does that), run once the recorder exists.
struct LinkLaunch {
    decision: Mutex<Decision>,
    /// `None` before `on_launch`.
    launched: Mutex<Option<Instant>>,
    queued: Mutex<Vec<Action>>,
    /// The `link` worker, started by the first action: one thread runs every link in the order
    /// macOS delivered them.
    worker: Mutex<Option<Sender<Action>>>,
}

impl Default for LinkLaunch {
    fn default() -> Self {
        LinkLaunch {
            decision: Mutex::new(Decision::Waiting),
            launched: Mutex::new(None),
            queued: Mutex::new(Vec::new()),
            worker: Mutex::new(None),
        }
    }
}

impl LinkLaunch {
    /// Keeps `action` for `launch` when the app has not launched yet; `false` when it has.
    fn queue(&self, action: Action) -> bool {
        let launched = self.launched.lock().unwrap_or_else(PoisonError::into_inner);
        if launched.is_some() {
            return false;
        }
        self.queued
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(action);
        true
    }

    /// Marks the launch; returns the actions queued before it, in order.
    fn launch(&self) -> Vec<Action> {
        *self.launched.lock().unwrap_or_else(PoisonError::into_inner) = Some(Instant::now());
        mem::take(&mut *self.queued.lock().unwrap_or_else(PoisonError::into_inner))
    }

    /// Records a link; `true` for the first one before the decision.
    fn link_arrived(&self) -> bool {
        let mut decision = self.decision.lock().unwrap_or_else(PoisonError::into_inner);
        let first = *decision == Decision::Waiting;
        if first {
            *decision = Decision::LinkArrived;
        }
        first
    }

    /// Makes the launch decision once: `true` when the window should show.
    fn decide(&self) -> bool {
        let mut decision = self.decision.lock().unwrap_or_else(PoisonError::into_inner);
        let link = match *decision {
            Decision::Waiting => false,
            Decision::LinkArrived => true,
            Decision::Decided => return false,
        };
        *decision = Decision::Decided;
        show_at_launch(link)
    }
}

/// Main thread only (setup and run events), so managing on first use cannot race.
fn launch_state<R: Runtime>(app: &AppHandle<R>) -> tauri::State<'_, LinkLaunch> {
    app.manage(LinkLaunch::default());
    app.state::<LinkLaunch>()
}

/// Called once at startup, after the tray. The main window starts hidden; it shows after
/// `LAUNCH_GRACE` unless a link arrived first (FR-4, D18).
pub fn on_launch<R: Runtime>(app: &AppHandle<R>) {
    for action in launch_state(app).launch() {
        handle(app, action);
    }
    let timer = app.clone();
    let spawned = thread::Builder::new()
        .name("link-launch".into())
        .spawn(move || {
            thread::sleep(LAUNCH_GRACE);
            if timer.state::<LinkLaunch>().decide() {
                let shown = timer.clone();
                if timer
                    .run_on_main_thread(move || crate::focus_main(&shown))
                    .is_err()
                {
                    tracing::warn!("link: the main window could not be shown");
                }
            }
        });
    if spawned.is_err() {
        tracing::warn!("link: the launch timer could not start; showing the main window");
        crate::focus_main(app);
    }
}

/// Called for macOS's opened-URLs run event, on the main thread.
pub fn on_opened<R: Runtime>(app: &AppHandle<R>, urls: Vec<Url>) {
    let launch = launch_state(app);
    if launch.link_arrived() {
        match *launch
            .launched
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
        {
            Some(at) => tracing::info!(
                "link: first link {} ms after launch",
                at.elapsed().as_millis()
            ),
            None => tracing::info!("link: first link before launch"),
        }
    }
    for url in urls {
        match parse(&url) {
            Err(reason) => tracing::info!("{}", ignored_line(reason)),
            Ok(action) if !launch.queue(action) => handle(app, action),
            Ok(_) => {}
        }
    }
}

/// Queues `action` for the `link` worker, off the main thread: Start may block on a
/// permission prompt, and later links wait for it, so they run in order.
fn handle<R: Runtime>(app: &AppHandle<R>, action: Action) {
    let launch = launch_state(app);
    let mut worker = launch.worker.lock().unwrap_or_else(PoisonError::into_inner);
    if worker.is_none() {
        *worker = start_worker(app);
    }
    let sent = worker.as_ref().is_some_and(|tx| tx.send(action).is_ok());
    if !sent {
        // Started again by the next link.
        *worker = None;
        tracing::warn!("link: {} not run", action.kind());
    }
}

fn start_worker<R: Runtime>(app: &AppHandle<R>) -> Option<Sender<Action>> {
    let Some(state) = app.try_state::<App>() else {
        tracing::warn!("link: the app has not started");
        return None;
    };
    let recorder = state.recorder.clone();
    let app = app.clone();
    let (tx, rx) = mpsc::channel::<Action>();
    let spawned = thread::Builder::new().name("link".into()).spawn(move || {
        for action in rx {
            let outcome = dispatch(&recorder, action);
            tracing::info!("{}", handled_line(action, outcome));
            if shows_window(outcome) {
                let shown = app.clone();
                if app
                    .run_on_main_thread(move || crate::focus_main(&shown))
                    .is_err()
                {
                    tracing::warn!("link: the main window could not be shown");
                }
            }
        }
    });
    match spawned {
        Ok(_) => Some(tx),
        Err(_) => {
            tracing::warn!("link: the link thread could not start");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use super::*;
    use crate::recorder::tests::{Rig, mic_denied_capture, ok_capture, rig};

    fn parsed(link: &str) -> Result<Action, Ignored> {
        parse(&Url::parse(link).expect("a valid URL"))
    }

    #[test]
    fn each_route_parses_in_any_case_with_or_without_a_trailing_slash() {
        let rows = [
            ("meeting-manager://record/start", Action::Start(None)),
            ("meeting-manager://record/start/", Action::Start(None)),
            ("MEETING-MANAGER://Record/START", Action::Start(None)),
            ("meeting-manager://record/stop", Action::Stop),
            ("meeting-manager://RECORD/Stop/", Action::Stop),
            ("meeting-manager://record/pause", Action::Pause),
            ("meeting-manager://record/PAUSE/", Action::Pause),
            ("meeting-manager://record/resume", Action::Resume),
            ("Meeting-Manager://record/Resume/", Action::Resume),
            ("meeting-manager://record/toggle", Action::Toggle(None)),
            ("meeting-manager://Record/TOGGLE/", Action::Toggle(None)),
            (
                "meeting-manager://record/start?language=en",
                Action::Start(Some(Language::En)),
            ),
            (
                "meeting-manager://record/start/?language=bn",
                Action::Start(Some(Language::Bn)),
            ),
            (
                "meeting-manager://record/toggle?language=bn",
                Action::Toggle(Some(Language::Bn)),
            ),
            ("meeting-manager://record/start?", Action::Start(None)),
        ];
        for (link, expected) in rows {
            assert_eq!(parsed(link), Ok(expected), "{link}");
        }
    }

    /// The seven links of the Alfred workflow's `rec` list (alfred-workflow FR-2), in order.
    const ALFRED_LINKS: [&str; 7] = [
        "meeting-manager://record/toggle",
        "meeting-manager://record/start",
        "meeting-manager://record/start?language=en",
        "meeting-manager://record/start?language=bn",
        "meeting-manager://record/pause",
        "meeting-manager://record/resume",
        "meeting-manager://record/stop",
    ];

    /// Every `meeting-manager:` link in an Alfred `info.plist`, plain string work so no plist
    /// crate is needed (alfred-workflow D9). A link ends at a quote (JSON item), `<` (XML
    /// string) or whitespace.
    fn workflow_links(plist: &str) -> Vec<String> {
        plist
            .match_indices("meeting-manager:")
            .map(|(start, _)| {
                let rest = &plist[start..];
                let end = rest
                    .find(|c: char| c == '"' || c == '<' || c.is_whitespace())
                    .unwrap_or(rest.len());
                rest[..end].replace("&amp;", "&")
            })
            .collect()
    }

    #[test]
    fn the_alfred_workflow_opens_only_links_the_app_acts_on() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../alfred/info.plist");
        let plist = fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        let links = workflow_links(&plist);
        for link in &links {
            assert!(
                parsed(link).is_ok(),
                "the Alfred workflow opens {link}, which the app ignores"
            );
        }
        for link in ALFRED_LINKS {
            assert!(
                links.iter().any(|found| found == link),
                "the Alfred workflow lacks {link}"
            );
        }
    }

    #[test]
    fn the_alfred_link_extractor_finds_every_link() {
        let plist = "<string>meeting-manager://record/stop</string>\n\
            <string>[{\"arg\":\"meeting-manager://record/start?language=en\"}]</string>\n\
            meeting-manager://record/pause meeting-manager://record/a?x=1&amp;y=2";
        assert_eq!(
            workflow_links(plist),
            [
                "meeting-manager://record/stop",
                "meeting-manager://record/start?language=en",
                "meeting-manager://record/pause",
                "meeting-manager://record/a?x=1&y=2",
            ]
        );
    }

    #[test]
    fn other_links_are_ignored_with_their_reason() {
        let rows = [
            ("meeting-manager://other/start", Ignored::UnknownRoute),
            ("meeting-manager://record/begin", Ignored::UnknownRoute),
            ("meeting-manager://record", Ignored::UnknownRoute),
            ("meeting-manager://record/", Ignored::UnknownRoute),
            ("meeting-manager://record/start//", Ignored::UnknownRoute),
            ("meeting-manager://record/start/now", Ignored::UnknownRoute),
            ("meeting-manager:record/start", Ignored::UnknownRoute),
            ("meeting-manager://record/start#x", Ignored::UnknownRoute),
            ("meeting-manager://me@record/start", Ignored::UnknownRoute),
            ("meeting-manager://record:80/start", Ignored::UnknownRoute),
            ("other://record/start", Ignored::UnknownRoute),
            ("https://record/start", Ignored::UnknownRoute),
            ("meeting-manager://other/start?x=1", Ignored::UnknownRoute),
            (
                "meeting-manager://record/start?language=fr",
                Ignored::BadLanguage,
            ),
            (
                "meeting-manager://record/start?language=",
                Ignored::BadLanguage,
            ),
            (
                "meeting-manager://record/start?language",
                Ignored::BadLanguage,
            ),
            (
                "meeting-manager://record/toggle?language=EN",
                Ignored::BadLanguage,
            ),
            ("meeting-manager://record/start?x=1", Ignored::UnknownParam),
            (
                "meeting-manager://record/stop?language=en",
                Ignored::UnknownParam,
            ),
            (
                "meeting-manager://record/pause?language=en",
                Ignored::UnknownParam,
            ),
            (
                "meeting-manager://record/resume?language=bn",
                Ignored::UnknownParam,
            ),
            (
                "meeting-manager://record/start?language=en&language=bn",
                Ignored::UnknownParam,
            ),
            (
                "meeting-manager://record/start?language=fr&x=1",
                Ignored::UnknownParam,
            ),
            (
                "meeting-manager://record/start?Language=en",
                Ignored::UnknownParam,
            ),
        ];
        for (link, expected) in rows {
            assert_eq!(parsed(link), Err(expected), "{link}");
        }
    }

    #[test]
    fn ignore_reasons_have_their_log_names() {
        assert_eq!(
            [
                Ignored::UnknownRoute,
                Ignored::BadLanguage,
                Ignored::UnknownParam
            ]
            .map(|r| r.kind()),
            ["unknown_route", "bad_language", "unknown_param"]
        );
    }

    #[test]
    fn log_lines_carry_no_link_text_or_language() {
        assert_eq!(
            ignored_line(Ignored::BadLanguage),
            "link ignored: bad_language"
        );
        let actions = [
            Action::Start(Some(Language::En)),
            Action::Toggle(Some(Language::Bn)),
            Action::Stop,
            Action::Pause,
            Action::Resume,
        ];
        let outcomes = [
            Outcome::Started,
            Outcome::Refused("permission_mic"),
            Outcome::Stopped,
            Outcome::Paused,
            Outcome::Resumed,
            Outcome::Nothing,
        ];
        for action in actions {
            for outcome in outcomes {
                let line = handled_line(action, outcome);
                for leak in ["meeting-manager", "record/", "?", "=", "language", "bn"] {
                    assert!(!line.contains(leak), "{line} contains {leak}");
                }
            }
        }
        assert_eq!(
            handled_line(Action::Start(Some(Language::En)), Outcome::Started),
            "link: start → started"
        );
    }

    /// What a "—" cell must leave as it was.
    fn snapshot(rig: &Rig) -> (&'static str, usize, Option<String>) {
        let status = rig.recorder.status();
        let folders = fs::read_dir(rig.home.join("Recordings")).map_or(0, |d| d.count());
        (
            status.state,
            folders,
            status.notice.map(|n| n.message.clone()),
        )
    }

    /// A rig in `state`: `idle`, `recording`, `paused` or `saving` (the updater's hold).
    fn rig_in(name: &str, state: &str) -> (Rig, Option<crate::recorder::SavingGuard>) {
        let rig = rig(&format!("link-{name}"), ok_capture());
        let mut guard = None;
        match state {
            "idle" => {}
            "recording" => {
                rig.start().unwrap();
            }
            "paused" => {
                rig.start().unwrap();
                rig.recorder.pause().unwrap();
            }
            "saving" => guard = rig.recorder.begin_saving(),
            _ => unreachable!(),
        }
        assert_eq!(rig.state(), state);
        (rig, guard)
    }

    fn finish(rig: &Rig) {
        if matches!(rig.state(), "recording" | "paused") {
            rig.stop(Reason::User);
        } else if rig.state() == "saving" {
            rig.wait_idle();
        }
    }

    #[test]
    fn start_and_toggle_start_when_idle_in_the_links_language() {
        for action in [
            Action::Start(Some(Language::En)),
            Action::Toggle(Some(Language::En)),
        ] {
            let (rig, _) = rig_in(&format!("start-{}", action.kind()), "idle");
            assert_eq!(dispatch(&rig.recorder, action), Outcome::Started);
            let status = rig.recorder.status();
            assert_eq!((status.state, status.language), ("recording", Language::En));
            finish(&rig);
            // The link's language is now the last used.
            assert_eq!(rig.recorder.status().language, Language::En);
        }
    }

    #[test]
    fn a_start_without_a_language_uses_the_last_used() {
        let rig = rig("link-last-language", ok_capture());
        rig.start().unwrap();
        rig.stop(Reason::User);
        assert_eq!(
            dispatch(&rig.recorder, Action::Start(None)),
            Outcome::Started
        );
        assert_eq!(rig.recorder.status().language, Language::En);
        finish(&rig);
    }

    #[test]
    fn pause_resume_and_stop_follow_the_table() {
        let rows = [
            ("recording", Action::Pause, Outcome::Paused, "paused"),
            ("paused", Action::Resume, Outcome::Resumed, "recording"),
            ("recording", Action::Stop, Outcome::Stopped, "saving"),
            ("paused", Action::Stop, Outcome::Stopped, "saving"),
            (
                "recording",
                Action::Toggle(None),
                Outcome::Stopped,
                "saving",
            ),
            ("paused", Action::Toggle(None), Outcome::Stopped, "saving"),
        ];
        for (i, (state, action, outcome, after)) in rows.into_iter().enumerate() {
            let (rig, _) = rig_in(&format!("table-{i}"), state);
            assert_eq!(
                dispatch(&rig.recorder, action),
                outcome,
                "{state} {action:?}"
            );
            let now = rig.state();
            // The saving thread may already have finished with the fake capture.
            assert!(
                now == after || after == "saving" && now == "idle",
                "{state} {action:?} → {now}"
            );
            finish(&rig);
        }
    }

    #[test]
    fn every_dash_cell_changes_nothing() {
        let all = [
            Action::Start(Some(Language::Bn)),
            Action::Stop,
            Action::Pause,
            Action::Resume,
            Action::Toggle(Some(Language::Bn)),
        ];
        let rows: [(&str, &[Action]); 4] = [
            ("idle", &[Action::Stop, Action::Pause, Action::Resume]),
            ("recording", &[Action::Start(None), Action::Resume]),
            ("paused", &[Action::Start(None), Action::Pause]),
            ("saving", &all),
        ];
        for (state, actions) in rows {
            for (i, &action) in actions.iter().enumerate() {
                let (rig, guard) = rig_in(&format!("dash-{state}-{i}"), state);
                let before = snapshot(&rig);
                assert_eq!(
                    dispatch(&rig.recorder, action),
                    Outcome::Nothing,
                    "{state} {action:?}"
                );
                assert_eq!(snapshot(&rig), before, "{state} {action:?}");
                assert_eq!(rig.recorder.status().start_error, None);
                drop(guard);
                finish(&rig);
            }
        }
    }

    #[test]
    fn a_refused_start_shows_the_window_and_a_start_while_recording_does_not() {
        let rig = rig("link-refused", mic_denied_capture());
        let outcome = dispatch(&rig.recorder, Action::Start(None));
        assert_eq!(outcome, Outcome::Refused("permission_mic"));
        assert!(shows_window(outcome));
        assert_eq!(rig.state(), "idle");

        let (rig, _) = rig_in("busy", "recording");
        let outcome = dispatch(&rig.recorder, Action::Start(None));
        assert!(!shows_window(outcome));
        assert_eq!(rig.recorder.status().start_error, None);
        finish(&rig);
        for outcome in [
            Outcome::Started,
            Outcome::Stopped,
            Outcome::Paused,
            Outcome::Resumed,
            Outcome::Nothing,
        ] {
            assert!(!shows_window(outcome));
        }
    }

    #[test]
    fn a_launch_shows_the_window_only_without_a_link() {
        assert!(show_at_launch(false));
        assert!(!show_at_launch(true));

        let normal = LinkLaunch::default();
        assert!(normal.decide());
        // A link after the decision changes nothing, and nothing is decided twice.
        assert!(!normal.link_arrived());
        assert!(!normal.decide());

        let linked = LinkLaunch::default();
        assert!(linked.link_arrived());
        assert!(!linked.link_arrived());
        assert!(!linked.decide());
    }

    #[test]
    fn links_before_the_launch_wait_for_it_and_later_ones_run_at_once() {
        let launch = LinkLaunch::default();
        assert!(launch.queue(Action::Start(Some(Language::En))));
        assert!(launch.queue(Action::Pause));
        assert_eq!(
            launch.launch(),
            [Action::Start(Some(Language::En)), Action::Pause]
        );
        assert!(!launch.queue(Action::Stop));
        assert_eq!(launch.launch(), []);
    }

    #[test]
    fn the_grace_is_under_a_second() {
        assert!(LAUNCH_GRACE < Duration::from_secs(1));
    }
}
