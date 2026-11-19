//! The meeting folder's hand-off to the tool: `context.json` (written at Start, so a crash keeps
//! it) and, after the WAV is finished, conversion to `recording.m4a` and `meeting process`
//! (PRD FR-4, FR-6). Logs carry rule names and error kinds only, never event values.

use std::fs;
use std::io;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use anyhow::Context as _;
use meeting::Language;
use meeting::context::{self, CalendarEvent, Context, Platform};

use super::Clock;
use super::convert;
use super::notice::{self, Kind, Notice};

pub const CONTEXT: &str = "context.json";
pub const WAV: &str = "recording.partial.wav";
pub const M4A: &str = "recording.m4a";
/// Under this much recorded time the m4a is kept but not processed (the extension's rule).
pub const MIN_PROCESS_S: u64 = 10;

/// `meeting::spawn::process_detached`'s shape.
pub type SpawnFn = fn(&Path, &Path, Option<Language>, Option<&str>) -> io::Result<()>;
/// `meeting::spawn::check_tool`'s shape.
pub type CheckToolFn = fn(&Path) -> Result<(), &'static str>;

/// What finishing a recording runs: the configured paths and the spawn functions.
pub struct Tools {
    pub ffmpeg: PathBuf,
    pub meeting_path: Option<PathBuf>,
    pub spawn: SpawnFn,
    pub check_tool: CheckToolFn,
}

/// Writes `context.json` for a desktop recording, mode `0600`. An event the tool's `check` would
/// reject is left out; a context it rejects whole is not written. Never fails the recording.
pub fn write_context(folder: &Path, started: &Clock, event: Option<CalendarEvent>) {
    let ctx = Context {
        version: 1,
        platform: Platform::Desktop,
        url: None,
        meeting_id: None,
        tab_title: None,
        meeting_title: None,
        started_at_ms: started.now_ms,
        utc_offset_min: started.utc_offset_min,
        time_zone: started.time_zone.clone(),
        participants: vec![],
        speaking: vec![],
        truncated: vec![],
        calendar_event: event,
    };
    let checked = check(&ctx).or_else(|rule| {
        if ctx.calendar_event.is_none() {
            return Err(rule);
        }
        tracing::warn!("recorder: calendar event left out of context.json: {rule}");
        check(&Context {
            calendar_event: None,
            ..ctx
        })
    });
    let written = match checked {
        Err(rule) => {
            tracing::warn!("recorder: context.json not written: {rule}");
            return;
        }
        Ok(ctx) => write_private(&folder.join(CONTEXT), &ctx),
    };
    if written.is_err() {
        tracing::warn!("recorder: context.json not written");
    }
}

/// `write_context`, only when the file is missing (Start's write failed or it was deleted).
pub fn ensure_context(folder: &Path, started: &Clock, event: Option<CalendarEvent>) {
    if !folder.join(CONTEXT).exists() {
        write_context(folder, started, event);
    }
}

fn check(ctx: &Context) -> Result<Context, &'static str> {
    serde_json::to_value(ctx)
        .map_err(|_| "not serialisable")
        .and_then(|raw| context::check(&raw))
}

fn write_private(path: &Path, ctx: &Context) -> anyhow::Result<()> {
    // As `jobs::write_atomic`, but the temp file is created `0600`: the file names the event
    // and its attendees, so it is never readable by others, not even before a rename.
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)
        .with_context(|| format!("cannot write {}", tmp.display()))?;
    file.write_all(&serde_json::to_vec(ctx)?)?;
    drop(file);
    fs::rename(&tmp, path).with_context(|| format!("cannot write {}", path.display()))
}

/// The WAV continued part `n` records into (continue-recording FR-9).
pub fn part_wav(n: u32) -> String {
    format!("recording-part-{n}.partial.wav")
}

/// The hard link of continued part `n`'s WAV, directly under the recordings folder, while the
/// part of meeting folder `meeting` records (D17): `.continuing-<meeting>-part-<n>.partial.wav`.
pub fn part_link(meeting: &str, n: u32) -> String {
    format!(".continuing-{meeting}-part-{n}.partial.wav")
}

/// What continued part `n`'s WAV is converted to.
pub fn part_m4a(n: u32) -> String {
    format!("recording-part-{n}.m4a")
}

/// Converts the folder's WAV to `recording.m4a`, deletes the WAV, and spawns `meeting process`
/// when at least `MIN_PROCESS_S` were recorded. `prefix` says why the recording stopped. Returns
/// the notice to show; a failed conversion keeps the WAV and names the folder for a retry.
pub fn finish_recording(
    tools: &Tools,
    folder: &Path,
    language: Language,
    frames: u64,
    prefix: &str,
) -> Notice {
    convert_and_hand_off(tools, folder, WAV, M4A, language, frames, prefix)
}

/// `finish_recording` for continued part `n` (continue-recording FR-9): converts
/// `recording-part-N.partial.wav` to `recording-part-N.m4a` and hands off the meeting's
/// `recording.m4a` through `tools.spawn` (`--continue`). `frames` is the meeting's total, so the
/// 10 s rule counts every part.
pub fn finish_part(
    tools: &Tools,
    folder: &Path,
    n: u32,
    language: Language,
    frames: u64,
    prefix: &str,
) -> Notice {
    match convert_part(tools, folder, n, prefix) {
        Ok(()) => hand_off(tools, folder, language, frames, prefix),
        Err(notice) => notice,
    }
}

/// Converts continued part `n`'s WAV to its m4a and deletes the WAV, without a hand-off: launch
/// recovery converts every leftover part of a meeting, then hands it off once.
pub fn convert_part(tools: &Tools, folder: &Path, n: u32, prefix: &str) -> Result<(), Notice> {
    convert(tools, folder, &part_wav(n), &part_m4a(n), prefix)
}

/// Converts `folder/wav` to `folder/m4a`, deletes the WAV, and hands off.
fn convert_and_hand_off(
    tools: &Tools,
    folder: &Path,
    wav: &str,
    m4a: &str,
    language: Language,
    frames: u64,
    prefix: &str,
) -> Notice {
    match convert(tools, folder, wav, m4a, prefix) {
        Ok(()) => hand_off(tools, folder, language, frames, prefix),
        Err(notice) => notice,
    }
}

/// Converts `folder/wav` to `folder/m4a` and deletes the WAV. A failure keeps the WAV and is the
/// notice naming the folder for a retry.
fn convert(tools: &Tools, folder: &Path, wav: &str, m4a: &str, prefix: &str) -> Result<(), Notice> {
    let (wav, m4a) = (folder.join(wav), folder.join(m4a));
    if let Err(e) = convert::to_m4a(&tools.ffmpeg, &wav, &m4a) {
        tracing::warn!("recorder: conversion failed; the WAV is kept");
        return Err(Notice::retry(format!("{prefix}{e:#}"), folder));
    }
    if let Err(e) = fs::remove_file(&wav) {
        // The m4a exists, so nothing will convert this folder again.
        tracing::warn!("recorder: the WAV was not deleted: {}", e.kind());
    }
    Ok(())
}

/// Spawns the tool on the meeting's `recording.m4a` when `frames` (the meeting's total) reach
/// `MIN_PROCESS_S`; the notice says what happened.
pub fn hand_off(
    tools: &Tools,
    folder: &Path,
    language: Language,
    frames: u64,
    prefix: &str,
) -> Notice {
    // A prefixed message is two sentences, so the second ends with a full stop too.
    let message = |text: &str| match prefix {
        "" => text.to_string(),
        _ if text.ends_with('.') => format!("{prefix}{text}"),
        _ => format!("{prefix}{text}."),
    };
    if frames < MIN_PROCESS_S * u64::from(crate::capture::SAMPLE_RATE) {
        return Notice::new(Kind::TooShort, message(notice::TOO_SHORT));
    }
    let Some(exe) = usable_tool(tools) else {
        return Notice::new(Kind::Setup, message(notice::SETUP));
    };
    match (tools.spawn)(exe, &folder.join(M4A), Some(language), None) {
        Ok(()) => {
            tracing::info!("recorder: sent for processing");
            Notice::new(Kind::Saved, message(notice::SAVED))
        }
        Err(e) => {
            tracing::warn!("recorder: processing not started: {}", e.kind());
            Notice::new(
                Kind::Error,
                message(&format!("Saved, but processing could not be started: {e}")),
            )
        }
    }
}

fn usable_tool(tools: &Tools) -> Option<&Path> {
    let exe = tools.meeting_path.as_deref()?;
    match (tools.check_tool)(exe) {
        Ok(()) => Some(exe),
        Err(rule) => {
            tracing::warn!("recorder: meeting_path unusable: {rule}");
            None
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::os::unix::fs::PermissionsExt;
    use std::sync::Mutex;

    use meeting::calendar::Response;
    use meeting::context::EventAttendee;
    use serde_json::Value;

    use super::*;
    use crate::recorder::convert::tests::{ffmpeg, tone_wav};
    use crate::recorder::wav::tests::temp;

    type Call = (PathBuf, PathBuf, Option<Language>);

    /// Every fake spawn, from any thread; a test reads only those under its own folder.
    static SPAWNS: Mutex<Vec<Call>> = Mutex::new(Vec::new());

    pub(crate) fn fake_spawn(
        exe: &Path,
        recording: &Path,
        language: Option<Language>,
        _job: Option<&str>,
    ) -> io::Result<()> {
        SPAWNS
            .lock()
            .unwrap()
            .push((exe.into(), recording.into(), language));
        Ok(())
    }

    /// Every fake `--continue` spawn, kept apart from `SPAWNS` so a test tells the two apart.
    static CONTINUES: Mutex<Vec<Call>> = Mutex::new(Vec::new());

    /// `meeting::spawn::continue_detached`'s fake: records the call, as `fake_spawn` does.
    pub(crate) fn fake_continue_spawn(
        exe: &Path,
        recording: &Path,
        language: Option<Language>,
        job: Option<&str>,
    ) -> io::Result<()> {
        assert_eq!(job, None, "the desktop passes no job id");
        CONTINUES
            .lock()
            .unwrap()
            .push((exe.into(), recording.into(), language));
        Ok(())
    }

    /// The `--continue` spawns of recordings under `dir`.
    pub(crate) fn continues_under(dir: &Path) -> Vec<Call> {
        let calls = CONTINUES.lock().unwrap();
        calls
            .iter()
            .filter(|(_, recording, _)| recording.starts_with(dir))
            .cloned()
            .collect()
    }

    pub(crate) fn failing_spawn(
        _: &Path,
        _: &Path,
        _: Option<Language>,
        _: Option<&str>,
    ) -> io::Result<()> {
        Err(io::Error::other("fork refused"))
    }

    pub(crate) fn spawns_under(dir: &Path) -> Vec<Call> {
        let calls = SPAWNS.lock().unwrap();
        calls
            .iter()
            .filter(|(_, recording, _)| recording.starts_with(dir))
            .cloned()
            .collect()
    }

    /// An executable file `check_tool` accepts.
    pub(crate) fn tool(dir: &Path) -> PathBuf {
        let exe = dir.join("meeting");
        fs::write(&exe, "#!/bin/sh\n").unwrap();
        fs::set_permissions(&exe, fs::Permissions::from_mode(0o755)).unwrap();
        exe
    }

    pub(crate) fn clock() -> Clock {
        Clock {
            now_ms: 1_794_528_000_000,
            utc_offset_min: 360,
            time_zone: Some("Asia/Dhaka".into()),
        }
    }

    pub(crate) fn event(title: &str, start: &str) -> CalendarEvent {
        CalendarEvent {
            occurrence_id: "occ-1".into(),
            series_id: "series-1".into(),
            title: title.into(),
            start: start.into(),
            end: "2026-11-13T07:00:00+06:00".into(),
            organizer: Some("Organiser".into()),
            calendar: Some("Work".into()),
            attendees: vec![EventAttendee {
                name: "Rafi".into(),
                response: Response::Accepted,
            }],
            description: None,
        }
    }

    fn tools(dir: &Path, spawn: SpawnFn) -> Tools {
        Tools {
            ffmpeg: ffmpeg(),
            meeting_path: Some(tool(dir)),
            spawn,
            check_tool: meeting::spawn::check_tool,
        }
    }

    fn folder_with_wav(dir: &Path, secs: usize) -> PathBuf {
        let folder = dir.join("meeting-2026-11-13_0600");
        fs::create_dir_all(&folder).unwrap();
        tone_wav(&folder.join(WAV), secs);
        folder
    }

    fn context_of(folder: &Path) -> Value {
        serde_json::from_slice(&fs::read(folder.join(CONTEXT)).unwrap()).unwrap()
    }

    #[test]
    fn ten_seconds_is_converted_and_sent_for_processing() {
        let dir = temp("handoff-saved");
        let folder = folder_with_wav(&dir, 10);
        let tools = tools(&dir, fake_spawn);
        let notice = finish_recording(&tools, &folder, Language::En, 480_000, "");
        assert_eq!(
            notice,
            Notice::new(Kind::Saved, "Saved, sent for processing")
        );
        assert!(folder.join(M4A).is_file());
        assert!(!folder.join(WAV).exists());
        assert!(!folder.join("recording.m4a.part").exists());
        assert_eq!(
            spawns_under(&dir),
            [(
                tools.meeting_path.clone().unwrap(),
                folder.join(M4A),
                Some(Language::En)
            )]
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn under_ten_seconds_is_kept_but_not_processed() {
        let dir = temp("handoff-short");
        let folder = folder_with_wav(&dir, 1);
        let notice = finish_recording(&tools(&dir, fake_spawn), &folder, Language::Bn, 479_999, "");
        assert_eq!(notice.kind, Kind::TooShort);
        assert_eq!(
            notice.message,
            "Under 10 seconds, so it was saved but not processed."
        );
        assert!(folder.join(M4A).is_file());
        assert!(spawns_under(&dir).is_empty());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_missing_or_unusable_tool_saves_and_says_how_to_fix_it() {
        let dir = temp("handoff-setup");
        let folder = dir.join("meeting-x");
        fs::create_dir_all(&folder).unwrap();
        let not_executable = dir.join("plain");
        fs::write(&not_executable, "").unwrap();
        for meeting_path in [None, Some(not_executable), Some(PathBuf::from("meeting"))] {
            tone_wav(&folder.join(WAV), 1);
            let tools = Tools {
                meeting_path,
                ..tools(&dir, fake_spawn)
            };
            let notice = finish_recording(&tools, &folder, Language::Bn, 480_000, "");
            assert_eq!(
                notice,
                Notice::new(
                    Kind::Setup,
                    "Processing tool not found — run `meeting install`"
                )
            );
            assert!(folder.join(M4A).is_file());
        }
        assert!(spawns_under(&dir).is_empty());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_failed_spawn_keeps_the_m4a_and_says_why() {
        let dir = temp("handoff-spawn");
        let folder = folder_with_wav(&dir, 1);
        let notice = finish_recording(
            &tools(&dir, failing_spawn),
            &folder,
            Language::Bn,
            480_000,
            "",
        );
        assert_eq!(notice.kind, Kind::Error);
        assert_eq!(
            notice.message,
            "Saved, but processing could not be started: fork refused"
        );
        assert_eq!(notice.folder, None);
        assert!(folder.join(M4A).is_file());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_failed_conversion_keeps_the_wav_and_offers_a_retry() {
        let dir = temp("handoff-ffmpeg");
        let folder = dir.join("meeting-x");
        fs::create_dir_all(&folder).unwrap();
        fs::write(folder.join(WAV), b"corrupt").unwrap();
        let notice = finish_recording(&tools(&dir, fake_spawn), &folder, Language::Bn, 480_000, "");
        assert_eq!(notice.kind, Kind::Error);
        assert!(
            notice.message.starts_with("ffmpeg failed: "),
            "{}",
            notice.message
        );
        assert_eq!(notice.folder.as_deref(), Some(folder.to_str().unwrap()));
        assert!(folder.join(WAV).is_file());
        assert!(!folder.join(M4A).exists());
        assert!(spawns_under(&dir).is_empty());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn the_stop_reason_leads_the_message() {
        let dir = temp("handoff-prefix");
        let folder = folder_with_wav(&dir, 1);
        let notice = finish_recording(
            &tools(&dir, fake_spawn),
            &folder,
            Language::Bn,
            480_000,
            "Stopped at 3 hours. ",
        );
        assert_eq!(
            notice.message,
            "Stopped at 3 hours. Saved, sent for processing."
        );
        fs::remove_dir_all(dir).unwrap();
    }

    /// A meeting folder holding `recording.m4a` and continued part `n`'s WAV of `secs` seconds.
    fn folder_with_part(dir: &Path, n: u32, secs: usize) -> PathBuf {
        let folder = folder_with_wav(dir, 1);
        convert::to_m4a(&ffmpeg(), &folder.join(WAV), &folder.join(M4A)).unwrap();
        fs::remove_file(folder.join(WAV)).unwrap();
        tone_wav(&folder.join(part_wav(n)), secs);
        folder
    }

    #[test]
    fn part_names_follow_the_meeting_folder_contract() {
        assert_eq!(part_wav(2), "recording-part-2.partial.wav");
        assert_eq!(part_m4a(13), "recording-part-13.m4a");
    }

    #[test]
    fn a_part_is_converted_and_the_meeting_handed_off_with_continue() {
        let dir = temp("handoff-part");
        let folder = folder_with_part(&dir, 2, 1);
        let earlier = fs::read(folder.join(M4A)).unwrap();
        let tools = tools(&dir, fake_continue_spawn);
        let notice = finish_part(&tools, &folder, 2, Language::En, 480_000, "");
        assert_eq!(notice, Notice::new(Kind::Saved, notice::SAVED));
        assert!(folder.join(part_m4a(2)).is_file());
        assert!(!folder.join(part_wav(2)).exists());
        assert_eq!(fs::read(folder.join(M4A)).unwrap(), earlier);
        assert_eq!(
            continues_under(&dir),
            [(
                tools.meeting_path.clone().unwrap(),
                folder.join(M4A),
                Some(Language::En)
            )]
        );
        assert!(spawns_under(&dir).is_empty());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_part_under_the_ten_second_total_is_kept_but_not_handed_off() {
        let dir = temp("handoff-part-short");
        let folder = folder_with_part(&dir, 3, 1);
        let notice = finish_part(
            &tools(&dir, fake_continue_spawn),
            &folder,
            3,
            Language::Bn,
            479_999,
            "",
        );
        assert_eq!(notice.kind, Kind::TooShort);
        assert!(folder.join(part_m4a(3)).is_file());
        assert!(!folder.join(part_wav(3)).exists());
        assert!(continues_under(&dir).is_empty());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_part_whose_conversion_fails_keeps_its_wav_and_the_meeting() {
        let dir = temp("handoff-part-ffmpeg");
        let folder = folder_with_part(&dir, 2, 1);
        fs::write(folder.join(part_wav(2)), b"corrupt").unwrap();
        let earlier = fs::read(folder.join(M4A)).unwrap();
        let notice = finish_part(
            &tools(&dir, fake_continue_spawn),
            &folder,
            2,
            Language::Bn,
            480_000,
            "",
        );
        assert_eq!(notice.kind, Kind::Error);
        assert!(
            notice.message.starts_with("ffmpeg failed: "),
            "{}",
            notice.message
        );
        assert_eq!(notice.folder.as_deref(), Some(folder.to_str().unwrap()));
        assert!(folder.join(part_wav(2)).is_file());
        assert!(!folder.join(part_m4a(2)).exists());
        assert_eq!(fs::read(folder.join(M4A)).unwrap(), earlier);
        assert!(continues_under(&dir).is_empty());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn an_ad_hoc_context_is_private_and_passes_the_tools_check() {
        let dir = temp("handoff-context");
        write_context(&dir, &clock(), None);
        let raw = context_of(&dir);
        assert_eq!(
            fs::metadata(dir.join(CONTEXT))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        let checked = context::check(&raw).unwrap();
        assert_eq!(checked.platform, Platform::Desktop);
        assert_eq!(raw["version"], 1);
        assert_eq!(raw["platform"], "desktop");
        assert_eq!(raw["started_at_ms"], 1_794_528_000_000i64);
        assert_eq!(raw["utc_offset_min"], 360);
        assert_eq!(raw["time_zone"], "Asia/Dhaka");
        for null in ["url", "meeting_id", "tab_title"] {
            assert_eq!(raw[null], Value::Null, "{null}");
        }
        assert_eq!(raw["participants"], serde_json::json!([]));
        assert_eq!(raw["speaking"], serde_json::json!([]));
        assert!(raw.get("calendar_event").is_none());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_chosen_event_is_written_unchanged() {
        let dir = temp("handoff-event");
        let chosen = event("Weekly sync", "2026-11-13T06:00:00+06:00");
        write_context(&dir, &clock(), Some(chosen.clone()));
        let checked = context::check(&context_of(&dir)).unwrap();
        assert_eq!(checked.calendar_event, Some(chosen));
        assert!(!fs::read_to_string(dir.join(CONTEXT)).unwrap().contains('@'));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_huge_title_still_leaves_a_valid_context() {
        let dir = temp("handoff-huge");
        let huge = event(&"t".repeat(40_000), "2026-11-13T06:00:00+06:00");
        write_context(&dir, &clock(), Some(huge));
        let checked = context::check(&context_of(&dir)).unwrap();
        assert!(checked.calendar_event.is_some());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn an_event_the_check_rejects_is_dropped_and_the_rest_written() {
        let dir = temp("handoff-bad-event");
        let bad = event("Sync", "not a date");
        write_context(&dir, &clock(), Some(bad));
        let raw = context_of(&dir);
        assert!(context::check(&raw).unwrap().calendar_event.is_none());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn ensure_context_never_rewrites_an_existing_file() {
        let dir = temp("handoff-ensure");
        fs::write(dir.join(CONTEXT), "kept").unwrap();
        ensure_context(&dir, &clock(), None);
        assert_eq!(fs::read_to_string(dir.join(CONTEXT)).unwrap(), "kept");
        fs::remove_file(dir.join(CONTEXT)).unwrap();
        ensure_context(&dir, &clock(), None);
        assert!(context::check(&context_of(&dir)).is_ok());
        fs::remove_dir_all(dir).unwrap();
    }
}
