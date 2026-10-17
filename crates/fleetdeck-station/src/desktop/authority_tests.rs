//! The dispatch authority (R13 §5E item 1, §5F items 1 and 2, §5G item 1):
//! the receipt checks, the single use of an `action_id` for the connection
//! epoch, and the start check. The clocks and the connection are fakes.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use fleetdeck_core::ChildEnv;
use fleetdeck_link::desktop_authority::issue;
use fleetdeck_proto::desktop::DesktopAction as A;
use fleetdeck_proto::desktop_authority::{AUTHORITY_REFUSED, DesktopAuthority, MAX_USED_IDS};

use super::Fail;
use super::authority::{
    Admitted, Conn, EXPIRED, QUIET_ACTIVE, Registry, SKEW_REFUSED, START_LIMIT, Start, TOO_MANY,
    Time, admit_with,
};

pub(super) const NODE: &str = "aa11";
pub(super) const EPOCH: &str = "cd01cd01cd01cd01cd01cd01cd01cd01";
/// 2024-01-01T12:00:00Z in Unix ms.
pub(super) const NOON: i64 = 1_704_110_400_000;

/// A connection and two clocks that a test moves.
pub(super) struct Harness {
    pub wall: Arc<AtomicI64>,
    mono_ms: Arc<AtomicU64>,
    base: Instant,
    pub current: Arc<AtomicBool>,
    pub alive: Arc<AtomicBool>,
    pub closed: Arc<Mutex<Vec<String>>>,
    pub registry: Mutex<Registry>,
    pub epoch: Option<String>,
}

impl Harness {
    pub fn new() -> Self {
        Self {
            wall: Arc::new(AtomicI64::new(NOON)),
            mono_ms: Arc::new(AtomicU64::new(0)),
            base: Instant::now(),
            current: Arc::new(AtomicBool::new(true)),
            alive: Arc::new(AtomicBool::new(true)),
            closed: Arc::new(Mutex::new(Vec::new())),
            registry: Mutex::new(Registry::new(MAX_USED_IDS)),
            epoch: Some(EPOCH.to_owned()),
        }
    }

    pub fn conn(&self) -> Conn {
        let current = Arc::clone(&self.current);
        let alive = Arc::clone(&self.alive);
        let closed = Arc::clone(&self.closed);
        Conn::new(
            self.epoch.clone(),
            Arc::new(move || current.load(Ordering::SeqCst)),
            Arc::new(move || alive.load(Ordering::SeqCst)),
            Arc::new(move |detail| closed.lock().unwrap().push(detail.to_owned())),
        )
    }

    pub fn time(&self) -> Time {
        let wall = Arc::clone(&self.wall);
        let mono = Arc::clone(&self.mono_ms);
        let base = self.base;
        Time::new(
            Arc::new(move || wall.load(Ordering::SeqCst)),
            Arc::new(move || base + Duration::from_millis(mono.load(Ordering::SeqCst))),
        )
    }

    pub fn now(&self) -> i64 {
        self.wall.load(Ordering::SeqCst)
    }

    /// Moves the monotonic clock (and nothing else) forward.
    pub fn advance_mono(&self, ms: u64) {
        self.mono_ms.fetch_add(ms, Ordering::SeqCst);
    }

    /// An authority that the deck would issue now for `action`.
    pub fn authority(&self, action: &A, n: u64) -> DesktopAuthority {
        issue(action, NODE, id(n), EPOCH.to_owned(), self.now(), None)
    }

    pub fn admit(&self, action: &A, authority: Option<DesktopAuthority>) -> Result<Admitted, Fail> {
        admit_with(
            &self.conn(),
            &self.time(),
            &self.registry,
            NODE,
            authority,
            action,
        )
    }

    /// An admitted action with the id number `n`.
    pub fn admitted(&self, action: &A, n: u64) -> Admitted {
        self.admit(action, Some(self.authority(action, n))).unwrap()
    }
}

/// An `action_id` of 26 characters.
pub(super) fn id(n: u64) -> String {
    format!("{n:026}")
}

fn refused(e: Fail) -> bool {
    e.code == "forbidden" && e.message == AUTHORITY_REFUSED
}

fn lock() -> A {
    A::ScreenLock
}

#[test]
fn a_valid_authority_is_admitted() {
    let h = Harness::new();
    assert!(h.admit(&lock(), Some(h.authority(&lock(), 1))).is_ok());
}

#[test]
fn a_missing_or_invalid_authority_is_refused() {
    let h = Harness::new();
    let ok = h.authority(&lock(), 1);
    assert!(refused(h.admit(&lock(), None).unwrap_err()));
    // The id shape: 26 ASCII alphanumeric characters.
    let bad_ids = [
        String::new(),
        "short".to_owned(),
        "a".repeat(25),
        "a".repeat(27),
        format!("{}-", "a".repeat(25)),
    ];
    for bad in &bad_ids {
        let a = DesktopAuthority {
            action_id: bad.clone(),
            ..ok.clone()
        };
        assert!(refused(h.admit(&lock(), Some(a)).unwrap_err()), "{bad:?}");
    }
    // The hash binds the action, the station and the monitor.
    let other = h.authority(&A::ScreenDnd { on: true }, 2);
    assert!(refused(h.admit(&lock(), Some(other)).unwrap_err()));
    let other_node = issue(&lock(), "bb22", id(3), EPOCH.to_owned(), h.now(), None);
    assert!(refused(h.admit(&lock(), Some(other_node)).unwrap_err()));
    let power = |monitor: &str| A::ScreenPower {
        monitor: Some(monitor.to_owned()),
        on: true,
    };
    let for_dp1 = h.authority(&power("DP-1"), 4);
    assert!(refused(h.admit(&power("DP-2"), Some(for_dp1)).unwrap_err()));
    // Nothing above consumed the id of `ok`.
    assert!(h.admit(&lock(), Some(ok)).is_ok());
}

#[test]
fn the_epoch_must_be_the_epoch_of_this_connection() {
    let h = Harness::new();
    let wrong = issue(&lock(), NODE, id(1), "ee".repeat(16), h.now(), None);
    assert!(refused(h.admit(&lock(), Some(wrong)).unwrap_err()));
    let mut none = Harness::new();
    none.epoch = None;
    let a = none.authority(&lock(), 1);
    assert!(refused(none.admit(&lock(), Some(a)).unwrap_err()));
}

#[test]
fn the_clock_skew_limit_is_five_seconds_in_both_directions() {
    let h = Harness::new();
    let at = |deck_now: i64, n: u64| {
        let a = issue(&lock(), NODE, id(n), EPOCH.to_owned(), deck_now, None);
        h.admit(&lock(), Some(a))
    };
    assert!(at(h.now() + 5_000, 1).is_ok());
    assert!(at(h.now() - 5_000, 2).is_ok());
    for (deck_now, n) in [(h.now() + 5_001, 3), (h.now() - 5_001, 4)] {
        let e = at(deck_now, n).unwrap_err();
        assert_eq!((e.code, e.message.as_str()), ("forbidden", SKEW_REFUSED));
    }
}

#[test]
fn an_action_id_is_used_once_for_the_epoch() {
    let h = Harness::new();
    h.admitted(&lock(), 1);
    assert!(refused(
        h.admit(&lock(), Some(h.authority(&lock(), 1))).unwrap_err()
    ));
    // Another id works.
    h.admitted(&lock(), 2);
    // The same id on another epoch is a new id (a new connection).
    let mut next = Harness::new();
    next.epoch = Some("ab".repeat(16));
    let a = issue(&lock(), NODE, id(1), "ab".repeat(16), next.now(), None);
    assert!(
        admit_with(
            &next.conn(),
            &next.time(),
            &h.registry,
            NODE,
            Some(a),
            &lock()
        )
        .is_ok()
    );
}

#[test]
fn a_full_epoch_refuses_new_ids_and_closes_the_link() {
    let h = Harness::new();
    let max = u64::try_from(MAX_USED_IDS).unwrap();
    for n in 0..max {
        h.admitted(&lock(), n);
    }
    assert!(h.closed.lock().unwrap().is_empty());
    // A repeated id is a plain refusal.
    assert!(refused(
        h.admit(&lock(), Some(h.authority(&lock(), 0))).unwrap_err()
    ));
    assert!(h.closed.lock().unwrap().is_empty());
    let e = h
        .admit(&lock(), Some(h.authority(&lock(), max)))
        .unwrap_err();
    assert_eq!((e.code, e.message.as_str()), ("failed", TOO_MANY));
    assert_eq!(h.closed.lock().unwrap().len(), 1);
    // The next connection has a new epoch and an empty set.
    let mut next = Harness::new();
    next.epoch = Some("ab".repeat(16));
    let a = issue(&lock(), NODE, id(1), "ab".repeat(16), next.now(), None);
    assert!(
        admit_with(
            &next.conn(),
            &next.time(),
            &h.registry,
            NODE,
            Some(a),
            &lock()
        )
        .is_ok()
    );
}

#[test]
fn the_epochs_of_ended_connections_are_dropped() {
    let h = Harness::new();
    h.admitted(&lock(), 1);
    assert_eq!(h.registry.lock().unwrap().epoch_count(), 1);
    h.alive.store(false, Ordering::SeqCst);
    let mut next = Harness::new();
    next.epoch = Some("ab".repeat(16));
    let a = issue(&lock(), NODE, id(1), "ab".repeat(16), next.now(), None);
    admit_with(
        &next.conn(),
        &next.time(),
        &h.registry,
        NODE,
        Some(a),
        &lock(),
    )
    .unwrap();
    assert_eq!(h.registry.lock().unwrap().epoch_count(), 1);
}

#[test]
fn a_connection_that_is_not_current_is_refused_before_the_id_is_consumed() {
    let h = Harness::new();
    h.current.store(false, Ordering::SeqCst);
    let a = h.authority(&lock(), 1);
    assert!(refused(h.admit(&lock(), Some(a.clone())).unwrap_err()));
    h.current.store(true, Ordering::SeqCst);
    assert!(h.admit(&lock(), Some(a)).is_ok(), "the id was not consumed");
}

#[test]
fn a_bad_quiet_hours_string_is_invalid_params_and_consumes_nothing() {
    let h = Harness::new();
    let mut a = h.authority(&lock(), 1);
    a.quiet_hours = Some("25:00-07:00".to_owned());
    let e = h.admit(&lock(), Some(a.clone())).unwrap_err();
    assert_eq!(e.code, "invalid_params");
    assert!(!e.message.contains("25:00"));
    a.quiet_hours = Some("22:00-07:00".to_owned());
    assert!(h.admit(&lock(), Some(a)).is_ok());
}

fn env() -> ChildEnv {
    ChildEnv::empty()
}

#[tokio::test]
async fn the_start_check_passes_and_then_latches() {
    let h = Harness::new();
    let admitted = h.admitted(&lock(), 1);
    let start = Start::new(admitted, &lock());
    start.begin(&env()).await.unwrap();
    // Later programs of the same action do not check again.
    h.wall.store(NOON + 1_000_000, Ordering::SeqCst);
    h.advance_mono(60_000);
    h.current.store(false, Ordering::SeqCst);
    start.begin(&env()).await.unwrap();
}

#[tokio::test]
async fn the_deadline_is_exact_on_the_wall_clock() {
    let h = Harness::new();
    let at_deadline = Start::new(h.admitted(&lock(), 1), &lock());
    h.wall.store(NOON + 10_000, Ordering::SeqCst);
    at_deadline.begin(&env()).await.unwrap();
    let late = Start::new(h.admitted(&lock(), 2), &lock());
    h.wall.store(NOON + 10_000 + 10_001, Ordering::SeqCst);
    let e = late.begin(&env()).await.unwrap_err();
    assert_eq!((e.code, e.message.as_str()), ("failed", EXPIRED));
    // One millisecond after the deadline is late.
    h.wall.store(NOON, Ordering::SeqCst);
    let again = Start::new(h.admitted(&lock(), 3), &lock());
    h.wall.store(NOON + 10_001, Ordering::SeqCst);
    assert_eq!(again.begin(&env()).await.unwrap_err().message, EXPIRED);
}

#[tokio::test]
async fn the_monotonic_limit_does_not_depend_on_the_wall_clock() {
    let h = Harness::new();
    let at_limit = Start::new(h.admitted(&lock(), 1), &lock());
    let limit = u64::try_from(START_LIMIT.as_millis()).unwrap();
    h.advance_mono(limit);
    at_limit.begin(&env()).await.unwrap();
    let slow = Start::new(h.admitted(&lock(), 2), &lock());
    h.advance_mono(limit + 1);
    // The wall clock is set back, so that the deadline alone would pass.
    h.wall.store(NOON - 3_600_000, Ordering::SeqCst);
    let e = slow.begin(&env()).await.unwrap_err();
    assert_eq!((e.code, e.message.as_str()), ("failed", EXPIRED));
}

#[tokio::test]
async fn the_start_check_refuses_when_the_connection_is_no_longer_current() {
    let h = Harness::new();
    let start = Start::new(h.admitted(&lock(), 1), &lock());
    h.current.store(false, Ordering::SeqCst);
    assert!(refused(start.begin(&env()).await.unwrap_err()));
}

#[tokio::test]
async fn the_start_check_refuses_an_action_that_is_not_the_authorized_one() {
    let h = Harness::new();
    let admitted = h.admitted(&lock(), 1);
    let start = Start::new(admitted, &A::ScreenDnd { on: true });
    assert!(refused(start.begin(&env()).await.unwrap_err()));
}

#[tokio::test]
async fn quiet_hours_fail_closed_when_the_offset_cannot_be_read() {
    let h = Harness::new();
    let mut a = h.authority(&lock(), 1);
    a.quiet_hours = Some("22:00-07:00".to_owned());
    let admitted = h.admit(&lock(), Some(a)).unwrap();
    // No `date` on the empty environment.
    let e = Start::new(admitted, &lock())
        .begin(&env())
        .await
        .unwrap_err();
    assert_eq!((e.code, e.message.as_str()), ("failed", QUIET_ACTIVE));
}
