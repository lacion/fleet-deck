//! The operation ledger of the coordinator desktop tools (R13 P6 §7.2): the
//! request key, the state changes, the start recovery and the rule that the
//! raw request id is never stored.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use fleetdeck_proto::coordinator_desktop_text::code;
use rusqlite::types::ValueRef;

use fleetdeck_proto::coordinator_desktop_wire::CoordinatorContext;

use super::admit_tests::{both, grant, new_op, rows, store, t0};
use super::*;
use crate::store::grants::GrantClass;
use crate::store::{DeckStore, call};

async fn accepted(s: &DeckStore, gid: &str, ctx: &CoordinatorContext, name: &str, now: i64) {
    let got = s
        .desktop_admit(new_op(gid, ctx, name, OpTool::Notify), now)
        .await
        .unwrap();
    assert!(matches!(got, Admission::Accepted(_)));
}

#[test]
fn the_request_key_is_the_lowercase_hex_sha256_of_the_utf8_bytes() {
    assert_eq!(
        request_key("abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    assert_eq!(request_key("é").len(), 64);
    assert_ne!(request_key("a"), request_key("b"));
}

#[test]
fn the_state_texts_round_trip_and_an_unknown_text_is_refused() {
    for state in [
        OpState::Prepared,
        OpState::Attempted,
        OpState::Done,
        OpState::Failed,
        OpState::Unknown,
    ] {
        assert_eq!(OpState::parse(state.as_str()), Some(state));
    }
    assert_eq!(OpState::parse("running"), None);
    assert_eq!(OpTool::WindowClose.as_str(), "window_close");
    assert_eq!(OpTool::PreviewOpen.as_str(), "preview_open");
    assert_eq!(OpClass::Capture.as_str(), "capture");
}

#[tokio::test]
async fn an_operation_goes_from_prepared_to_attempted_to_done() {
    let (det, s) = store().await;
    let (gid, ctx) = both(&s).await;
    let now = t0(&det);
    accepted(&s, &gid, &ctx, "r1", now).await;
    assert!(s.desktop_op_attempted("op-r1", now + 1).await.unwrap());
    let row = s.desktop_op("op-r1").await.unwrap().unwrap();
    assert_eq!((row.state, row.updated_at), (OpState::Attempted, now + 1));
    // A second `attempted` changes nothing.
    assert!(!s.desktop_op_attempted("op-r1", now + 2).await.unwrap());
    let result = serde_json::json!({"window": "w-1"});
    assert!(
        s.desktop_op_finish("op-r1", OpOutcome::Done(result.clone()), now + 3)
            .await
            .unwrap()
    );
    let row = s.desktop_op("op-r1").await.unwrap().unwrap();
    assert_eq!(
        (row.state, row.result, row.code, row.updated_at),
        (OpState::Done, Some(result), None, now + 3)
    );
    let by_key = s
        .desktop_op_by_key(&gid, &request_key("r1"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(by_key.operation_id, "op-r1");
    assert!(s.desktop_op("op-none").await.unwrap().is_none());
    assert!(
        s.desktop_op_by_key(&gid, &request_key("none"))
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn a_prepared_operation_can_fail_with_a_code_and_end_unknown_from_attempted() {
    let (det, s) = store().await;
    let (gid, ctx) = both(&s).await;
    let now = t0(&det);
    accepted(&s, &gid, &ctx, "r1", now).await;
    accepted(&s, &gid, &ctx, "r2", now).await;
    assert!(
        s.desktop_op_finish("op-r1", OpOutcome::Failed(code::BUSY), now + 1)
            .await
            .unwrap()
    );
    let row = s.desktop_op("op-r1").await.unwrap().unwrap();
    assert_eq!(
        (row.state, row.code.as_deref(), row.result),
        (OpState::Failed, Some(code::BUSY), None)
    );
    s.desktop_op_attempted("op-r2", now + 1).await.unwrap();
    assert!(
        s.desktop_op_finish("op-r2", OpOutcome::Unknown, now + 2)
            .await
            .unwrap()
    );
    let row = s.desktop_op("op-r2").await.unwrap().unwrap();
    assert_eq!((row.state, row.code), (OpState::Unknown, None));
}

#[tokio::test]
async fn a_row_in_a_final_state_never_changes() {
    let (det, s) = store().await;
    let (gid, ctx) = both(&s).await;
    let now = t0(&det);
    for name in ["r1", "r2", "r3"] {
        accepted(&s, &gid, &ctx, name, now).await;
    }
    let result = serde_json::json!({"ok": true});
    s.desktop_op_finish("op-r1", OpOutcome::Done(result.clone()), now + 1)
        .await
        .unwrap();
    s.desktop_op_finish("op-r2", OpOutcome::Failed(code::BUSY), now + 1)
        .await
        .unwrap();
    s.desktop_op_attempted("op-r3", now + 1).await.unwrap();
    s.desktop_op_finish("op-r3", OpOutcome::Unknown, now + 1)
        .await
        .unwrap();
    for name in ["op-r1", "op-r2", "op-r3"] {
        let before = s.desktop_op(name).await.unwrap().unwrap();
        for outcome in [
            OpOutcome::Done(serde_json::json!({"ok": false})),
            OpOutcome::Failed(code::AUTHORITY_LOST),
            OpOutcome::Unknown,
        ] {
            assert!(!s.desktop_op_finish(name, outcome, now + 9).await.unwrap());
        }
        assert!(!s.desktop_op_attempted(name, now + 9).await.unwrap());
        assert_eq!(s.desktop_op(name).await.unwrap().unwrap(), before);
    }
}

#[tokio::test]
async fn the_start_recovery_fails_prepared_rows_and_makes_attempted_rows_unknown() {
    let (det, s) = store().await;
    let (gid, ctx) = both(&s).await;
    let now = t0(&det);
    for name in ["r1", "r2", "r3"] {
        accepted(&s, &gid, &ctx, name, now).await;
    }
    s.desktop_op_attempted("op-r2", now + 1).await.unwrap();
    s.desktop_op_finish("op-r3", OpOutcome::Done(serde_json::json!({})), now + 1)
        .await
        .unwrap();
    assert_eq!(s.desktop_ops_recover(now + 5).await.unwrap(), (1, 1));
    let r1 = s.desktop_op("op-r1").await.unwrap().unwrap();
    assert_eq!(
        (r1.state, r1.code.as_deref(), r1.updated_at),
        (OpState::Failed, Some(code::AUTHORITY_UNKNOWN), now + 5)
    );
    let r2 = s.desktop_op("op-r2").await.unwrap().unwrap();
    assert_eq!((r2.state, r2.code), (OpState::Unknown, None));
    let r3 = s.desktop_op("op-r3").await.unwrap().unwrap();
    assert_eq!((r3.state, r3.updated_at), (OpState::Done, now + 1));
    // The counters are not refunded.
    let g = s.grant(gid).await.unwrap().unwrap();
    assert_eq!((g.desktop_used, g.desktop_attempts), (3, 3));
    assert_eq!(s.desktop_ops_recover(now + 6).await.unwrap(), (0, 0));
}

/// Every column of every row of the three tables, as text.
async fn dump(s: &DeckStore) -> String {
    call(&s.inner.db, |conn| {
        let mut all = String::new();
        for table in [
            "coordinator_desktop_ops",
            "coordinator_desktop_rejects",
            "coordinator_desktop_rate",
        ] {
            let mut stmt = conn.prepare(&format!("SELECT * FROM {table}"))?;
            let columns = stmt.column_count();
            let mut rows = stmt.query([])?;
            while let Some(row) = rows.next()? {
                for i in 0..columns {
                    match row.get_ref(i)? {
                        ValueRef::Text(t) => all.push_str(&String::from_utf8_lossy(t)),
                        other => all.push_str(&format!("{other:?}")),
                    }
                    all.push('|');
                }
            }
        }
        Ok(all)
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn the_raw_request_id_is_in_no_stored_column() {
    let (det, s) = store().await;
    let (gid, ctx) = grant(&s, "coord", &[GrantClass::Desktop], |g| {
        g.desktop_cap = Some(1);
    })
    .await;
    let now = t0(&det);
    let sentinel = "REQUEST-ID-SENTINEL-5f3a9";
    let second = "REQUEST-ID-SENTINEL-77c1e";
    // One accepted operation, one stored refusal (the cap is 1).
    for (n, id) in [sentinel, second].into_iter().enumerate() {
        // The operation id and the payload hash are the caller's own values.
        let mut op = new_op(&gid, &ctx, id, OpTool::Notify);
        op.operation_id = format!("op-{n}");
        op.payload_hash = format!("payload-{n}");
        s.desktop_admit(op, now).await.unwrap();
    }
    // One rate row exists too.
    assert_eq!(rows(&s, "coordinator_desktop_ops").await, 1);
    assert_eq!(rows(&s, "coordinator_desktop_rejects").await, 1);
    assert_eq!(rows(&s, "coordinator_desktop_rate").await, 2);
    let text = dump(&s).await;
    assert!(text.contains(&request_key(sentinel)));
    assert!(text.contains(&request_key(second)));
    assert!(!text.contains("SENTINEL"));
}
