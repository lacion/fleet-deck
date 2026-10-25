//! The unit tests of the wall control hub (B17 §3.1.3).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::{Duration, Instant};

use fleetdeck_proto::display_directive::SessionRef;
use fleetdeck_proto::wall_control::{
    ScrollStep, WallArea, WallControl, WallEvent, WallLink, WallRemoteState,
};

use super::{PushError, PushEvent, WallControlHub};

fn hub() -> WallControlHub {
    WallControlHub::new().unwrap()
}

fn page_down() -> PushEvent {
    PushEvent::Control(WallControl::Scroll {
        area: WallArea::Page,
        step: ScrollStep::PageDown,
    })
}

fn select(id: &str) -> PushEvent {
    PushEvent::Control(WallControl::Select {
        item: Some(SessionRef {
            station_id: "st".into(),
            session_id: id.into(),
        }),
    })
}

fn secs(n: u64) -> Duration {
    Duration::from_secs(n)
}

#[test]
fn hub_orders_and_keeps_at_most_32() {
    let hub = hub();
    let t0 = Instant::now();
    for i in 0..40u64 {
        // 200 ms apart: the rate window never fills.
        let at = t0 + Duration::from_millis(i * 200);
        let frame = hub.push("d1", 1, page_down(), at).unwrap();
        assert_eq!(frame.seq, i + 1);
    }
    let parts = hub.view("d1", 1, 0, t0 + secs(9));
    assert_eq!(parts.seq, 40);
    let seqs: Vec<u64> = parts.controls.iter().map(|f| f.seq).collect();
    assert_eq!(seqs, (9..=40).collect::<Vec<u64>>());
    assert!(!parts.complete, "frames 1 to 8 are gone");
    assert_eq!(parts.incarnation, hub.incarnation());
}

#[test]
fn hub_drops_controls_older_than_30_s_and_keeps_the_checkpoint() {
    let hub = hub();
    let t0 = Instant::now();
    hub.push("d1", 4, select("s1"), t0).unwrap();
    hub.push("d1", 4, page_down(), t0 + Duration::from_millis(10))
        .unwrap();
    let soon = hub.view("d1", 4, 0, t0 + secs(29));
    assert_eq!(soon.controls.len(), 2);
    assert!(soon.complete);
    let late = hub.view("d1", 4, 0, t0 + secs(31));
    assert!(late.controls.is_empty());
    assert!(!late.complete);
    assert_eq!(late.seq, 2);
    assert_eq!(late.state.frame_tenths, 9, "one page_down of the area page");
    assert!(late.state.selected.is_some());
    // A new control moves the checkpoint on from its old value.
    hub.push("d1", 4, page_down(), t0 + secs(40)).unwrap();
    let again = hub.view("d1", 4, 0, t0 + secs(100));
    assert!(again.controls.is_empty());
    assert_eq!(again.state.frame_tenths, 18);
    assert!(again.state.selected.is_some());
    // The hold counts from the last control: 120 s from t0 + 40 s.
    assert_eq!(again.hold_left_ms, 60_000);
    assert_eq!(hub.view("d1", 4, 3, t0 + secs(300)).hold_left_ms, 0);
}

#[test]
fn hub_resets_the_checkpoint_on_a_new_generation() {
    let hub = hub();
    let t0 = Instant::now();
    hub.push("d1", 1, select("s1"), t0).unwrap();
    let moved = hub.push("d1", 1, page_down(), t0).unwrap();
    assert_eq!(moved.state.frame_tenths, 9);
    // A navigation to a new generation starts from the default state.
    let nav = hub.push("d1", 2, PushEvent::Navigate, t0).unwrap();
    assert_eq!(nav.event, WallEvent::Navigate);
    assert_eq!(nav.state, WallRemoteState::default());
    assert_eq!(nav.seq, 3, "seq continues across generations");
    // The view reads the checkpoint of the generation that is asked for.
    assert_eq!(hub.view("d1", 2, 0, t0).state, WallRemoteState::default());
    assert_eq!(hub.view("d1", 1, 0, t0).state, WallRemoteState::default());
    let again = hub.push("d1", 2, page_down(), t0).unwrap();
    assert_eq!(again.state.frame_tenths, 9);
    assert!(again.state.selected.is_none());
    // A directive that changed with no remote frame: the old state is not
    // shown for the new generation.
    assert_eq!(hub.view("d1", 3, 0, t0).state, WallRemoteState::default());
}

#[test]
fn hub_refuses_an_older_generation_and_keeps_the_newer_checkpoint() {
    // The review's interleaving: a control of generation 4 is delayed; the
    // navigation to 5 and a selection in 5 arrive first.
    let hub = hub();
    let t0 = Instant::now();
    hub.push("d1", 4, page_down(), t0).unwrap();
    hub.push("d1", 5, PushEvent::Navigate, t0).unwrap();
    let chosen = hub.push("d1", 5, select("s5"), t0).unwrap();
    let moved = hub.push("d1", 5, page_down(), t0).unwrap();
    assert_eq!(moved.seq, 4);
    // The delayed request of generation 4 is refused and records nothing.
    assert_eq!(hub.push("d1", 4, select("s4"), t0), Err(PushError::Stale));
    assert_eq!(
        hub.push("d1", 4, PushEvent::Navigate, t0),
        Err(PushError::Stale)
    );
    let parts = hub.view("d1", 5, 0, t0);
    assert_eq!(parts.seq, 4, "no frame for the stale requests");
    assert_eq!(parts.state.selected, chosen.state.selected);
    assert_eq!(parts.state.frame_tenths, 9);
    assert_eq!(hub.sync("d1", 5).state, parts.state);
    // A stale request takes no place in the rate window.
    for _ in 0..6 {
        hub.push("d1", 5, page_down(), t0).unwrap();
    }
}

#[tokio::test]
async fn the_turn_of_a_display_serializes_its_requests_only() {
    use futures_util::FutureExt as _;
    let hub = hub();
    let first = hub.turn("d1").await;
    // A second request of d1 waits while the first holds the turn.
    assert!(hub.turn("d1").now_or_never().is_none());
    // Another display does not wait.
    let other = hub.turn("d2").now_or_never();
    assert!(other.is_some());
    drop(other);
    drop(first);
    // Released: the next request of d1 gets the turn at once.
    let again = hub.turn("d1").now_or_never();
    assert!(again.is_some());
    drop(again);
    assert!(hub.turns.lock().unwrap().is_empty(), "no entry stays");
}

#[test]
fn hub_view_is_complete_only_without_a_gap() {
    let hub = hub();
    let t0 = Instant::now();
    // No frame yet.
    assert!(hub.view("d1", 1, 0, t0).complete);
    assert!(!hub.view("d1", 1, 1, t0).complete, "after above seq");
    // Frames 1 and 2 at t0, frames 3 and 4 at t0 + 20 s.
    for _ in 0..2 {
        hub.push("d1", 1, page_down(), t0).unwrap();
    }
    for _ in 0..2 {
        hub.push("d1", 1, page_down(), t0 + secs(20)).unwrap();
    }
    let now = t0 + secs(25);
    // `after = seq`.
    let parts = hub.view("d1", 1, 4, now);
    assert!(parts.complete && parts.controls.is_empty());
    // `after` inside the kept range.
    let parts = hub.view("d1", 1, 2, now);
    assert!(parts.complete);
    assert_eq!(
        parts.controls.iter().map(|f| f.seq).collect::<Vec<_>>(),
        [3, 4]
    );
    // `after` above `seq`.
    assert!(!hub.view("d1", 1, 5, now).complete);
    // Frames 1 and 2 expire; `after` below the oldest kept frame.
    let later = t0 + secs(35);
    assert!(!hub.view("d1", 1, 0, later).complete);
    assert!(!hub.view("d1", 1, 1, later).complete, "frame 2 is gone");
    // `after = 2` still has frame 3 as the next frame.
    let parts = hub.view("d1", 1, 2, later);
    assert!(parts.complete);
    assert_eq!(parts.controls.len(), 2);
}

#[test]
fn hub_refuses_the_eleventh_push_in_a_second() {
    let hub = hub();
    let t0 = Instant::now();
    for i in 0..10 {
        // Controls and navigations count together.
        let event = if i % 2 == 0 {
            page_down()
        } else {
            PushEvent::Navigate
        };
        hub.push("d1", 1, event, t0).unwrap();
    }
    assert!(!hub.rate_ok("d1", t0));
    assert_eq!(
        hub.push("d1", 1, page_down(), t0).unwrap_err(),
        PushError::TooMany
    );
    // A refused push changes nothing.
    assert_eq!(hub.view("d1", 1, 0, t0).seq, 10);
    // Another display has its own window.
    assert!(hub.rate_ok("d2", t0));
    assert!(hub.push("d2", 1, page_down(), t0).is_ok());
    // The window moves on after one second.
    let next = t0 + Duration::from_millis(1000);
    assert!(hub.rate_ok("d1", next));
    assert_eq!(hub.push("d1", 1, page_down(), next).unwrap().seq, 11);
}

#[test]
fn hub_link_counts_ready_and_older_sockets() {
    let hub = hub();
    assert_eq!(hub.link("d1"), WallLink::Offline);
    let older = hub.attach_older("d1");
    assert_eq!(hub.link("d1"), WallLink::Older);
    let (_rx, sync, ready) = hub.attach_ready("d1", 7);
    assert_eq!(hub.link("d1"), WallLink::Ready);
    assert_eq!(sync.event, WallEvent::Sync);
    assert_eq!(sync.generation, 7);
    assert_eq!(sync.seq, 0);
    let (_rx2, _, ready2) = hub.attach_ready("d1", 7);
    drop(ready);
    assert_eq!(hub.link("d1"), WallLink::Ready, "one ready socket is left");
    drop(ready2);
    assert_eq!(hub.link("d1"), WallLink::Older);
    drop(older);
    assert_eq!(hub.link("d1"), WallLink::Offline);
    // The other displays are not counted.
    assert_eq!(hub.link("d2"), WallLink::Offline);
}

#[test]
fn hub_sync_frame_carries_the_checkpoint_and_the_current_seq() {
    let hub = hub();
    let t0 = Instant::now();
    hub.push("d1", 3, select("s1"), t0).unwrap();
    let (mut rx, sync, _guard) = hub.attach_ready("d1", 3);
    assert_eq!(sync.seq, 1);
    assert!(sync.state.selected.is_some());
    // A later push reaches the receiver in order.
    hub.push("d1", 3, page_down(), t0).unwrap();
    let frame = rx.try_recv().unwrap();
    assert_eq!(frame.seq, 2);
    // `sync` is not kept and sets no hold.
    let view = hub.view("d1", 3, 0, t0);
    assert!(view.controls.iter().all(|f| f.event != WallEvent::Sync));
    // The sync of another generation holds the default state.
    assert_eq!(hub.sync("d1", 4).state, WallRemoteState::default());
}

#[test]
fn hub_audits_the_start_of_a_remote_session_only() {
    let hub = hub();
    let t0 = Instant::now();
    assert!(hub.audit_due("d1", t0));
    for i in 1..5 {
        assert!(!hub.audit_due("d1", t0 + secs(i)));
    }
    // Ten minutes with no control: the next one starts a session.
    assert!(!hub.audit_due("d1", t0 + secs(300)));
    assert!(hub.audit_due("d1", t0 + secs(300 + 600)));
    assert!(hub.audit_due("d2", t0));
}

#[test]
fn forget_ends_the_receivers() {
    let hub = hub();
    let (mut rx, _, _guard) = hub.attach_ready("d1", 1);
    hub.forget("d1");
    assert!(matches!(
        rx.try_recv(),
        Err(tokio::sync::broadcast::error::TryRecvError::Closed)
    ));
    assert_eq!(hub.link("d1"), WallLink::Offline);
}

#[test]
fn two_hubs_have_different_incarnations() {
    let (a, b) = (hub(), hub());
    assert_ne!(a.incarnation(), b.incarnation());
    assert_eq!(a.incarnation().len(), 64);
}
