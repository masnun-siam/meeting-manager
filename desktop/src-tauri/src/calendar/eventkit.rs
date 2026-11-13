//! The real `EventSource`: EventKit, the only Objective-C in the module. Mapping only; no test
//! here can reach EventKit (no permission in CI), so the rules live in `model.rs`.
//!
//! `EKEventStore` is not documented as thread-safe, so one worker thread owns it and every call
//! sends it a job. EventKit objects never leave that thread: a job returns plain data.

use std::ptr::NonNull;
use std::sync::OnceLock;
use std::sync::mpsc::{self, Sender};
use std::time::{SystemTime, UNIX_EPOCH};

use block2::RcBlock;
use meeting::calendar::Response;
use objc2::rc::{Retained, autoreleasepool};
use objc2::runtime::Bool;
use objc2_event_kit::{
    EKAuthorizationStatus, EKEntityType, EKEvent, EKEventStore, EKEventStoreChangedNotification,
    EKParticipant, EKParticipantStatus, EKParticipantType,
};
use objc2_foundation::{NSDate, NSError, NSNotification, NSNotificationCenter, NSTimeZone};

use super::model::{Access, EventSource, Notify, PersonKind, RawCalendar, RawEvent, RawPerson};

const WORKER_GONE: &str = "the calendar worker stopped";

pub fn real() -> EventSource {
    EventSource {
        access,
        request,
        calendars,
        events,
        now,
        offset_min,
        watch,
    }
}

struct Worker {
    store: Retained<EKEventStore>,
}

type Job = Box<dyn FnOnce(&mut Worker) + Send>;

/// Runs `job` on the worker thread (started on first use) and waits for its result.
fn run<T: Send + 'static>(
    job: impl FnOnce(&mut Worker) -> T + Send + 'static,
) -> Result<T, &'static str> {
    static JOBS: OnceLock<Sender<Job>> = OnceLock::new();
    let jobs = JOBS.get_or_init(start_worker);
    let (reply, answer) = mpsc::channel();
    jobs.send(Box::new(move |w| {
        let _ = reply.send(job(w));
    }))
    .map_err(|_| WORKER_GONE)?;
    answer.recv().map_err(|_| WORKER_GONE)
}

fn start_worker() -> Sender<Job> {
    let (jobs, queue) = mpsc::channel::<Job>();
    let started = std::thread::Builder::new()
        .name("eventkit".into())
        .spawn(move || {
            // SAFETY: `new` has no preconditions; the store stays on this thread.
            let mut worker = Worker {
                store: unsafe { EKEventStore::new() },
            };
            for job in queue {
                autoreleasepool(|_| job(&mut worker));
            }
        });
    if let Err(e) = started {
        // The queue was moved into the closure and dropped with it, so every send fails.
        tracing::error!("calendar: the EventKit thread did not start: {}", e.kind());
    }
    jobs
}

fn access() -> Access {
    // SAFETY: a class method with a valid enum value.
    let status = unsafe { EKEventStore::authorizationStatusForEntityType(EKEntityType::Event) };
    access_of(status)
}

fn access_of(status: EKAuthorizationStatus) -> Access {
    match status {
        EKAuthorizationStatus::NotDetermined => Access::NotAsked,
        EKAuthorizationStatus::FullAccess => Access::Granted,
        // Restricted, denied and write-only all mean no events to read.
        _ => Access::Denied,
    }
}

fn request() -> Access {
    let asked = run(|w| {
        let (tx, rx) = mpsc::channel();
        let done = RcBlock::new(move |granted: Bool, _error: *mut NSError| {
            let _ = tx.send(granted.as_bool());
        });
        // SAFETY: `done` is a valid block with the handler's signature; EventKit copies it.
        unsafe {
            w.store
                .requestFullAccessToEventsWithCompletion(RcBlock::as_ptr(&done))
        };
        // The completion runs on another queue: waiting here does not block it.
        if rx.recv().unwrap_or(false) {
            // A store made before the grant may hold no calendars: start from a fresh one.
            // SAFETY: as in `start_worker`.
            w.store = unsafe { EKEventStore::new() };
        }
    });
    if let Err(e) = asked {
        tracing::error!("calendar: access request failed: {e}");
    }
    access()
}

fn calendars() -> Vec<RawCalendar> {
    run(|w| {
        // SAFETY: reading properties of objects owned by this thread's store.
        unsafe {
            w.store
                .calendarsForEntityType(EKEntityType::Event)
                .iter()
                .map(|c| RawCalendar {
                    id: c.calendarIdentifier().to_string(),
                    title: c.title().to_string(),
                    account: c
                        .source()
                        .map(|s| s.title().to_string())
                        .unwrap_or_default(),
                })
                .collect()
        }
    })
    .unwrap_or_else(|e| {
        tracing::error!("calendar: listing calendars failed: {e}");
        Vec::new()
    })
}

fn events(from: i64, to: i64) -> Result<Vec<RawEvent>, &'static str> {
    run(move |w| {
        let from = NSDate::dateWithTimeIntervalSince1970(from as f64);
        let to = NSDate::dateWithTimeIntervalSince1970(to as f64);
        // SAFETY: the predicate comes from this store; recurrences are expanded by EventKit.
        unsafe {
            let predicate = w
                .store
                .predicateForEventsWithStartDate_endDate_calendars(&from, &to, None);
            w.store
                .eventsMatchingPredicate(&predicate)
                .iter()
                .filter_map(|e| raw_event(&e))
                .collect()
        }
    })
}

/// `None` for an event with no identifier (not saved yet).
///
/// # Safety
/// `e` belongs to the worker thread's store.
unsafe fn raw_event(e: &EKEvent) -> Option<RawEvent> {
    // SAFETY: property reads on a live event, on its store's thread.
    unsafe {
        let identifier = e.eventIdentifier()?.to_string();
        let series_id = e
            .calendarItemExternalIdentifier()
            .map_or_else(|| identifier.clone(), |s| s.to_string());
        let calendar = e.calendar();
        let attendees: Vec<RawPerson> = e
            .attendees()
            .map(|list| list.iter().map(|p| raw_person(&p)).collect())
            .unwrap_or_default();
        let my_response = attendees
            .iter()
            .find(|p| p.is_current_user)
            .map(|p| p.response);
        Some(RawEvent {
            identifier,
            series_id,
            title: e.title().to_string(),
            start: e.startDate().timeIntervalSince1970().floor() as i64,
            end: e.endDate().timeIntervalSince1970().floor() as i64,
            all_day: e.isAllDay(),
            calendar_id: calendar
                .as_ref()
                .map(|c| c.calendarIdentifier().to_string())
                .unwrap_or_default(),
            calendar_title: calendar
                .as_ref()
                .map(|c| c.title().to_string())
                .unwrap_or_default(),
            notes: e.notes().map(|n| n.to_string()),
            organizer: e.organizer().map(|p| raw_person(&p)),
            attendees,
            my_response,
        })
    }
}

/// Reads the display name only, never `URL` (the address).
///
/// # Safety
/// `p` belongs to the worker thread's store.
unsafe fn raw_person(p: &EKParticipant) -> RawPerson {
    // SAFETY: property reads on a live participant.
    unsafe {
        RawPerson {
            name: p.name().map(|n| n.to_string()),
            kind: kind_of(p.participantType()),
            response: response_of(p.participantStatus()),
            is_current_user: p.isCurrentUser(),
        }
    }
}

fn kind_of(kind: EKParticipantType) -> PersonKind {
    match kind {
        EKParticipantType::Person => PersonKind::Person,
        EKParticipantType::Room => PersonKind::Room,
        EKParticipantType::Resource => PersonKind::Resource,
        EKParticipantType::Group => PersonKind::Group,
        _ => PersonKind::Unknown,
    }
}

fn response_of(status: EKParticipantStatus) -> Response {
    match status {
        EKParticipantStatus::Accepted => Response::Accepted,
        EKParticipantStatus::Declined => Response::Declined,
        EKParticipantStatus::Tentative => Response::Tentative,
        // Pending, unknown, delegated, completed and in process.
        _ => Response::NeedsAction,
    }
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

fn offset_min(at: i64) -> i32 {
    let date = NSDate::dateWithTimeIntervalSince1970(at as f64);
    let secs = NSTimeZone::localTimeZone().secondsFromGMTForDate(&date);
    i32::try_from(secs / 60).unwrap_or(0)
}

fn watch(notify: Notify) -> bool {
    // EventKit posts the notification for live stores: make sure the worker's exists.
    if run(|_| ()).is_err() {
        return false;
    }
    let block = RcBlock::new(move |_: NonNull<NSNotification>| notify());
    let center = NSNotificationCenter::defaultCenter();
    // SAFETY: the name is EventKit's constant; no object or queue, so the block runs on the
    // posting thread; the center copies the block.
    let token = unsafe {
        center.addObserverForName_object_queue_usingBlock(
            Some(EKEventStoreChangedNotification),
            None,
            None,
            &block,
        )
    };
    // Observed for the app's lifetime: the token is never removed.
    std::mem::forget(token);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn access_maps_every_status() {
        assert_eq!(
            access_of(EKAuthorizationStatus::NotDetermined),
            Access::NotAsked
        );
        assert_eq!(
            access_of(EKAuthorizationStatus::FullAccess),
            Access::Granted
        );
        for denied in [
            EKAuthorizationStatus::Denied,
            EKAuthorizationStatus::Restricted,
            EKAuthorizationStatus::WriteOnly,
        ] {
            assert_eq!(access_of(denied), Access::Denied, "{denied:?}");
        }
    }

    #[test]
    fn responses_map_to_the_four_values() {
        let table = [
            (EKParticipantStatus::Accepted, Response::Accepted),
            (EKParticipantStatus::Declined, Response::Declined),
            (EKParticipantStatus::Tentative, Response::Tentative),
            (EKParticipantStatus::Pending, Response::NeedsAction),
            (EKParticipantStatus::Unknown, Response::NeedsAction),
            (EKParticipantStatus::Delegated, Response::NeedsAction),
            (EKParticipantStatus::Completed, Response::NeedsAction),
            (EKParticipantStatus::InProcess, Response::NeedsAction),
        ];
        for (status, response) in table {
            assert_eq!(response_of(status), response, "{status:?}");
        }
    }

    #[test]
    fn kinds_map_rooms_and_resources_apart() {
        assert_eq!(kind_of(EKParticipantType::Person), PersonKind::Person);
        assert_eq!(kind_of(EKParticipantType::Room), PersonKind::Room);
        assert_eq!(kind_of(EKParticipantType::Resource), PersonKind::Resource);
        assert_eq!(kind_of(EKParticipantType::Group), PersonKind::Group);
        assert_eq!(kind_of(EKParticipantType::Unknown), PersonKind::Unknown);
    }

    #[test]
    fn the_time_helpers_answer_without_permission() {
        assert!(now() > 1_794_528_000);
        let offset = offset_min(now());
        assert!((-12 * 60..=14 * 60).contains(&offset), "{offset}");
    }
}
