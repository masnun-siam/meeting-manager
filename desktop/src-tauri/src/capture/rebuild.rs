//! Following device changes: a pure timing state machine and the worker thread that drives it.
//! The worker owns every call that touches the graph, so a change notification only sends a
//! message and Core Audio's own threads never block on a rebuild.

use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use super::{Event, Events};

/// How the worker paces rebuilds. The PRD fixes the outcome (a gap of at most 1 s, failure after
/// 5 s, revocation noticed within 3 s); these numbers are chosen to meet it.
#[derive(Clone, Copy, Debug)]
pub struct Policy {
    /// Quiet time after the last change before rebuilding; macOS sends a burst on connect.
    pub settle: Duration,
    /// Wait between failed attempts.
    pub retry: Duration,
    /// Time after the first change at which the worker stops trying.
    pub give_up: Duration,
    /// How often the health check (microphone permission, buffer layout) runs.
    pub health: Duration,
}

impl Policy {
    pub const PRD: Policy = Policy {
        settle: Duration::from_millis(200),
        retry: Duration::from_millis(250),
        give_up: Duration::from_secs(5),
        health: Duration::from_secs(2),
    };
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Next {
    /// Nothing to rebuild.
    Idle,
    Wait(Instant),
    Attempt,
    GiveUp,
}

/// When to rebuild, given the changes and attempts so far. Pure: every call takes the time.
pub struct Rebuild {
    policy: Policy,
    first_change: Option<Instant>,
    due: Option<Instant>,
}

impl Rebuild {
    pub fn new(policy: Policy) -> Rebuild {
        Rebuild {
            policy,
            first_change: None,
            due: None,
        }
    }

    /// A device changed. Each change pushes the attempt back, so a burst gives one rebuild; the
    /// 5 s window still counts from the first change.
    pub fn changed(&mut self, now: Instant) {
        self.first_change.get_or_insert(now);
        self.due = Some(now + self.policy.settle);
    }

    pub fn next(&self, now: Instant) -> Next {
        let (Some(first), Some(due)) = (self.first_change, self.due) else {
            return Next::Idle;
        };
        let deadline = first + self.policy.give_up;
        if now >= deadline {
            Next::GiveUp
        } else if now >= due {
            Next::Attempt
        } else {
            Next::Wait(due.min(deadline))
        }
    }

    pub fn attempt_failed(&mut self, now: Instant) {
        self.due = Some(now + self.policy.retry);
    }

    /// A rebuild succeeded: the window closes. A change that arrived during the attempt is
    /// delivered after this and starts a new one.
    pub fn attempt_ok(&mut self) {
        self.first_change = None;
        self.due = None;
    }
}

pub enum Msg {
    Changed,
    Stop,
}

/// The rebuild worker. Its dependencies are injected so the tests drive it without Core Audio.
pub struct Worker {
    pub policy: Policy,
    /// Tears down the current graph, if any, and builds a new one on the current devices.
    pub build: Box<dyn FnMut() -> anyhow::Result<()> + Send>,
    /// Tears down the current graph, if any. Calling it twice is safe.
    pub halt: Box<dyn FnMut() + Send>,
    /// `Some(reason)` when capture can no longer go on (microphone permission revoked, buffers
    /// in an unexpected layout).
    pub health: Box<dyn FnMut() -> Option<String> + Send>,
    pub events: Events,
    pub now: fn() -> Instant,
}

impl Worker {
    /// Runs until `Msg::Stop` arrives or every sender is gone; the graph is halted either way.
    pub fn run(mut self, rx: Receiver<Msg>) {
        let mut state = Rebuild::new(self.policy);
        let mut next_health = (self.now)() + self.policy.health;
        let mut last_error = String::new();
        loop {
            let now = (self.now)();
            if now >= next_health {
                if let Some(reason) = (self.health)() {
                    return self.fail(reason, &rx);
                }
                next_health = now + self.policy.health;
            }
            let wake = match state.next(now) {
                Next::Attempt => {
                    match (self.build)() {
                        Ok(()) => {
                            state.attempt_ok();
                            (self.events)(Event::DeviceChanged);
                        }
                        Err(e) => {
                            last_error = format!("{e:#}");
                            state.attempt_failed((self.now)());
                        }
                    }
                    continue;
                }
                Next::GiveUp => {
                    return self.fail(
                        format!("Audio capture could not follow the device change: {last_error}"),
                        &rx,
                    );
                }
                Next::Wait(at) => at.min(next_health),
                Next::Idle => next_health,
            };
            match rx.recv_timeout(wake.saturating_duration_since((self.now)())) {
                Ok(Msg::Changed) => state.changed((self.now)()),
                Ok(Msg::Stop) | Err(RecvTimeoutError::Disconnected) => break,
                Err(RecvTimeoutError::Timeout) => {}
            }
        }
        (self.halt)();
    }

    /// Stops delivering samples, reports the failure once and waits for `Stop`, ignoring changes.
    fn fail(mut self, reason: String, rx: &Receiver<Msg>) {
        (self.halt)();
        (self.events)(Event::Failed(reason));
        while let Ok(Msg::Changed) = rx.recv() {}
        (self.halt)();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc::{self, Sender};
    use std::sync::{Arc, Mutex};
    use std::thread::{self, JoinHandle};

    use super::*;

    const MS: Duration = Duration::from_millis(1);

    fn policy() -> Policy {
        Policy::PRD
    }

    #[test]
    fn nothing_happens_without_a_change() {
        let t0 = Instant::now();
        assert_eq!(Rebuild::new(policy()).next(t0), Next::Idle);
    }

    #[test]
    fn one_change_gives_one_attempt_after_settling() {
        let t0 = Instant::now();
        let mut r = Rebuild::new(policy());
        r.changed(t0);
        assert_eq!(r.next(t0), Next::Wait(t0 + 200 * MS));
        assert_eq!(r.next(t0 + 199 * MS), Next::Wait(t0 + 200 * MS));
        assert_eq!(r.next(t0 + 200 * MS), Next::Attempt);
        r.attempt_ok();
        assert_eq!(r.next(t0 + 201 * MS), Next::Idle);
    }

    #[test]
    fn a_burst_of_changes_gives_one_attempt() {
        let t0 = Instant::now();
        let mut r = Rebuild::new(policy());
        for i in 0..5 {
            r.changed(t0 + i * 30 * MS);
            assert!(matches!(r.next(t0 + i * 30 * MS), Next::Wait(_)));
        }
        assert_eq!(r.next(t0 + 320 * MS), Next::Attempt);
    }

    #[test]
    fn failures_retry_and_give_up_exactly_five_seconds_after_the_first_change() {
        let t0 = Instant::now();
        let mut r = Rebuild::new(policy());
        r.changed(t0);
        let mut now = t0 + 200 * MS;
        let mut attempts = 0;
        while r.next(now) == Next::Attempt {
            attempts += 1;
            r.attempt_failed(now);
            assert_eq!(
                r.next(now),
                Next::Wait((now + 250 * MS).min(t0 + 5000 * MS))
            );
            now += 250 * MS;
        }
        // Attempts at 200, 450, ..., 4950 ms.
        assert_eq!(attempts, 20);
        assert_eq!(r.next(t0 + 4999 * MS), Next::Wait(t0 + 5000 * MS));
        assert_eq!(r.next(t0 + 5000 * MS), Next::GiveUp);
    }

    #[test]
    fn success_resets_the_five_second_window() {
        let t0 = Instant::now();
        let mut r = Rebuild::new(policy());
        r.changed(t0);
        r.attempt_ok();
        r.changed(t0 + 4900 * MS);
        assert_eq!(r.next(t0 + 5100 * MS), Next::Attempt);
        assert_eq!(r.next(t0 + 9900 * MS), Next::GiveUp);
    }

    struct Fake {
        tx: Sender<Msg>,
        handle: JoinHandle<()>,
        builds: Arc<AtomicUsize>,
        halts: Arc<AtomicUsize>,
        events: Arc<Mutex<Vec<Event>>>,
    }

    /// A worker on a millisecond policy whose `build` sleeps `build_time`, fails the first
    /// `failures` times and calls `during` on its first call.
    fn spawn(
        failures: usize,
        build_time: Duration,
        health: Box<dyn FnMut() -> Option<String> + Send>,
        during: Box<dyn FnMut() + Send>,
    ) -> Fake {
        let builds = Arc::new(AtomicUsize::new(0));
        let halts = Arc::new(AtomicUsize::new(0));
        let events = Arc::new(Mutex::new(Vec::new()));
        let (tx, rx) = mpsc::channel();
        let mut during = Some(during);
        let worker = Worker {
            policy: Policy {
                settle: 10 * MS,
                retry: 5 * MS,
                give_up: 100 * MS,
                health: 20 * MS,
            },
            build: Box::new({
                let builds = builds.clone();
                move || {
                    let n = builds.fetch_add(1, Ordering::SeqCst);
                    if let Some(mut f) = during.take() {
                        f();
                    }
                    thread::sleep(build_time);
                    if n < failures {
                        anyhow::bail!("Core Audio create aggregate device failed (OSStatus -50)")
                    }
                    Ok(())
                }
            }),
            halt: Box::new({
                let halts = halts.clone();
                move || {
                    halts.fetch_add(1, Ordering::SeqCst);
                }
            }),
            health,
            events: Box::new({
                let events = events.clone();
                move |e| events.lock().unwrap().push(e)
            }),
            now: Instant::now,
        };
        let handle = thread::spawn(move || worker.run(rx));
        Fake {
            tx,
            handle,
            builds,
            halts,
            events,
        }
    }

    fn healthy() -> Box<dyn FnMut() -> Option<String> + Send> {
        Box::new(|| None)
    }

    fn wait_for(events: &Arc<Mutex<Vec<Event>>>, count: usize) {
        let start = Instant::now();
        while events.lock().unwrap().len() < count {
            assert!(start.elapsed() < Duration::from_secs(5), "timed out");
            thread::sleep(MS);
        }
    }

    #[test]
    fn the_worker_rebuilds_once_per_burst_and_reports_it() {
        let fake = spawn(0, Duration::ZERO, healthy(), Box::new(|| {}));
        for _ in 0..5 {
            fake.tx.send(Msg::Changed).unwrap();
        }
        wait_for(&fake.events, 1);
        fake.tx.send(Msg::Stop).unwrap();
        fake.handle.join().unwrap();
        assert_eq!(fake.builds.load(Ordering::SeqCst), 1);
        assert_eq!(*fake.events.lock().unwrap(), vec![Event::DeviceChanged]);
        assert_eq!(fake.halts.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_change_during_a_rebuild_gives_one_more_rebuild() {
        let (tx, rx) = mpsc::channel::<Sender<Msg>>();
        let fake = spawn(
            0,
            20 * MS,
            healthy(),
            Box::new(move || {
                rx.recv().unwrap().send(Msg::Changed).unwrap();
            }),
        );
        tx.send(fake.tx.clone()).unwrap();
        fake.tx.send(Msg::Changed).unwrap();
        wait_for(&fake.events, 2);
        fake.tx.send(Msg::Stop).unwrap();
        fake.handle.join().unwrap();
        assert_eq!(fake.builds.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn failures_are_retried_until_one_succeeds() {
        let fake = spawn(3, Duration::ZERO, healthy(), Box::new(|| {}));
        fake.tx.send(Msg::Changed).unwrap();
        wait_for(&fake.events, 1);
        fake.tx.send(Msg::Stop).unwrap();
        fake.handle.join().unwrap();
        assert_eq!(fake.builds.load(Ordering::SeqCst), 4);
        assert_eq!(*fake.events.lock().unwrap(), vec![Event::DeviceChanged]);
    }

    #[test]
    fn giving_up_reports_once_halts_and_ignores_later_changes() {
        let fake = spawn(usize::MAX, Duration::ZERO, healthy(), Box::new(|| {}));
        fake.tx.send(Msg::Changed).unwrap();
        wait_for(&fake.events, 1);
        let builds = fake.builds.load(Ordering::SeqCst);
        fake.tx.send(Msg::Changed).unwrap();
        thread::sleep(50 * MS);
        fake.tx.send(Msg::Stop).unwrap();
        fake.handle.join().unwrap();
        assert_eq!(fake.builds.load(Ordering::SeqCst), builds);
        assert_eq!(
            *fake.events.lock().unwrap(),
            vec![Event::Failed(
                "Audio capture could not follow the device change: \
                 Core Audio create aggregate device failed (OSStatus -50)"
                    .into()
            )]
        );
        assert!(fake.halts.load(Ordering::SeqCst) >= 1);
    }

    #[test]
    fn a_failed_health_check_is_reported_once() {
        let fake = spawn(
            0,
            Duration::ZERO,
            Box::new(|| Some("Microphone permission was revoked".into())),
            Box::new(|| {}),
        );
        wait_for(&fake.events, 1);
        thread::sleep(60 * MS);
        fake.tx.send(Msg::Stop).unwrap();
        fake.handle.join().unwrap();
        assert_eq!(
            *fake.events.lock().unwrap(),
            vec![Event::Failed("Microphone permission was revoked".into())]
        );
    }

    #[test]
    fn stop_ends_the_worker_promptly_without_an_event() {
        let fake = spawn(0, Duration::ZERO, healthy(), Box::new(|| {}));
        let start = Instant::now();
        fake.tx.send(Msg::Stop).unwrap();
        fake.handle.join().unwrap();
        assert!(start.elapsed() < Duration::from_millis(500));
        assert!(fake.events.lock().unwrap().is_empty());
        assert_eq!(fake.builds.load(Ordering::SeqCst), 0);
        assert_eq!(fake.halts.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn dropping_every_sender_stops_the_worker() {
        let fake = spawn(0, Duration::ZERO, healthy(), Box::new(|| {}));
        drop(fake.tx);
        fake.handle.join().unwrap();
        assert_eq!(fake.halts.load(Ordering::SeqCst), 1);
    }
}
