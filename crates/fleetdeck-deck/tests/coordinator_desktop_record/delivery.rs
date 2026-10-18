//! The idle expiry of the deck-local pass and the final authority check
//! before a recording result is delivered (R13 P5b-2 PRE-PR round 1,
//! findings 1 and 2; contract §13.4).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::Duration;

use super::lease::{LOST, lease_answer, scripted, sql};
use crate::coordinator_desktop_support::*;
use fleetdeck_deck::store::GrantState;
use fleetdeck_deck::store::coordinator_desktop::OpState;
use fleetdeck_deck::store::desktop_captures::CaptureState;
use fleetdeck_proto::recording::{LeaseDecision, LeasePhase, StopReason};
use serde_json::Value;

/// A hook that moves the fixture clock by `seconds`.
fn advance(w: &World, seconds: u64) -> Step {
    let det = w.f.det.clone();
    hook(move || {
        let det = det.clone();
        async move { det.advance(Duration::from_secs(seconds)) }
    })
}

/// A step that gives the grant an idle TTL of 10 s.
fn short_idle(w: &World) -> Step {
    let gid = w.b.grant_id.clone();
    sql(
        w,
        format!("UPDATE tokens SET idle_ttl_ms = 10000 WHERE grant_id = '{gid}'"),
    )
}

const AUTHORITY_LOST: (LeaseDecision, Option<StopReason>) = (
    LeaseDecision::Discard,
    Some(StopReason::CoordinatorAuthorityLost),
);

/// PRE-PR r1 finding 1: the grant is idle-expired and the 30 s sweep did not
/// run; the next record lease discards.
#[tokio::test]
async fn an_idle_expired_grant_discards_the_next_lease_without_the_sweep() {
    let w = rich_world().await;
    let steps = vec![
        short_idle(&w),
        Step::Lease(LeasePhase::Start),
        advance(&w, 11),
        Step::Lease(LeasePhase::Record),
    ];
    let (reply, got) = scripted(&w, steps, LOST).await;
    assert_eq!(
        lease_answer(&got[0]),
        (LeaseDecision::Continue, None),
        "{got:?}"
    );
    assert_eq!(lease_answer(&got[1]), AUTHORITY_LOST, "{got:?}");
    assert_eq!(code(&reply), "authority_lost");
    // No sweep ran: the grant row is still active.
    let grant =
        w.f.store
            .grant(w.b.grant_id.clone())
            .await
            .unwrap()
            .unwrap();
    assert_eq!(grant.state, GrantState::Active);
}

/// PRE-PR r1 finding 1: the same for the import lease.
#[tokio::test]
async fn an_idle_expired_grant_fails_the_import_lease() {
    let w = rich_world().await;
    let steps = vec![
        short_idle(&w),
        Step::Lease(LeasePhase::Start),
        Step::Lease(LeasePhase::Record),
        advance(&w, 11),
        Step::Lease(LeasePhase::Import),
    ];
    let (_, got) = scripted(&w, steps, LOST).await;
    assert_eq!(
        lease_answer(&got[0]),
        (LeaseDecision::Continue, None),
        "{got:?}"
    );
    assert_eq!(
        lease_answer(&got[1]),
        (LeaseDecision::Continue, None),
        "{got:?}"
    );
    assert_eq!(lease_answer(&got[2]), AUTHORITY_LOST, "{got:?}");
}

fn delivery_rows(rows: Vec<(String, String, String)>) -> Vec<Value> {
    rows.into_iter()
        .map(|r| serde_json::from_str::<Value>(&r.2).unwrap())
        .filter(|d| d["stage"] == "delivery")
        .collect()
}

/// PRE-PR r1 finding 2: the grant is revoked after the station commit; the
/// durable result stays and the caller gets no reference.
#[tokio::test]
async fn a_revoke_after_the_station_commit_delivers_no_reference() {
    let w = rich_world().await;
    let store = w.f.store.clone();
    let gid = w.b.grant_id.clone();
    let revoke = hook(move || {
        let (store, gid) = (store.clone(), gid.clone());
        async move {
            store.revoke_grant(gid, "test".into()).await.unwrap();
        }
    });
    let steps = vec![
        Step::Lease(LeasePhase::Start),
        Step::Lease(LeasePhase::Record),
        revoke,
    ];
    let end = Final::Ok(RecSpec::of(&w, 30));
    let (reply, got) = scripted(&w, steps, end).await;
    assert_eq!(got.len(), 2);
    assert_eq!(code(&reply), "authority_lost", "{reply}");
    let o = cap_op(&w, "r1").await;
    assert_eq!(reply["capture_id"], o.operation_id.as_str(), "{reply}");
    assert!(
        !reply.to_string().contains("\"artifact\""),
        "no reference: {reply}"
    );
    // The durable result is kept: committed, indexed, the operation done.
    assert_eq!(o.state, OpState::Done);
    let r = rec_row(&w, &o.operation_id, &w.station()).await;
    assert_eq!(
        (r.capture.state, r.capture.indexed),
        (CaptureState::Committed, true)
    );
    assert_eq!(count(&w, "SELECT COUNT(*) FROM artifacts").await, 1);
    // The deck audit tells the refusal at the delivery stage.
    let rows = delivery_rows(w.audit_rows(Some("desktop.capture")).await);
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0]["code"], "authority_lost");
}

/// The control case of the test above: a valid grant still gets its
/// reference.
#[tokio::test]
async fn a_valid_grant_still_gets_its_reference() {
    let w = rich_world().await;
    let steps = vec![
        Step::Lease(LeasePhase::Start),
        Step::Lease(LeasePhase::Record),
    ];
    let end = Final::Ok(RecSpec::of(&w, 30));
    let (reply, _) = scripted(&w, steps, end).await;
    assert_eq!(reply["result"]["state"], "committed", "{reply}");
    assert_eq!(reply["result"]["artifact"]["artifact_id"], REC_ART);
    let o = cap_op(&w, "r1").await;
    assert_eq!(o.state, OpState::Done);
    let r = rec_row(&w, &o.operation_id, &w.station()).await;
    assert_eq!(
        (r.capture.state, r.capture.indexed),
        (CaptureState::Committed, true)
    );
    assert!(delivery_rows(w.audit_rows(Some("desktop.capture")).await).is_empty());
}
