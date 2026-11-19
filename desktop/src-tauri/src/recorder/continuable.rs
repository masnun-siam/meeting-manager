//! The last recording and whether it can be continued (PRD continue-recording FR-4, FR-14). The
//! recorder remembers its last recording in the store setting `last_recording` at every save
//! hand-off and at launch recovery, whatever the outcome; whether it can still be continued is
//! decided when the status is read. Built only with `FEATURE_CONTINUE_RECORDING=1`
//! (`Deps::continue_enabled`): off, nothing is written and nothing is offered.

use std::path::Path;

use meeting::Language;
use serde::{Deserialize, Serialize};

use super::Recorder;
use super::handoff::M4A;

const KEY: &str = "last_recording";
/// How long after its stop (or its recovery) a recording can be continued.
const WINDOW_MS: i64 = 2 * 3600 * 1000;

/// The value of the `last_recording` setting. `stopped_at` is Unix epoch milliseconds.
#[derive(Serialize, Deserialize, Debug, PartialEq)]
pub struct LastRecording {
    pub folder: String,
    pub recorded_s: u64,
    pub language: Language,
    pub stopped_at: i64,
    /// How many parts the meeting has; 1 until a recording is continued (#281).
    pub parts: u32,
}

/// What `recorder_status` offers to continue.
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Continuable {
    pub folder: String,
    /// The folder's last path component.
    pub name: String,
    pub recorded_s: u64,
    pub stopped_at: i64,
}

/// The FR-4 rule: offered while at most 2 hours have passed since the stop, the recorded time is
/// under the cap and the recording is still where it was saved. The surface being idle is the
/// caller's check.
pub fn offer(
    last: &LastRecording,
    now_ms: i64,
    cap_s: u64,
    recording_exists: bool,
) -> Option<Continuable> {
    let in_window = now_ms.saturating_sub(last.stopped_at) <= WINDOW_MS;
    (in_window && last.recorded_s < cap_s && recording_exists).then(|| Continuable {
        folder: last.folder.clone(),
        name: Path::new(&last.folder)
            .file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned()),
        recorded_s: last.recorded_s,
        stopped_at: last.stopped_at,
    })
}

/// The stored value; `None` when there is none, it is `null` or it cannot be parsed.
fn parse(raw: &str) -> Option<LastRecording> {
    serde_json::from_str::<Option<LastRecording>>(raw)
        .ok()
        .flatten()
}

impl Recorder {
    /// Remembers `folder` as the last recording, stopped now. `recorded_s` and `parts` are the
    /// meeting's totals, so a continued part (#281) writes them with the same call. A failed
    /// write is logged and never fails the recording.
    pub(super) fn remember_last(
        &self,
        folder: &Path,
        recorded_s: u64,
        language: Language,
        parts: u32,
    ) {
        if !self.inner.deps.continue_enabled {
            return;
        }
        let last = LastRecording {
            folder: folder.display().to_string(),
            recorded_s,
            language,
            stopped_at: (self.inner.deps.clock)().now_ms,
            parts,
        };
        self.write_last(Some(&last));
    }

    /// A new recording is the last one from its start (FR-4): the earlier one is no longer
    /// offered, even if the new one leaves no audio.
    pub(super) fn clear_last(&self) {
        if self.inner.deps.continue_enabled {
            self.write_last(None);
        }
    }

    /// The store has no delete, so `None` is written as JSON `null`.
    fn write_last(&self, last: Option<&LastRecording>) {
        let saved = serde_json::to_string(&last)
            .map_err(anyhow::Error::from)
            .and_then(|raw| self.inner.env.store()?.set_setting(KEY, &raw));
        if let Err(e) = saved {
            tracing::warn!(
                "recorder: last recording not saved: {}",
                meeting::store::error_kind(&e)
            );
        }
    }

    /// What can be continued now, for an idle recorder: `None` unless the flag is on and the last
    /// recording passes `offer`. One store read and one file check.
    pub(super) fn continuable(&self) -> Option<Continuable> {
        self.offered().map(|(_, offered)| offered)
    }

    /// The stored last recording as it is, whether or not it can still be continued; `None` when
    /// the flag is off, there is none or it cannot be read.
    pub(super) fn stored_last(&self) -> Option<LastRecording> {
        if !self.inner.deps.continue_enabled {
            return None;
        }
        let read = self.inner.env.store().and_then(|store| store.setting(KEY));
        match read {
            Ok(raw) => parse(&raw?),
            Err(e) => {
                tracing::warn!(
                    "recorder: last recording not read: {}",
                    meeting::store::error_kind(&e)
                );
                None
            }
        }
    }

    /// The stored last recording, re-checked by `offer` now: what `recorder_continue` continues.
    pub(super) fn last_continuable(&self) -> Option<LastRecording> {
        self.offered().map(|(last, _)| last)
    }

    fn offered(&self) -> Option<(LastRecording, Continuable)> {
        let last = self.stored_last()?;
        let exists = Path::new(&last.folder).join(M4A).is_file();
        let deps = &self.inner.deps;
        let offered = offer(&last, (deps.clock)().now_ms, deps.cap_s, exists)?;
        Some((last, offered))
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::fs;
    use std::path::PathBuf;

    use super::*;
    use crate::recorder::Reason;
    use crate::recorder::handoff::WAV;
    use crate::recorder::handoff::tests::clock;
    use crate::recorder::notice::Kind;
    use crate::recorder::tests::{Rig, SECOND, ok_capture, push, rig, rig_unflagged};

    const STOP: i64 = 1_794_528_000_000;
    const MINUTE: i64 = 60 * 1000;
    const CAP: u64 = 3 * 3600;

    fn last(recorded_s: u64) -> LastRecording {
        LastRecording {
            folder: "/r/meeting-2026-11-13_0600".into(),
            recorded_s,
            language: Language::En,
            stopped_at: STOP,
            parts: 1,
        }
    }

    #[test]
    fn offer_boundaries_with_a_fixed_clock() {
        let offered = |l: &LastRecording, now: i64, exists: bool| offer(l, now, CAP, exists);
        let l = last(600);
        assert_eq!(
            offered(&l, STOP + MINUTE, true),
            Some(Continuable {
                folder: "/r/meeting-2026-11-13_0600".into(),
                name: "meeting-2026-11-13_0600".into(),
                recorded_s: 600,
                stopped_at: STOP,
            })
        );
        assert!(offered(&l, STOP + WINDOW_MS, true).is_some());
        assert_eq!(offered(&l, STOP + WINDOW_MS + 1, true), None);
        assert_eq!(offered(&l, STOP + 121 * MINUTE, true), None);
        assert_eq!(offered(&l, STOP + MINUTE, false), None);
        assert!(offered(&last(CAP - 1), STOP + MINUTE, true).is_some());
        assert_eq!(offered(&last(CAP), STOP + MINUTE, true), None);
        assert_eq!(offered(&last(CAP + 1), STOP + MINUTE, true), None);
    }

    #[test]
    fn a_clock_set_back_still_offers() {
        // Measured on the wall clock from the stored stop: a stop "in the future" is in window.
        assert!(offer(&last(600), STOP - MINUTE, CAP, true).is_some());
    }

    #[test]
    fn invalid_or_missing_setting_is_none() {
        for raw in ["null", "", "{}", "[1]", r#"{"folder":"/r/a"}"#, "not json"] {
            assert_eq!(parse(raw), None, "{raw}");
        }
        let stored = serde_json::to_string(&last(5)).unwrap();
        assert_eq!(parse(&stored), Some(last(5)));
        let value: serde_json::Value = serde_json::from_str(&stored).unwrap();
        assert_eq!(
            value,
            serde_json::json!({
                "folder": "/r/meeting-2026-11-13_0600",
                "recorded_s": 5,
                "language": "en",
                "stopped_at": STOP,
                "parts": 1
            })
        );
    }

    fn stored(rig: &Rig) -> Option<String> {
        rig.env().store().unwrap().setting(KEY).unwrap()
    }

    pub(crate) fn store_last(rig: &Rig, last: &LastRecording) {
        let raw = serde_json::to_string(last).unwrap();
        rig.env().store().unwrap().set_setting(KEY, &raw).unwrap();
    }

    /// Records `secs` seconds and stops; the meeting folder.
    fn record(rig: &Rig, secs: usize) -> PathBuf {
        let folder = rig.start().unwrap();
        push(secs * SECOND);
        rig.stop(Reason::User);
        folder
    }

    /// `folder` as the last recording, stopped now.
    fn recording_in(folder: &Path, recorded_s: u64) -> LastRecording {
        LastRecording {
            folder: folder.display().to_string(),
            recorded_s,
            language: Language::En,
            stopped_at: clock().now_ms,
            parts: 1,
        }
    }

    fn offered(rig: &Rig) -> Option<String> {
        rig.recorder.status().continuable.map(|c| c.folder)
    }

    #[test]
    fn a_stop_remembers_the_last_recording() {
        let rig = rig("continuable-stop", ok_capture());
        let folder = record(&rig, 12);
        let path = folder.display().to_string();
        assert_eq!(
            parse(&stored(&rig).unwrap()),
            Some(LastRecording {
                folder: path.clone(),
                recorded_s: 12,
                language: Language::En,
                stopped_at: clock().now_ms,
                parts: 1,
            })
        );
        assert_eq!(
            rig.recorder.status().continuable,
            Some(Continuable {
                folder: path,
                name: folder.file_name().unwrap().to_string_lossy().into_owned(),
                recorded_s: 12,
                stopped_at: clock().now_ms,
            })
        );
    }

    #[test]
    fn a_new_recording_replaces_the_last_one() {
        let rig = rig("continuable-replace", ok_capture());
        record(&rig, 3);
        let second = record(&rig, 4);
        assert_eq!(offered(&rig).as_deref(), second.to_str());
    }

    #[test]
    fn a_new_start_clears_the_last_recording_at_once() {
        let rig = rig("continuable-clear", ok_capture());
        let first = record(&rig, 3);
        assert_eq!(offered(&rig).as_deref(), first.to_str());
        rig.start().unwrap();
        assert_eq!(stored(&rig).as_deref(), Some("null"));
        // The new recording leaves no audio: the first is still not offered.
        rig.stop(Reason::User);
        assert_ne!(offered(&rig).as_deref(), first.to_str());
    }

    #[test]
    fn status_has_no_continuable_while_recording_or_saving() {
        let rig = rig("continuable-busy", ok_capture());
        let first = record(&rig, 3);
        let shown = || rig.recorder.status().continuable;
        rig.start().unwrap();
        // The first recording stored again: only the state hides it now.
        store_last(&rig, &recording_in(&first, 3));
        assert_eq!(rig.state(), "recording");
        assert_eq!(shown(), None);
        rig.recorder.pause().unwrap();
        assert_eq!(shown(), None);
        rig.stop(Reason::User);
        store_last(&rig, &recording_in(&first, 3));
        let saving = rig.recorder.begin_saving().unwrap();
        assert_eq!(rig.state(), "saving");
        assert_eq!(shown(), None);
        drop(saving);
        assert_eq!(offered(&rig).as_deref(), first.to_str());
    }

    #[test]
    fn a_too_short_or_unsent_recording_is_continuable() {
        let rig = rig("continuable-short", ok_capture());
        let folder = rig.start().unwrap();
        push(5 * SECOND);
        assert_eq!(rig.stop(Reason::User).notice.unwrap().kind, Kind::TooShort);
        assert_eq!(offered(&rig).as_deref(), folder.to_str());

        // `meeting process` cannot be spawned: the setup notice.
        fs::remove_file(rig.home.join("meeting")).unwrap();
        let folder = rig.start().unwrap();
        push(11 * SECOND);
        assert_eq!(rig.stop(Reason::User).notice.unwrap().kind, Kind::Setup);
        assert_eq!(offered(&rig).as_deref(), folder.to_str());
    }

    #[test]
    fn an_archived_or_removed_recording_is_not_offered() {
        let rig = rig("continuable-archived", ok_capture());
        let folder = record(&rig, 3);
        let archive = rig.home.join("Recordings").join("Archive");
        fs::create_dir_all(&archive).unwrap();
        let archived = archive.join(folder.file_name().unwrap());
        fs::rename(&folder, &archived).unwrap();
        assert_eq!(offered(&rig), None);
        fs::rename(&archived, &folder).unwrap();
        assert_eq!(offered(&rig).as_deref(), folder.to_str());
        fs::remove_file(folder.join(M4A)).unwrap();
        assert_eq!(offered(&rig), None);
    }

    #[test]
    fn the_window_and_the_cap_apply_to_the_stored_recording() {
        let rig = rig("continuable-window", ok_capture());
        let folder = record(&rig, 3);
        let mut last = parse(&stored(&rig).unwrap()).unwrap();
        last.stopped_at = clock().now_ms - WINDOW_MS;
        store_last(&rig, &last);
        assert_eq!(offered(&rig).as_deref(), folder.to_str());
        last.stopped_at -= 1;
        store_last(&rig, &last);
        assert_eq!(offered(&rig), None);
        last.stopped_at = clock().now_ms;
        last.recorded_s = 3 * 3600;
        store_last(&rig, &last);
        assert_eq!(offered(&rig), None);
    }

    #[test]
    fn an_invalid_value_or_an_unreadable_store_offers_nothing() {
        let rig = rig("continuable-invalid", ok_capture());
        assert_eq!(rig.recorder.status().continuable, None);
        rig.env()
            .store()
            .unwrap()
            .set_setting(KEY, "not json")
            .unwrap();
        assert_eq!(rig.recorder.status().continuable, None);

        let env = rig.env();
        fs::write(&env.store_path, "this is not a database, not at all").unwrap();
        let status = rig.recorder.status();
        assert_eq!((status.state, status.continuable), ("idle", None));
        // A failed write never fails the recording.
        let folder = rig.start().unwrap();
        push(3 * SECOND);
        assert_eq!(rig.stop(Reason::User).notice.unwrap().kind, Kind::TooShort);
        assert!(folder.join(M4A).is_file());
    }

    #[test]
    fn flag_off_writes_nothing_and_reports_null() {
        let rig = rig_unflagged("continuable-off");
        let folder = record(&rig, 12);
        assert!(folder.join(M4A).is_file());
        assert_eq!(stored(&rig), None);
        rig.start().unwrap();
        assert_eq!(stored(&rig), None);
        rig.stop(Reason::User);
        // Even a value left by a flagged build is not offered.
        store_last(&rig, &recording_in(&folder, 12));
        let status = serde_json::to_value(rig.recorder.status()).unwrap();
        assert_eq!(status["continuable"], serde_json::Value::Null);
        assert!(status.as_object().unwrap().contains_key("continuable"));
        assert!(!folder.join(WAV).exists());
    }
}
