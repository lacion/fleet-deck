//! The continuation lease client (design §6.3, §6.4, §7.5): the verdict of
//! each answer, the renewals that keep a recording alive, one request in
//! flight, and the answers that discard.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::atomic::Ordering;
use std::time::Duration;

use fleetdeck_proto::capture::code;
use fleetdeck_proto::link::{Outcome, WireError};
use fleetdeck_proto::recording::{CaptureLeaseAnswer, LeaseDecision, LeasePhase, StopReason};

use super::lease::{Lease, Verdict, judge};
use super::rig_tests::{ID, Jumpy, Peer, Reply, T, err, timing};

fn ok(
    sequence: u64,
    decision: LeaseDecision,
    reason: Option<StopReason>,
) -> Result<Outcome, String> {
    Ok(Outcome::Ok(
        serde_json::to_value(CaptureLeaseAnswer {
            sequence,
            decision,
            reason,
        })
        .unwrap(),
    ))
}

#[test]
fn the_verdict_of_an_answer_follows_the_allowed_set() {
    use LeaseDecision::{Continue, Discard, Save};
    let denied = Some(Verdict::Discard(StopReason::LeaseDenied));
    assert_eq!(judge(3, &ok(3, Continue, None)), Some(Verdict::Continue));
    assert_eq!(
        judge(3, &ok(3, Save, Some(StopReason::OwnerStop))),
        Some(Verdict::Save)
    );
    for allowed in [
        StopReason::DeckPolicy,
        StopReason::OwnerLoginEnded,
        StopReason::CoordinatorAuthorityLost,
        StopReason::ArtifactDeleted,
        StopReason::Deadline,
    ] {
        assert_eq!(
            judge(3, &ok(3, Discard, Some(allowed))),
            Some(Verdict::Discard(allowed))
        );
    }
    // A reason outside the set, a save with another reason, a discard
    // without a reason and a continue with a reason are all denied.
    for reason in [
        StopReason::Duration,
        StopReason::PolicyOff,
        StopReason::LeaseExpired,
        StopReason::RecorderExited,
    ] {
        assert_eq!(judge(3, &ok(3, Discard, Some(reason))), denied);
    }
    assert_eq!(judge(3, &ok(3, Save, Some(StopReason::Duration))), denied);
    assert_eq!(judge(3, &ok(3, Save, None)), denied);
    assert_eq!(judge(3, &ok(3, Discard, None)), denied);
    assert_eq!(
        judge(3, &ok(3, Continue, Some(StopReason::DeckPolicy))),
        denied
    );
}

#[test]
fn an_error_a_wire_error_and_a_malformed_answer_are_denied() {
    let denied = Some(Verdict::Discard(StopReason::LeaseDenied));
    assert_eq!(judge(1, &Err("down".to_owned())), denied);
    assert_eq!(
        judge(1, &Ok(Outcome::Err(WireError::new("forbidden", "no")))),
        denied
    );
    assert_eq!(
        judge(1, &Ok(Outcome::Ok(serde_json::json!({"sequence": "x"})))),
        denied
    );
    assert_eq!(judge(1, &Ok(Outcome::Ok(serde_json::Value::Null))), denied);
}

#[test]
fn a_late_answer_for_another_sequence_is_ignored() {
    use LeaseDecision::{Continue, Discard};
    assert_eq!(judge(5, &ok(4, Continue, None)), None);
    assert_eq!(judge(5, &ok(6, Continue, None)), None);
    // Also an answer that would discard: it belongs to another request.
    assert_eq!(
        judge(5, &ok(4, Discard, Some(StopReason::DeckPolicy))),
        None
    );
}

#[tokio::test]
async fn renewals_keep_a_recording_alive_past_the_first_lease() {
    let t = T::new().await;
    t.set_timing(|timing| timing.validity = Duration::from_millis(1000));
    // 2 s is twice the validity of the first lease.
    let done = t.run(ID, "DP-1", 2).await;
    // The fixture has 30 samples: it validates for 2 s too.
    assert!(done.is_ok(), "{:?}", done.err());
    assert!(t.peer.count(LeasePhase::Record) >= 3);
}

#[tokio::test]
async fn answers_that_never_renew_let_the_lease_expire() {
    let t = T::new().await;
    t.set_timing(|timing| timing.validity = Duration::from_millis(1000));
    t.peer.set_phase(LeasePhase::Record, Reply::WrongSequence);
    let (c, _) = err(t.run(ID, "DP-1", 30).await);
    assert_eq!(c, code::RECORDING_INTERRUPTED);
    t.assert_discarded(ID, code::RECORDING_INTERRUPTED, StopReason::LeaseExpired)
        .await;
    // The ignored answers did not stop the requests.
    assert!(t.peer.count(LeasePhase::Record) >= 2);
}

#[tokio::test]
async fn a_lease_discard_answer_discards_with_its_reason() {
    for reason in [
        StopReason::DeckPolicy,
        StopReason::OwnerLoginEnded,
        StopReason::ArtifactDeleted,
        StopReason::Deadline,
    ] {
        let t = T::new().await;
        let run = t.start(ID, "DP-1", 60);
        t.until_recording(ID).await;
        t.peer.set_phase(LeasePhase::Record, Reply::Discard(reason));
        let result = tokio::time::timeout(Duration::from_secs(15), run)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(err(result).0, reason.owner_code(false));
        t.assert_discarded(ID, reason.owner_code(false), reason)
            .await;
    }
}

#[tokio::test]
async fn a_lease_that_is_denied_discards() {
    for reply in [
        Reply::Wire("forbidden"),
        Reply::Transport,
        Reply::Garbage,
        Reply::Discard(StopReason::PolicyOff),
        // The request deadline (0.5 s) passes before the answer: a timeout.
        Reply::Hang,
        Reply::Delay(900, Box::new(Reply::Continue)),
    ] {
        let t = T::new().await;
        let run = t.start(ID, "DP-1", 60);
        t.until_recording(ID).await;
        t.peer.set_phase(LeasePhase::Record, reply);
        let result = tokio::time::timeout(Duration::from_secs(15), run)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(err(result).0, code::RECORDING_INTERRUPTED);
        t.assert_discarded(ID, code::RECORDING_INTERRUPTED, StopReason::LeaseDenied)
            .await;
    }
}

#[tokio::test]
async fn only_one_request_is_open_at_a_time() {
    let t = T::new().await;
    // Each answer takes about two periods; the requests still never overlap.
    t.set_timing(|timing| timing.validity = Duration::from_millis(2500));
    t.peer.set_phase(
        LeasePhase::Record,
        Reply::Delay(320, Box::new(Reply::Continue)),
    );
    let done = t.run(ID, "DP-1", 1).await;
    assert!(done.is_ok(), "{:?}", done.err());
    assert_eq!(t.peer.peak.load(Ordering::SeqCst), 1);
    assert!(t.peer.count(LeasePhase::Record) >= 2);
}

#[tokio::test]
async fn the_import_request_waits_for_a_renewal_in_flight() {
    let t = T::new().await;
    t.set_timing(|timing| timing.request = Duration::from_secs(2));
    t.peer.set_phase(
        LeasePhase::Record,
        Reply::Delay(600, Box::new(Reply::Continue)),
    );
    let done = t.run(ID, "DP-1", 1).await;
    assert!(done.is_ok(), "{:?}", done.err());
    assert_eq!(t.peer.peak.load(Ordering::SeqCst), 1);
    assert_eq!(t.peer.count(LeasePhase::Import), 1);
}

#[tokio::test]
async fn a_request_that_fails_on_a_link_that_ended_is_link_closed() {
    for supersede in [false, true] {
        let peer = Peer::new();
        peer.set_phase(LeasePhase::Record, Reply::Transport);
        if supersede {
            peer.supersede();
        } else {
            peer.close();
        }
        let lease = Lease::new(peer, ID, "h", Jumpy::new(), timing());
        let answered = lease.request(LeasePhase::Record).await;
        assert_eq!(
            answered.verdict,
            Some(Verdict::Discard(StopReason::LinkClosed))
        );
    }
    // On a link that is still open, the same failure is a denial.
    let peer = Peer::new();
    peer.set_phase(LeasePhase::Record, Reply::Transport);
    let lease = Lease::new(peer, ID, "h", Jumpy::new(), timing());
    let answered = lease.request(LeasePhase::Record).await;
    assert_eq!(
        answered.verdict,
        Some(Verdict::Discard(StopReason::LeaseDenied))
    );
}
