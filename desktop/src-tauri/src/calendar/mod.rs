//! The macOS calendar: access, the upcoming events, one event with its earlier notes, the hidden
//! calendars, change notifications and the lookup the recorder uses for `context.json`.
//! Attendee addresses never get past `eventkit.rs`; titles, names and notes are never logged.

mod eventkit;
pub mod model;
pub mod window;

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use meeting::context::CalendarEvent;
use meeting::store::{ListRow, Store, error_kind};
use serde::Serialize;
use serde_json::{Value, json};
use tauri::{AppHandle, Emitter, Manager, Runtime};

use crate::App;
use crate::env::Env;
use crate::reply;
use model::{Access, EventSource, Notify, RawEvent, clean, occurrence_parts, visible};

/// The store setting with the hidden calendars' identifiers, a JSON list.
const HIDDEN_KEY: &str = "hidden_calendars";
/// "Earlier notes" shows at most this many (FR-10).
const EARLIER_LIMIT: usize = 20;
/// The Tauri event the UI refetches on.
pub const CHANGED_EVENT: &str = "calendar-changed";
pub(crate) const DENIED_MESSAGE: &str =
    "Allow Meeting in System Settings › Privacy & Security › Calendars";

#[derive(Debug, PartialEq)]
pub enum CalendarError {
    /// Access is not granted.
    Denied,
    /// EventKit could not be read; a fixed message, safe to log.
    Failed(&'static str),
}

#[derive(Serialize, Debug, PartialEq)]
struct ListEvent {
    occurrence_id: String,
    series_id: String,
    title: String,
    start: String,
    end: String,
    calendar: Option<String>,
    has_notes: bool,
}

#[derive(Serialize, Debug, PartialEq)]
struct Day {
    date: String,
    events: Vec<ListEvent>,
}

#[derive(Serialize, Debug, PartialEq)]
struct CalendarInfo {
    id: String,
    title: String,
    account: String,
    hidden: bool,
}

pub struct Calendar {
    store_path: PathBuf,
    source: EventSource,
    /// Set by `on_launch`; called on every Calendar change once registered.
    notify: Mutex<Option<Notify>>,
    watching: AtomicBool,
}

impl Calendar {
    pub fn new(env: &Env) -> Calendar {
        Calendar::with(env, default_source())
    }

    pub fn with(env: &Env, source: EventSource) -> Calendar {
        Calendar {
            store_path: env.store_path.clone(),
            source,
            notify: Mutex::new(None),
            watching: AtomicBool::new(false),
        }
    }

    fn access(&self) -> Access {
        (self.source.access)()
    }

    /// The access, prompting once when it was never asked (FR-8: on first open of the list).
    fn access_or_ask(&self) -> Access {
        match self.access() {
            Access::NotAsked => {
                let access = (self.source.request)();
                tracing::info!("calendar: access asked, now {}", access.as_str());
                self.watch();
                access
            }
            access => access,
        }
    }

    fn store(&self) -> anyhow::Result<Store> {
        Store::open(&self.store_path)
    }

    fn hidden(&self) -> HashSet<String> {
        let read = self.store().and_then(|s| s.setting(HIDDEN_KEY));
        match read {
            Ok(None) => HashSet::new(),
            Ok(Some(raw)) => serde_json::from_str::<Vec<String>>(&raw)
                .map(HashSet::from_iter)
                .unwrap_or_else(|_| {
                    tracing::warn!("calendar: hidden calendars setting is not a JSON list");
                    HashSet::new()
                }),
            Err(e) => {
                tracing::warn!("calendar: hidden calendars not read: {}", error_kind(&e));
                HashSet::new()
            }
        }
    }

    pub fn status_reply(&self) -> Value {
        reply::ok(json!({"access": self.access().as_str()}))
    }

    pub fn events_reply(&self) -> Value {
        if self.access_or_ask() != Access::Granted {
            return denied();
        }
        let (from, to) = window::window((self.source.now)(), self.source.offset_min);
        let raw = match (self.source.events)(from, to) {
            Ok(raw) => raw,
            Err(e) => return failed(e),
        };
        let hidden = self.hidden();
        let mut shown: Vec<&RawEvent> = raw
            .iter()
            .filter(|e| (from..to).contains(&e.start) && visible(e, &hidden))
            .collect();
        shown.sort_by(|a, b| (a.start, &a.title).cmp(&(b.start, &b.title)));
        let mut days: BTreeMap<String, Vec<ListEvent>> = BTreeMap::new();
        let mut skipped = 0;
        for e in shown {
            let Some(event) = clean(e, self.source.offset_min) else {
                skipped += 1;
                continue;
            };
            let date = window::local_date(e.start, (self.source.offset_min)(e.start));
            days.entry(date).or_default().push(ListEvent {
                has_notes: event.description.is_some(),
                occurrence_id: event.occurrence_id,
                series_id: event.series_id,
                title: event.title,
                start: event.start,
                end: event.end,
                calendar: event.calendar,
            });
        }
        let listed: usize = days.values().map(Vec::len).sum();
        tracing::info!(
            "calendar: {listed} events listed, {} hidden or filtered, {skipped} with long ids",
            raw.len() - listed - skipped
        );
        let days: Vec<Day> = days
            .into_iter()
            .map(|(date, events)| Day { date, events })
            .collect();
        reply::ok(json!({"days": days}))
    }

    /// The event for `context.json`, by occurrence id, without the list's filters or window:
    /// the user picked it. `Ok(None)` when it is unknown, malformed or deleted.
    pub fn event_for_context(
        &self,
        occurrence_id: &str,
    ) -> Result<Option<CalendarEvent>, CalendarError> {
        if self.access() != Access::Granted {
            return Err(CalendarError::Denied);
        }
        let Some((identifier, start)) = occurrence_parts(occurrence_id) else {
            return Ok(None);
        };
        // `eventWithIdentifier:` returns a series' first occurrence: match the start instead.
        let events =
            (self.source.events)(start, start.saturating_add(1)).map_err(CalendarError::Failed)?;
        Ok(events
            .iter()
            .find(|e| e.identifier == identifier && e.start == start)
            .and_then(|e| clean(e, self.source.offset_min)))
    }

    pub fn event_reply(&self, occurrence_id: &str) -> Value {
        let event = match self.event_for_context(occurrence_id) {
            Ok(Some(event)) => event,
            Ok(None) => return reply::err("unknown_event", "This event no longer exists"),
            Err(CalendarError::Denied) => return denied(),
            Err(CalendarError::Failed(e)) => return failed(e),
        };
        let earlier = match self
            .store()
            .and_then(|s| s.earlier(&event.series_id, None, EARLIER_LIMIT))
        {
            Ok(rows) => rows.iter().map(earlier_note).collect(),
            Err(e) => {
                tracing::warn!("calendar: earlier notes not read: {}", error_kind(&e));
                Vec::new()
            }
        };
        reply::ok(json!({"event": event, "earlier": earlier}))
    }

    fn calendar_list(&self, hidden: &HashSet<String>) -> Vec<CalendarInfo> {
        if self.access() != Access::Granted {
            return Vec::new();
        }
        let mut list: Vec<CalendarInfo> = (self.source.calendars)()
            .into_iter()
            .map(|c| CalendarInfo {
                hidden: hidden.contains(&c.id),
                id: c.id,
                title: c.title,
                account: c.account,
            })
            .collect();
        list.sort_by_cached_key(|c| (c.account.to_lowercase(), c.title.to_lowercase()));
        list
    }

    pub fn calendars_reply(&self) -> Value {
        reply::ok(json!({"calendars": self.calendar_list(&self.hidden())}))
    }

    pub fn set_hidden_reply(&self, ids: Vec<String>) -> Value {
        let saved = serde_json::to_string(&ids)
            .map_err(anyhow::Error::from)
            .and_then(|raw| self.store()?.set_setting(HIDDEN_KEY, &raw));
        if let Err(e) = saved {
            tracing::error!("calendar: hidden calendars not saved: {}", error_kind(&e));
            return reply::err("store_unavailable", format!("{e:#}"));
        }
        reply::ok(json!({"calendars": self.calendar_list(&HashSet::from_iter(ids))}))
    }

    fn set_notify(&self, notify: Notify) {
        *self.notify.lock().unwrap_or_else(PoisonError::into_inner) = Some(notify);
    }

    /// Registers for change notifications once, when access is granted and `on_launch` ran.
    fn watch(&self) {
        let Some(notify) = self
            .notify
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
        else {
            return;
        };
        if self.access() != Access::Granted || self.watching.swap(true, Ordering::SeqCst) {
            return;
        }
        if !(self.source.watch)(notify) {
            tracing::error!("calendar: change notifications not registered");
            self.watching.store(false, Ordering::SeqCst);
        }
    }
}

/// EventKit in the app. In this crate's tests access reads as denied, so no test binary shows
/// the system prompt or reaches EventKit: every other call checks access first.
fn default_source() -> EventSource {
    let real = eventkit::real();
    if cfg!(test) {
        EventSource {
            access: no_access,
            request: no_access,
            ..real
        }
    } else {
        real
    }
}

fn no_access() -> Access {
    Access::Denied
}

fn denied() -> Value {
    reply::err("calendar_denied", DENIED_MESSAGE)
}

fn failed(kind: &'static str) -> Value {
    tracing::error!("calendar: {kind}");
    reply::err(
        "calendar_failed",
        "Calendar could not be read; quit and reopen Meeting",
    )
}

fn earlier_note(row: &ListRow) -> Value {
    let title = row
        .title
        .clone()
        .unwrap_or_else(|| folder_name(&row.recording_path));
    json!({"id": row.id, "title": title, "recorded_at": row.recorded_at})
}

/// The meeting folder's name: the recording's parent directory.
fn folder_name(recording: &Path) -> String {
    recording
        .parent()
        .and_then(Path::file_name)
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Runs `f` on the calendar off the main and async threads: EventKit calls block.
async fn blocking<R: Runtime>(
    app: AppHandle<R>,
    f: impl FnOnce(&Calendar) -> Value + Send + 'static,
) -> Value {
    // `launch` manages the state before it opens the window, so no command can run without it.
    let ran = tauri::async_runtime::spawn_blocking(move || f(&app.state::<App>().calendar)).await;
    ran.unwrap_or_else(|_| failed("a calendar command panicked"))
}

/// Called once at startup: registers for Calendar changes when access is granted (or later,
/// when it is granted on the first request) and emits `calendar-changed` to every window.
pub fn on_launch<R: Runtime>(app: &AppHandle<R>) {
    let Some(state) = app.try_state::<App>() else {
        return;
    };
    let handle = app.clone();
    state.calendar.set_notify(Arc::new(move || {
        if handle.emit(CHANGED_EVENT, ()).is_err() {
            tracing::warn!("calendar: change event not emitted");
        }
    }));
    state.calendar.watch();
}

#[tauri::command(rename_all = "snake_case")]
pub async fn calendar_status<R: Runtime>(app: AppHandle<R>) -> Value {
    blocking(app, Calendar::status_reply).await
}

#[tauri::command(rename_all = "snake_case")]
pub async fn calendar_events<R: Runtime>(app: AppHandle<R>) -> Value {
    blocking(app, Calendar::events_reply).await
}

#[tauri::command(rename_all = "snake_case")]
pub async fn calendar_event<R: Runtime>(app: AppHandle<R>, occurrence_id: String) -> Value {
    blocking(app, move |c| c.event_reply(&occurrence_id)).await
}

#[tauri::command(rename_all = "snake_case")]
pub async fn calendars<R: Runtime>(app: AppHandle<R>) -> Value {
    blocking(app, Calendar::calendars_reply).await
}

#[tauri::command(rename_all = "snake_case")]
pub async fn set_hidden_calendars<R: Runtime>(app: AppHandle<R>, ids: Vec<String>) -> Value {
    blocking(app, move |c| c.set_hidden_reply(ids)).await
}

#[cfg(test)]
pub(crate) mod tests {
    use std::fs;
    use std::sync::atomic::AtomicUsize;

    use meeting::calendar::Response;
    use meeting::store::{EventLink, RunResult, Source, Status};

    use super::model::tests::{person, raw};
    use super::model::{PersonKind, RawCalendar, RawPerson};
    use super::*;

    /// 2026-11-13T10:00:00+06:00.
    const NOW: i64 = 1_794_528_000 + 4 * 3600;
    /// 2026-11-13T00:00:00+06:00.
    const TODAY: i64 = 1_794_528_000 - 6 * 3600;
    const HOUR: i64 = 3600;
    const DAY: i64 = 86_400;

    pub fn denied() -> Access {
        Access::Denied
    }
    fn granted() -> Access {
        Access::Granted
    }
    fn not_asked() -> Access {
        Access::NotAsked
    }
    fn fixed_now() -> i64 {
        NOW
    }
    fn dhaka(_: i64) -> i32 {
        360
    }
    fn no_calendars() -> Vec<RawCalendar> {
        Vec::new()
    }
    fn no_events(_: i64, _: i64) -> Result<Vec<RawEvent>, &'static str> {
        Ok(Vec::new())
    }
    fn broken_events(_: i64, _: i64) -> Result<Vec<RawEvent>, &'static str> {
        Err("the calendar worker stopped")
    }
    fn watch_ok(_: Notify) -> bool {
        true
    }

    pub fn source(access: fn() -> Access) -> EventSource {
        EventSource {
            access,
            request: denied,
            calendars: no_calendars,
            events: no_events,
            now: fixed_now,
            offset_min: dhaka,
            watch: watch_ok,
        }
    }

    fn with_attendees(mut e: RawEvent) -> RawEvent {
        e.organizer = Some(person(
            "Rahim <rahim@example.com>",
            PersonKind::Person,
            Response::Accepted,
        ));
        e.attendees = vec![
            person("Karim", PersonKind::Person, Response::Accepted),
            person("rahim@example.com", PersonKind::Person, Response::Accepted),
            person(
                "Rahim <rahim@example.com>",
                PersonKind::Person,
                Response::Tentative,
            ),
            person("Room 4", PersonKind::Room, Response::Accepted),
        ];
        e
    }

    /// The fake calendar: what EventKit would return for `[from, to)`.
    fn all_events() -> Vec<RawEvent> {
        let mut list = Vec::new();
        // Today, already over; and one later today with notes and attendees.
        list.push(raw("past", TODAY + 8 * HOUR, "work"));
        let mut sync = with_attendees(raw("sync", TODAY + 15 * HOUR, "work"));
        sync.notes = Some(" agenda ".into());
        list.push(sync);
        // Same start as `sync`: sorted by title.
        let mut a = raw("a-first", TODAY + 15 * HOUR, "home");
        a.title = "A first".into();
        a.notes = Some("  \n".into());
        list.push(a);
        // Started yesterday, still running: not listed.
        list.push(raw("yesterday", TODAY - 2 * HOUR, "work"));
        // A two-day event from tomorrow: on its start day only.
        let mut long = raw("long", TODAY + DAY + 10 * HOUR, "work");
        long.end = long.start + 2 * DAY;
        list.push(long);
        let mut all_day = raw("allday", TODAY + DAY, "work");
        all_day.all_day = true;
        list.push(all_day);
        let mut declined = raw("declined", TODAY + DAY + 12 * HOUR, "work");
        declined.attendees = vec![RawPerson {
            is_current_user: true,
            ..person("Me", PersonKind::Person, Response::Declined)
        }];
        declined.my_response = Some(Response::Declined);
        list.push(declined);
        list.push(raw("hidden", TODAY + 2 * DAY, "private"));
        // A weekly series: three occurrences, the last on the 7th following day.
        for week in 0..3 {
            let mut e = raw("weekly", TODAY + 9 * HOUR + week * 7 * DAY / 2, "work");
            e.series_id = "series-weekly".into();
            list.push(e);
        }
        // The last minute of the window, and the first after it.
        list.push(raw("last", TODAY + 8 * DAY - 60, "work"));
        list.push(raw("after", TODAY + 8 * DAY, "work"));
        list
    }

    fn fake_events(from: i64, to: i64) -> Result<Vec<RawEvent>, &'static str> {
        Ok(all_events()
            .into_iter()
            .filter(|e| e.start < to && e.end > from)
            .collect())
    }

    fn fake_calendars() -> Vec<RawCalendar> {
        let cal = |id: &str, title: &str, account: &str| RawCalendar {
            id: id.into(),
            title: title.into(),
            account: account.into(),
        };
        vec![
            cal("work", "Work", "Google"),
            cal("private", "Private", "iCloud"),
            cal("home", "Home", "iCloud"),
            cal("birthdays", "Birthdays", "Other"),
        ]
    }

    fn full() -> EventSource {
        EventSource {
            calendars: fake_calendars,
            events: fake_events,
            ..source(granted)
        }
    }

    struct Home(PathBuf);

    impl Drop for Home {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn home(name: &str) -> (Home, Env) {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "meeting-desktop-calendar-{name}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        let env = Env::at(dir.clone());
        (Home(dir), env)
    }

    fn calendar(name: &str, source: EventSource) -> (Home, Calendar) {
        let (home, env) = home(name);
        (home, Calendar::with(&env, source))
    }

    fn ids(reply: &Value) -> Vec<Vec<String>> {
        reply["days"]
            .as_array()
            .unwrap()
            .iter()
            .map(|d| {
                d["events"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|e| {
                        let id = e["occurrence_id"].as_str().unwrap();
                        id.split('|').next().unwrap().to_string()
                    })
                    .collect()
            })
            .collect()
    }

    #[test]
    fn status_maps_the_access_and_never_asks() {
        for (access, text) in [
            (not_asked as fn() -> Access, "not_asked"),
            (granted, "granted"),
            (denied, "denied"),
        ] {
            let source = EventSource {
                request: || panic!("status must not prompt"),
                ..source(access)
            };
            let (_home, c) = calendar("status", source);
            assert_eq!(c.status_reply(), json!({"ok": true, "access": text}));
        }
    }

    #[test]
    fn denied_answers_calendar_denied_and_no_calendars() {
        let (_home, c) = calendar(
            "denied",
            EventSource {
                calendars: fake_calendars,
                events: fake_events,
                ..source(denied)
            },
        );
        for reply in [c.events_reply(), c.event_reply("weekly|1")] {
            assert_eq!(reply["ok"], false);
            assert_eq!(reply["code"], "calendar_denied");
            let message = reply["message"].as_str().unwrap();
            assert!(
                message.contains("System Settings › Privacy & Security › Calendars"),
                "{message}"
            );
        }
        assert_eq!(c.calendars_reply(), json!({"ok": true, "calendars": []}));
        assert_eq!(c.event_for_context("weekly|1"), Err(CalendarError::Denied));
    }

    static ASKED: AtomicUsize = AtomicUsize::new(0);
    static WATCHED: AtomicUsize = AtomicUsize::new(0);
    static GRANTED: AtomicBool = AtomicBool::new(false);

    fn ask_once() -> Access {
        ASKED.fetch_add(1, Ordering::SeqCst);
        GRANTED.store(true, Ordering::SeqCst);
        Access::Granted
    }
    fn after_ask() -> Access {
        if GRANTED.load(Ordering::SeqCst) {
            Access::Granted
        } else {
            Access::NotAsked
        }
    }
    fn count_watch(notify: Notify) -> bool {
        WATCHED.fetch_add(1, Ordering::SeqCst);
        notify();
        true
    }

    #[test]
    fn the_list_asks_once_then_lists_and_starts_watching() {
        let (_home, c) = calendar(
            "ask",
            EventSource {
                access: after_ask,
                request: ask_once,
                watch: count_watch,
                ..full()
            },
        );
        let notified = Arc::new(AtomicUsize::new(0));
        let seen = notified.clone();
        c.set_notify(Arc::new(move || {
            seen.fetch_add(1, Ordering::SeqCst);
        }));
        // Not asked yet: on_launch's watch does nothing.
        c.watch();
        assert_eq!(WATCHED.load(Ordering::SeqCst), 0);
        assert_eq!(c.events_reply()["ok"], true);
        assert_eq!(c.events_reply()["ok"], true);
        c.watch();
        assert_eq!(ASKED.load(Ordering::SeqCst), 1);
        assert_eq!(WATCHED.load(Ordering::SeqCst), 1);
        assert_eq!(notified.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_refused_request_answers_denied() {
        let (_home, c) = calendar("refused", source(not_asked));
        assert_eq!(c.events_reply()["code"], "calendar_denied");
    }

    #[test]
    fn the_list_groups_filters_and_sorts() {
        let (_home, c) = calendar("list", full());
        c.store()
            .unwrap()
            .set_setting(HIDDEN_KEY, r#"["private"]"#)
            .unwrap();
        let reply = c.events_reply();
        assert_eq!(reply["ok"], true);
        let dates: Vec<_> = reply["days"]
            .as_array()
            .unwrap()
            .iter()
            .map(|d| d["date"].as_str().unwrap())
            .collect();
        assert_eq!(
            dates,
            ["2026-11-13", "2026-11-14", "2026-11-16", "2026-11-20"]
        );
        assert_eq!(
            ids(&reply),
            [
                vec!["past", "weekly", "a-first", "sync"],
                vec!["long"],
                vec!["weekly"],
                vec!["weekly", "last"],
            ]
        );
        let sync = &reply["days"][0]["events"][3];
        assert_eq!(
            sync,
            &json!({
                "occurrence_id": format!("sync|{}", TODAY + 15 * HOUR),
                "series_id": "series-sync",
                "title": "Event sync",
                "start": "2026-11-13T15:00:00+06:00",
                "end": "2026-11-13T16:00:00+06:00",
                "calendar": "Calendar work",
                "has_notes": true,
            })
        );
        assert_eq!(reply["days"][0]["events"][2]["has_notes"], false);
        let weekly: Vec<_> = reply["days"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|d| d["events"].as_array().unwrap())
            .filter(|e| e["series_id"] == "series-weekly")
            .map(|e| e["occurrence_id"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(weekly.len(), 3);
        assert_eq!(weekly.iter().collect::<HashSet<_>>().len(), 3);
        assert!(!reply.to_string().contains('@'));
    }

    #[test]
    fn hiding_a_calendar_removes_its_events_and_persists() {
        let (home, env) = home("hide");
        let c = Calendar::with(&env, full());
        let listed = |c: &Calendar| ids(&c.events_reply()).concat();
        assert!(listed(&c).contains(&"sync".to_string()));
        assert!(listed(&c).contains(&"hidden".to_string()));
        let reply = c.set_hidden_reply(vec!["work".into(), "gone".into()]);
        assert_eq!(reply["ok"], true);
        assert_eq!(
            c.store().unwrap().setting(HIDDEN_KEY).unwrap().as_deref(),
            Some(r#"["work","gone"]"#)
        );
        // Re-opened, as after a relaunch.
        let again = Calendar::with(&env, full());
        assert_eq!(listed(&again), ["a-first", "hidden"]);
        let calendars = again.calendars_reply();
        assert_eq!(calendars, reply);
        drop(home);
    }

    #[test]
    fn calendars_are_sorted_with_their_hidden_flag() {
        let (_home, c) = calendar("calendars", full());
        let reply = c.set_hidden_reply(vec!["home".into()]);
        assert_eq!(
            reply,
            json!({"ok": true, "calendars": [
                {"id": "work", "title": "Work", "account": "Google", "hidden": false},
                {"id": "home", "title": "Home", "account": "iCloud", "hidden": true},
                {"id": "private", "title": "Private", "account": "iCloud", "hidden": false},
                {"id": "birthdays", "title": "Birthdays", "account": "Other", "hidden": false},
            ]})
        );
        let reply = c.set_hidden_reply(vec![]);
        assert!(
            reply["calendars"]
                .as_array()
                .unwrap()
                .iter()
                .all(|c| c["hidden"] == false)
        );
    }

    #[test]
    fn a_bad_hidden_setting_hides_nothing() {
        let (_home, c) = calendar("bad-setting", full());
        c.store()
            .unwrap()
            .set_setting(HIDDEN_KEY, "not json")
            .unwrap();
        assert!(ids(&c.events_reply()).concat().contains(&"hidden".into()));
    }

    #[test]
    fn a_read_failure_is_reported() {
        let (_home, c) = calendar(
            "broken",
            EventSource {
                events: broken_events,
                ..source(granted)
            },
        );
        assert_eq!(c.events_reply()["code"], "calendar_failed");
        assert_eq!(c.event_reply("a|1")["code"], "calendar_failed");
        assert_eq!(
            c.event_for_context("a|1"),
            Err(CalendarError::Failed("the calendar worker stopped"))
        );
    }

    #[test]
    fn the_lookup_finds_the_right_occurrence_without_the_list_filters() {
        let (_home, c) = calendar("find", full());
        let second = TODAY + 9 * HOUR + 7 * DAY / 2;
        let event = c
            .event_for_context(&format!("weekly|{second}"))
            .unwrap()
            .unwrap();
        assert_eq!(event.start, "2026-11-16T21:00:00+06:00");
        assert_eq!(event.end, "2026-11-16T22:00:00+06:00");
        assert_eq!(event.series_id, "series-weekly");
        // Declined, hidden or outside the window: still found.
        for id in ["declined", "hidden", "after"] {
            let start = all_events()
                .into_iter()
                .find(|e| e.identifier == id)
                .unwrap()
                .start;
            let found = c.event_for_context(&format!("{id}|{start}")).unwrap();
            assert!(found.is_some(), "{id}");
        }
        for unknown in [
            "weekly|1".to_string(),
            format!("nothing|{second}"),
            format!("weekly|{}", second + 1),
            String::new(),
            "weekly".into(),
            "weekly|soon".into(),
        ] {
            assert_eq!(c.event_for_context(&unknown), Ok(None), "{unknown}");
            assert_eq!(c.event_reply(&unknown)["code"], "unknown_event");
        }
    }

    #[test]
    fn the_lookup_returns_names_only() {
        let (_home, c) = calendar("context", full());
        let id = format!("sync|{}", TODAY + 15 * HOUR);
        let event = c.event_for_context(&id).unwrap().unwrap();
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(
            json,
            json!({
                "occurrence_id": id,
                "series_id": "series-sync",
                "title": "Event sync",
                "start": "2026-11-13T15:00:00+06:00",
                "end": "2026-11-13T16:00:00+06:00",
                "organizer": null,
                "calendar": "Calendar work",
                "attendees": [{"name": "Karim", "response": "accepted"}],
                "description": "agenda",
            })
        );
        // The tool takes it unchanged.
        let mut context = json!({
            "version": 1, "platform": "desktop", "url": null, "meeting_id": null,
            "tab_title": null, "meeting_title": null, "started_at_ms": 0,
            "utc_offset_min": 360, "time_zone": "Asia/Dhaka", "participants": [],
            "speaking": [], "truncated": [],
        });
        context["calendar_event"] = json;
        let checked = meeting::context::check(&context).unwrap();
        assert_eq!(checked.calendar_event, Some(event));
        assert!(checked.truncated.is_empty());
        let detail = c.event_reply(&id);
        assert!(!detail.to_string().contains('@'), "{detail}");
    }

    fn record(store: &Store, folder: &str, at: &str, series: &str, title: Option<&str>) -> i64 {
        let result = RunResult {
            status: Status::Done,
            failed_stage: None,
            error: None,
            title: title.map(String::from),
            body_md: None,
            speakers: vec![],
            language: None,
            duration_s: None,
            transcript: None,
            event: Some(EventLink {
                occurrence_id: format!("{series}|{folder}"),
                series_id: series.into(),
                title: "Weekly".into(),
                start: at.into(),
                end: at.into(),
                calendar: None,
            }),
            source: Source::Desktop,
            recorded_at: at.into(),
            attendees: None,
        };
        let path = PathBuf::from(format!("/rec/{folder}/recording.m4a"));
        store.merge_run(&path, &result).unwrap().id
    }

    #[test]
    fn the_detail_lists_twenty_earlier_notes_newest_first() {
        let (_home, c) = calendar("earlier", full());
        let store = c.store().unwrap();
        for day in 1..=25 {
            let title = (day != 25).then(|| format!("Sync {day}"));
            record(
                &store,
                &format!("2026-10-{day:02} Sync"),
                &format!("2026-10-{day:02}T09:00:00+06:00"),
                "series-weekly",
                title.as_deref(),
            );
        }
        record(
            &store,
            "other",
            "2026-10-30T09:00:00+06:00",
            "series-x",
            None,
        );
        let id = format!("weekly|{}", TODAY + 9 * HOUR);
        let reply = c.event_reply(&id);
        assert_eq!(reply["ok"], true);
        assert_eq!(reply["event"]["occurrence_id"], id);
        assert_eq!(reply["event"]["calendar"], "Calendar work");
        let earlier = reply["earlier"].as_array().unwrap();
        assert_eq!(earlier.len(), 20);
        assert_eq!(earlier[0]["title"], "2026-10-25 Sync");
        assert_eq!(earlier[0]["recorded_at"], "2026-10-25T09:00:00+06:00");
        assert_eq!(earlier[1]["title"], "Sync 24");
        assert_eq!(earlier[19]["title"], "Sync 6");
        assert!(earlier[0]["id"].is_i64());
    }

    #[test]
    fn an_unopenable_store_gives_the_event_without_earlier_notes() {
        let (_home, c) = calendar("corrupt", full());
        fs::create_dir_all(c.store_path.parent().unwrap()).unwrap();
        fs::write(&c.store_path, "this is not a database, not at all").unwrap();
        let reply = c.event_reply(&format!("weekly|{}", TODAY + 9 * HOUR));
        assert_eq!(reply["ok"], true);
        assert_eq!(reply["earlier"], json!([]));
        assert_eq!(c.set_hidden_reply(vec![])["code"], "store_unavailable");
    }

    #[test]
    fn the_folder_name_is_the_recordings_parent() {
        assert_eq!(
            folder_name(Path::new("/r/2026-11-13 Sync/a.m4a")),
            "2026-11-13 Sync"
        );
        assert_eq!(folder_name(Path::new("a.m4a")), "");
    }

    /// A mock app with the `App` state around `calendar`, as `launch` manages it, and a counter
    /// of the `calendar-changed` events it emits.
    fn launched(
        name: &str,
        source: EventSource,
    ) -> (Home, tauri::App<tauri::test::MockRuntime>, Arc<AtomicUsize>) {
        use tauri::Listener;
        let (home, env) = home(name);
        let app = tauri::test::mock_app();
        app.manage(App {
            recorder: crate::recorder::Recorder::new(&env),
            calendar: Calendar::with(&env, source),
            notes: crate::notes::Notes::new(&env),
            admin: crate::admin::Admin::new(&env),
            updater: crate::updater::tests::unplugged(app.handle()),
            env,
        });
        let changed = Arc::new(AtomicUsize::new(0));
        let seen = changed.clone();
        app.listen_any(CHANGED_EVENT, move |_| {
            seen.fetch_add(1, Ordering::SeqCst);
        });
        (home, app, changed)
    }

    /// Fires the change notification as soon as it is registered.
    fn fire_on_watch(notify: Notify) -> bool {
        notify();
        true
    }

    #[test]
    fn on_launch_emits_calendar_changed_on_a_change() {
        let (_home, app, changed) = launched(
            "launch",
            EventSource {
                watch: fire_on_watch,
                ..source(granted)
            },
        );
        on_launch(app.handle());
        assert_eq!(changed.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn on_launch_does_nothing_without_access() {
        fn must_not_watch(_: Notify) -> bool {
            panic!("watched without access")
        }
        for access in [denied as fn() -> Access, not_asked] {
            let (_home, app, changed) = launched(
                "launch-denied",
                EventSource {
                    watch: must_not_watch,
                    ..source(access)
                },
            );
            on_launch(app.handle());
            assert_eq!(changed.load(Ordering::SeqCst), 0);
        }
    }

    #[test]
    fn watching_needs_access_and_registers_once() {
        static COUNT: AtomicUsize = AtomicUsize::new(0);
        fn counting(_: Notify) -> bool {
            COUNT.fetch_add(1, Ordering::SeqCst);
            true
        }
        let (_home, c) = calendar(
            "watch",
            EventSource {
                watch: counting,
                ..source(granted)
            },
        );
        // No notify yet: nothing to register.
        c.watch();
        assert_eq!(COUNT.load(Ordering::SeqCst), 0);
        c.set_notify(Arc::new(|| {}));
        c.watch();
        c.watch();
        assert_eq!(COUNT.load(Ordering::SeqCst), 1);
        let (_home, d) = calendar(
            "watch-denied",
            EventSource {
                watch: counting,
                ..source(denied)
            },
        );
        d.set_notify(Arc::new(|| {}));
        d.watch();
        assert_eq!(COUNT.load(Ordering::SeqCst), 1);
    }
}
