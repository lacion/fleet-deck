//! The admission transaction of the coordinator desktop tools (R13 P6 §6.2
//! step 9, §7.1): the request key, the grant recheck, the budgets and the
//! single writer. The helpers here are shared with the sibling test files.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;

use fleetdeck_proto::coordinator_desktop_text::code;
use fleetdeck_proto::coordinator_desktop_wire::CoordinatorContext;
use fleetdeck_proto::mutation::CoordinatorBinding;
use fleetdeck_proto::{SessionId, StationId};
use fleetdeck_testkit::Det;

use super::*;
use crate::store::grants::{GrantClass, GrantState, NewGrant};
use crate::store::{DeckStore, call};

pub(super) const HOUR_MS: i64 = 3_600_000;

pub(super) async fn store() -> (Det, DeckStore) {
    let det = Det::new();
    let s = DeckStore::open_in_memory(det.clock(), det.ids())
        .await
        .unwrap();
    (det, s)
}

/// The clock of the store in milliseconds.
pub(super) fn t0(det: &Det) -> i64 {
    i64::try_from(det.now_ms()).unwrap()
}

/// A bound grant of `session` on `alpha`, with the stations `alpha`.
pub(super) async fn grant(
    s: &DeckStore,
    session: &str,
    classes: &[GrantClass],
    edit: impl FnOnce(&mut NewGrant),
) -> (String, CoordinatorContext) {
    let mut g = NewGrant::new(StationId::from("alpha"), SessionId::from(session));
    g.classes = classes.iter().copied().collect::<BTreeSet<_>>();
    edit(&mut g);
    let g = s.create_grant(g).await.unwrap();
    let bound = s
        .bind_grant(CoordinatorBinding {
            binding_id: "b-1".to_owned(),
            grant_id: g.grant_id.clone(),
            generation: 1,
            provider_instance: "claude".into(),
            native_id: "native-1".to_owned(),
        })
        .await
        .unwrap();
    assert!(bound);
    let ctx = CoordinatorContext {
        grant_id: g.grant_id.clone(),
        binding_id: "b-1".to_owned(),
        generation: 1,
        caller_station: StationId::from("alpha"),
        caller_session: SessionId::from(session),
        caller_label: session.to_owned(),
    };
    (g.grant_id, ctx)
}

/// A desktop grant (`desktop` and `capture`) of the session `coord`.
pub(super) async fn both(s: &DeckStore) -> (String, CoordinatorContext) {
    grant(
        s,
        "coord",
        &[GrantClass::Desktop, GrantClass::Capture],
        |_| {},
    )
    .await
}

/// A new operation for `name` (a request id; the key is its hash).
pub(super) fn new_op(grant_id: &str, ctx: &CoordinatorContext, name: &str, tool: OpTool) -> NewOp {
    NewOp {
        operation_id: format!("op-{name}"),
        grant_id: grant_id.to_owned(),
        request_key: request_key(name),
        payload_hash: format!("payload-{name}"),
        tool,
        class: if tool == OpTool::Screenshot {
            OpClass::Capture
        } else {
            OpClass::Desktop
        },
        ctx: ctx.clone(),
        target_station: StationId::from("alpha"),
        action_hash: None,
        resource: serde_json::json!({"display": "d-1"}),
        seconds: None,
    }
}

/// The number of rows of a table.
pub(super) async fn rows(s: &DeckStore, table: &'static str) -> i64 {
    call(&s.inner.db, move |conn| {
        Ok(conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))?)
    })
    .await
    .unwrap()
}

/// Nothing of the grant was charged and nothing was stored.
async fn assert_untouched(s: &DeckStore, grant_id: &str) {
    let g = s.grant(grant_id.to_owned()).await.unwrap().unwrap();
    assert_eq!(
        (g.desktop_used, g.capture_used, g.desktop_attempts),
        (0, 0, 0)
    );
    for table in [
        "coordinator_desktop_ops",
        "coordinator_desktop_rejects",
        "coordinator_desktop_rate",
    ] {
        assert_eq!(rows(s, table).await, 0, "{table}");
    }
}

pub(super) fn rejected(a: Admission) -> (&'static str, bool) {
    match a {
        Admission::Rejected { code, stored } => (code, stored),
        other => panic!("expected Rejected, got {other:?}"),
    }
}

#[tokio::test]
async fn an_accepted_operation_is_prepared_and_charges_one_action_and_one_attempt() {
    let (det, s) = store().await;
    let (gid, ctx) = both(&s).await;
    let now = t0(&det);
    let Admission::Accepted(row) = s
        .desktop_admit(new_op(&gid, &ctx, "r1", OpTool::Notify), now)
        .await
        .unwrap()
    else {
        panic!("expected Accepted");
    };
    assert_eq!(row.state, OpState::Prepared);
    assert_eq!(
        row.charged,
        Charges {
            desktop: 1,
            capture: 0
        }
    );
    assert_eq!((row.created_at, row.updated_at), (now, now));
    assert_eq!((row.binding_id.as_str(), row.generation), ("b-1", 1));
    assert_eq!(s.desktop_op("op-r1").await.unwrap(), Some(row));
    let g = s.grant(gid).await.unwrap().unwrap();
    assert_eq!(
        (g.desktop_used, g.capture_used, g.desktop_attempts),
        (1, 0, 1)
    );
    assert_eq!(g.last_used_at, Some(now));
}

#[tokio::test]
async fn an_accepted_screenshot_charges_the_capture_counter_too() {
    let (det, s) = store().await;
    let (gid, ctx) = both(&s).await;
    let Admission::Accepted(row) = s
        .desktop_admit(new_op(&gid, &ctx, "r1", OpTool::Screenshot), t0(&det))
        .await
        .unwrap()
    else {
        panic!("expected Accepted");
    };
    assert_eq!(
        row.charged,
        Charges {
            desktop: 1,
            capture: 1
        }
    );
    assert_eq!(row.class, OpClass::Capture);
    let g = s.grant(gid).await.unwrap().unwrap();
    assert_eq!(
        (g.desktop_used, g.capture_used, g.desktop_attempts),
        (1, 1, 1)
    );
}

#[tokio::test]
async fn the_same_key_and_payload_returns_the_stored_row_and_charges_nothing_more() {
    let (det, s) = store().await;
    let (gid, ctx) = both(&s).await;
    let now = t0(&det);
    let op = new_op(&gid, &ctx, "r1", OpTool::Notify);
    let Admission::Accepted(row) = s.desktop_admit(op.clone(), now).await.unwrap() else {
        panic!("expected Accepted");
    };
    let mut again = op;
    again.operation_id = "op-other".to_owned();
    assert_eq!(
        s.desktop_admit(again, now + 5).await.unwrap(),
        Admission::Existing(row)
    );
    let g = s.grant(gid).await.unwrap().unwrap();
    assert_eq!((g.desktop_used, g.desktop_attempts), (1, 1));
    assert_eq!(rows(&s, "coordinator_desktop_ops").await, 1);
}

#[tokio::test]
async fn the_same_key_with_another_payload_is_a_conflict() {
    let (det, s) = store().await;
    let (gid, ctx) = both(&s).await;
    let now = t0(&det);
    let op = new_op(&gid, &ctx, "r1", OpTool::Notify);
    s.desktop_admit(op.clone(), now).await.unwrap();
    let mut other = op;
    other.payload_hash = "payload-changed".to_owned();
    assert_eq!(
        s.desktop_admit(other, now).await.unwrap(),
        Admission::Conflict
    );
    let g = s.grant(gid).await.unwrap().unwrap();
    assert_eq!((g.desktop_used, g.desktop_attempts), (1, 1));
}

#[tokio::test]
async fn a_stored_refusal_replays_with_the_same_code_and_another_payload_conflicts() {
    let (det, s) = store().await;
    let (gid, ctx) = grant(&s, "coord", &[GrantClass::Desktop], |g| {
        g.desktop_cap = Some(0);
    })
    .await;
    let now = t0(&det);
    let op = new_op(&gid, &ctx, "r1", OpTool::Notify);
    assert_eq!(
        rejected(s.desktop_admit(op.clone(), now).await.unwrap()),
        (code::DESKTOP_CAP_REACHED, true)
    );
    // The cap rises; the stored refusal still answers the same key.
    s.desktop_caps_update(&gid, Some(10), None, None, true)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        rejected(s.desktop_admit(op.clone(), now + 1).await.unwrap()),
        (code::DESKTOP_CAP_REACHED, true)
    );
    assert_eq!(
        s.desktop_reject_by_key(&gid, &op.request_key)
            .await
            .unwrap(),
        Some((
            "payload-r1".to_owned(),
            code::DESKTOP_CAP_REACHED.to_owned()
        ))
    );
    let mut other = op;
    other.payload_hash = "payload-changed".to_owned();
    assert_eq!(
        s.desktop_admit(other, now + 2).await.unwrap(),
        Admission::Conflict
    );
    // One stored refusal counted one attempt; a replay counts none.
    let g = s.grant(gid).await.unwrap().unwrap();
    assert_eq!((g.desktop_used, g.desktop_attempts), (0, 1));
}

#[tokio::test]
async fn a_replay_of_a_known_request_needs_the_grant_in_the_same_transaction() {
    let (det, s) = store().await;
    let (gid, ctx) = both(&s).await;
    let now = t0(&det);
    let op = new_op(&gid, &ctx, "r1", OpTool::Notify);
    let Admission::Accepted(_) = s.desktop_admit(op.clone(), now).await.unwrap() else {
        panic!("expected Accepted");
    };
    // Another generation: the stored row is not returned.
    let mut stale = ctx.clone();
    stale.generation = 2;
    let mut replay = op.clone();
    replay.ctx = stale;
    let got = s.desktop_admit(replay, now + 1).await.unwrap();
    assert_eq!(rejected(got), (code::BINDING_STALE, false));
    // A revoked grant: not granted for the known key, nothing is returned.
    s.revoke_grant(gid.clone(), "test".to_owned())
        .await
        .unwrap();
    let got = s.desktop_admit(op, now + 2).await.unwrap();
    assert_eq!(rejected(got), (code::NOT_GRANTED, false));
    // Nothing was charged or stored by the refused replays.
    let g = s.grant(gid).await.unwrap().unwrap();
    assert_eq!((g.desktop_used, g.desktop_attempts), (1, 1));
    assert_eq!(rows(&s, "coordinator_desktop_ops").await, 1);
}

#[tokio::test]
async fn a_stored_refusal_is_not_replayed_to_a_grant_that_is_gone() {
    let (det, s) = store().await;
    let (gid, ctx) = grant(&s, "coord", &[GrantClass::Desktop], |g| {
        g.desktop_cap = Some(0);
    })
    .await;
    let now = t0(&det);
    let op = new_op(&gid, &ctx, "r1", OpTool::Notify);
    assert_eq!(
        rejected(s.desktop_admit(op.clone(), now).await.unwrap()),
        (code::DESKTOP_CAP_REACHED, true)
    );
    s.revoke_grant(gid.clone(), "test".to_owned())
        .await
        .unwrap();
    assert_eq!(
        rejected(s.desktop_admit(op, now + 1).await.unwrap()),
        (code::NOT_GRANTED, false)
    );
}

#[tokio::test]
async fn a_revoked_grant_is_not_granted_and_nothing_is_charged() {
    let (det, s) = store().await;
    let (gid, ctx) = both(&s).await;
    s.revoke_grant(gid.clone(), "test".to_owned())
        .await
        .unwrap();
    let got = s
        .desktop_admit(new_op(&gid, &ctx, "r1", OpTool::Notify), t0(&det))
        .await
        .unwrap();
    assert_eq!(rejected(got), (code::NOT_GRANTED, false));
    assert_untouched(&s, &gid).await;
}

#[tokio::test]
async fn an_expired_grant_is_not_granted_and_nothing_is_charged() {
    let (det, s) = store().await;
    let (gid, ctx) = both(&s).await;
    let late = t0(&det) + 13 * HOUR_MS;
    let got = s
        .desktop_admit(new_op(&gid, &ctx, "r1", OpTool::Notify), late)
        .await
        .unwrap();
    assert_eq!(rejected(got), (code::NOT_GRANTED, false));
    assert_untouched(&s, &gid).await;
}

#[tokio::test]
async fn an_idle_grant_expires_and_is_not_granted() {
    let (det, s) = store().await;
    let (gid, ctx) = both(&s).await;
    // The idle TTL is 30 minutes; the lifetime is 12 hours.
    let idle = t0(&det) + 31 * 60_000;
    let got = s
        .desktop_admit(new_op(&gid, &ctx, "r1", OpTool::Notify), idle)
        .await
        .unwrap();
    assert_eq!(rejected(got), (code::NOT_GRANTED, false));
    let g = s.grant(gid.clone()).await.unwrap().unwrap();
    assert_eq!(g.state, GrantState::Expired);
    assert_eq!(g.revoked_reason.as_deref(), Some("idle"));
    assert_untouched(&s, &gid).await;
}

/// A call whose context differs from the bound grant is refused and charges
/// nothing.
async fn refused_for_context(edit: impl FnOnce(&mut CoordinatorContext), expect: &'static str) {
    let (det, s) = store().await;
    let (gid, mut ctx) = both(&s).await;
    edit(&mut ctx);
    let got = s
        .desktop_admit(new_op(&gid, &ctx, "r1", OpTool::Notify), t0(&det))
        .await
        .unwrap();
    assert_eq!(rejected(got), (expect, false));
    assert_untouched(&s, &gid).await;
}

#[tokio::test]
async fn another_caller_station_is_not_granted_and_nothing_is_charged() {
    refused_for_context(
        |c| c.caller_station = StationId::from("beta"),
        code::NOT_GRANTED,
    )
    .await;
}

#[tokio::test]
async fn another_caller_session_is_not_granted_and_nothing_is_charged() {
    refused_for_context(
        |c| c.caller_session = SessionId::from("intruder"),
        code::NOT_GRANTED,
    )
    .await;
}

#[tokio::test]
async fn another_binding_id_is_binding_stale_and_nothing_is_charged() {
    refused_for_context(|c| c.binding_id = "b-2".to_owned(), code::BINDING_STALE).await;
}

#[tokio::test]
async fn another_generation_is_binding_stale_and_nothing_is_charged() {
    refused_for_context(|c| c.generation = 2, code::BINDING_STALE).await;
}

#[tokio::test]
async fn an_unknown_grant_is_not_granted() {
    let (det, s) = store().await;
    let (_, ctx) = both(&s).await;
    let got = s
        .desktop_admit(
            new_op("no-such-grant", &ctx, "r1", OpTool::Notify),
            t0(&det),
        )
        .await
        .unwrap();
    assert_eq!(rejected(got), (code::NOT_GRANTED, false));
}

#[tokio::test]
async fn a_grant_without_the_class_of_the_tool_is_not_granted() {
    let (det, s) = store().await;
    let (gid, ctx) = grant(&s, "coord", &[GrantClass::Desktop], |_| {}).await;
    let got = s
        .desktop_admit(new_op(&gid, &ctx, "r1", OpTool::Screenshot), t0(&det))
        .await
        .unwrap();
    assert_eq!(rejected(got), (code::NOT_GRANTED, false));
    assert_untouched(&s, &gid).await;
}

#[tokio::test]
async fn a_target_station_outside_the_grant_is_not_granted() {
    let (det, s) = store().await;
    let (gid, ctx) = both(&s).await;
    let mut op = new_op(&gid, &ctx, "r1", OpTool::Notify);
    op.target_station = StationId::from("beta");
    let got = s.desktop_admit(op, t0(&det)).await.unwrap();
    assert_eq!(rejected(got), (code::NOT_GRANTED, false));
    assert_untouched(&s, &gid).await;
}

#[tokio::test]
async fn a_record_monitor_row_keeps_its_seconds_and_reads_back() {
    let (det, s) = store().await;
    let (gid, ctx) = grant(
        &s,
        "coord",
        &[GrantClass::Desktop, GrantClass::Capture],
        |g| {
            g.recording_cap = 5;
            g.recording_second_cap = 600;
        },
    )
    .await;
    let now = t0(&det);
    let mut op = new_op(&gid, &ctx, "rec", OpTool::RecordMonitor);
    op.class = OpClass::Capture;
    op.seconds = Some(45);
    let Admission::Accepted(row) = s.desktop_admit(op, now).await.unwrap() else {
        panic!("expected Accepted");
    };
    assert_eq!(row.tool, OpTool::RecordMonitor);
    assert_eq!(row.tool.as_str(), "record_monitor");
    assert_eq!(row.seconds, Some(45));
    let back = s.desktop_op(&row.operation_id).await.unwrap().unwrap();
    assert_eq!(back, row);
    // Another tool has no seconds.
    let Admission::Accepted(other) = s
        .desktop_admit(new_op(&gid, &ctx, "n1", OpTool::Notify), now)
        .await
        .unwrap()
    else {
        panic!("expected Accepted");
    };
    assert_eq!(other.seconds, None);
    assert_eq!(
        s.desktop_op(&other.operation_id)
            .await
            .unwrap()
            .unwrap()
            .seconds,
        None
    );
    // The column takes only 1 to 120.
    let bad = call(&s.inner.db, |conn| {
        Ok(conn
            .execute(
                "UPDATE coordinator_desktop_ops SET seconds = 121 WHERE operation_id = 'op-rec'",
                [],
            )
            .is_err())
    })
    .await
    .unwrap();
    assert!(bad);
}
