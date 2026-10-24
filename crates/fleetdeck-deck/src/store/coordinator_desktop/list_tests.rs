//! The caller grant and the recent operations of a grant (R13 P6, decision
//! O12-9 item 17 and the ops route).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use fleetdeck_proto::{SessionId, StationId};

use super::admit_tests::{HOUR_MS, both, grant, new_op, store, t0};
use super::*;
use crate::store::grants::{GrantClass, GrantState};

fn alpha() -> StationId {
    StationId::from("alpha")
}

#[tokio::test]
async fn the_caller_grant_is_found_for_its_station_and_session_only() {
    let (det, s) = store().await;
    let (gid, _) = both(&s).await;
    let now = t0(&det);
    let got = s
        .desktop_caller_grant(&alpha(), &SessionId::from("coord"), now)
        .await
        .unwrap()
        .expect("the grant");
    assert_eq!(got.grant_id, gid);
    assert!(got.binding.is_some(), "the binding comes with the grant");
    for (station, session) in [("alpha", "other"), ("beta", "coord")] {
        let none = s
            .desktop_caller_grant(&StationId::from(station), &SessionId::from(session), now)
            .await
            .unwrap();
        assert!(none.is_none(), "{station}/{session}");
    }
}

#[tokio::test]
async fn a_revoked_or_expired_grant_is_none() {
    let (det, s) = store().await;
    let (gid, _) = both(&s).await;
    let session = SessionId::from("coord");
    let late = t0(&det) + 13 * HOUR_MS;
    assert!(
        s.desktop_caller_grant(&alpha(), &session, late)
            .await
            .unwrap()
            .is_none()
    );
    s.revoke_grant(gid, "test".to_owned()).await.unwrap();
    assert!(
        s.desktop_caller_grant(&alpha(), &session, t0(&det))
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn an_idle_grant_is_expired_in_the_same_transaction() {
    let (det, s) = store().await;
    let (gid, _) = both(&s).await;
    // The idle TTL is 30 minutes; the lifetime is 12 hours.
    let idle = t0(&det) + 31 * 60_000;
    assert!(
        s.desktop_caller_grant(&alpha(), &SessionId::from("coord"), idle)
            .await
            .unwrap()
            .is_none()
    );
    let g = s.grant(gid).await.unwrap().unwrap();
    assert_eq!(g.state, GrantState::Expired);
    assert_eq!(g.revoked_reason.as_deref(), Some("idle"));
}

#[tokio::test]
async fn the_newest_grant_of_the_session_is_the_caller_grant() {
    let (det, s) = store().await;
    let (old, _) = grant(&s, "coord", &[GrantClass::Desktop], |_| {}).await;
    s.revoke_grant(old.clone(), "test".to_owned())
        .await
        .unwrap();
    det.advance(std::time::Duration::from_secs(1));
    let (new, _) = grant(&s, "coord", &[GrantClass::Desktop], |_| {}).await;
    let got = s
        .desktop_caller_grant(&alpha(), &SessionId::from("coord"), t0(&det))
        .await
        .unwrap()
        .expect("the newest grant");
    assert_eq!(got.grant_id, new);
    assert_ne!(old, new);
}

#[tokio::test]
async fn the_recent_operations_are_the_newest_first_and_hold_no_resource_or_hash() {
    let (det, s) = store().await;
    let (gid, ctx) = both(&s).await;
    let mut now = t0(&det);
    for name in ["r1", "r2", "r3"] {
        let op = new_op(&gid, &ctx, name, OpTool::Notify);
        now += 10;
        s.desktop_admit(op, now).await.unwrap();
    }
    s.desktop_op_finish("op-r2", OpOutcome::Failed("busy"), now + 5)
        .await
        .unwrap();
    let rows = s.desktop_ops_recent(&gid, 50).await.unwrap();
    let ids: Vec<&str> = rows.iter().map(|r| r.operation_id.as_str()).collect();
    assert_eq!(ids, ["op-r3", "op-r2", "op-r1"]);
    assert_eq!(rows[1].state, "failed");
    assert_eq!(rows[1].code.as_deref(), Some("busy"));
    assert_eq!(rows[0].code, None);
    assert_eq!(rows[0].tool, "notify");
    assert_eq!(rows[0].target_station, alpha());
    let json = serde_json::to_value(&rows[0]).unwrap();
    let mut keys: Vec<&str> = json
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        [
            "code",
            "created_at",
            "operation_id",
            "state",
            "target_station",
            "tool",
            "updated_at"
        ]
    );
    // The limit and the grant filter.
    assert_eq!(s.desktop_ops_recent(&gid, 2).await.unwrap().len(), 2);
    assert!(
        s.desktop_ops_recent("other-grant", 50)
            .await
            .unwrap()
            .is_empty()
    );
}
