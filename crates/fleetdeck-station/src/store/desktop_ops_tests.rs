//! The durable claim of a coordinator desktop mutation (R13 P6 §7.3): a new
//! claim, a repeat with its stored result, another hash or kind, the terminal
//! rows that never change, the status row, and a claim that survives a store
//! reopen.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use fleetdeck_proto::coordinator_desktop_wire::CoordinatorContext;
use fleetdeck_proto::{SessionId, StationId};
use fleetdeck_testkit::Det;
use serde_json::json;

use super::Claim;
use crate::store::{Store, StoreConfig};

const FP: &str = "ab00000000000000000000000000000000000000000000000000000000000001";
const OTHER_FP: &str = "cd00000000000000000000000000000000000000000000000000000000000002";
const OP: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
const OP2: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAW";

fn ctx() -> CoordinatorContext {
    CoordinatorContext {
        grant_id: "grant-1".to_owned(),
        binding_id: "binding-1".to_owned(),
        generation: 3,
        caller_station: StationId::new("01HZZZZZZZZZZZZZZZZZZZZZZA"),
        caller_session: SessionId::new("01HZZZZZZZZZZZZZZZZZZZZZZB"),
        caller_label: "bold-fox".to_owned(),
    }
}

async fn store() -> Store {
    let det = Det::new();
    let store = Store::open_in_memory(det.clock(), det.ids(), StoreConfig::default())
        .await
        .unwrap();
    store.recover(Vec::new()).await.unwrap();
    store
}

async fn claim(s: &Store, fp: &str, op: &str, hash: &str, kind: &str) -> Claim {
    s.desktop_op_claim(fp, op, hash, kind, &ctx(), 10)
        .await
        .unwrap()
}

#[tokio::test]
async fn a_new_key_is_claimed_and_a_repeat_is_seen_until_it_is_done() {
    let s = store().await;
    assert_eq!(claim(&s, FP, OP, "h1", "notify").await, Claim::New);
    let row = s.desktop_op_get(FP, OP).await.unwrap().unwrap();
    assert_eq!(row.state, "claimed");
    assert_eq!(
        (row.kind.as_str(), row.grant_id.as_str()),
        ("notify", "grant-1")
    );
    assert_eq!(row.caller_station, "01HZZZZZZZZZZZZZZZZZZZZZZA");
    assert_eq!(row.caller_session, "01HZZZZZZZZZZZZZZZZZZZZZZB");
    assert_eq!((row.result, row.code), (None, None));
    // The effect may have run: a repeat is seen and never runs again.
    assert_eq!(claim(&s, FP, OP, "h1", "notify").await, Claim::Seen);
    // Another operation id is another claim.
    assert_eq!(claim(&s, FP, OP2, "h1", "notify").await, Claim::New);
    // The key has the deck fingerprint.
    assert_eq!(claim(&s, OTHER_FP, OP, "h1", "notify").await, Claim::New);
}

#[tokio::test]
async fn a_done_row_returns_its_result_only_for_the_same_hash_and_kind() {
    let s = store().await;
    assert_eq!(claim(&s, FP, OP, "h1", "preview_open").await, Claim::New);
    let result = json!({"window_id": "ab".repeat(16)});
    s.desktop_op_finish(FP, OP, Ok(Some(result.clone())), 11)
        .await
        .unwrap();
    assert_eq!(
        claim(&s, FP, OP, "h1", "preview_open").await,
        Claim::Done(Some(result.clone()))
    );
    // Another hash or another kind never returns the result.
    assert_eq!(claim(&s, FP, OP, "h2", "preview_open").await, Claim::Seen);
    assert_eq!(claim(&s, FP, OP, "h1", "preview_close").await, Claim::Seen);
    let row = s.desktop_op_get(FP, OP).await.unwrap().unwrap();
    assert_eq!((row.state.as_str(), row.result), ("done", Some(result)));
}

#[tokio::test]
async fn a_failed_row_keeps_its_code_and_is_seen() {
    let s = store().await;
    assert_eq!(claim(&s, FP, OP, "h1", "notify").await, Claim::New);
    s.desktop_op_finish(FP, OP, Err("forbidden"), 11)
        .await
        .unwrap();
    let row = s.desktop_op_get(FP, OP).await.unwrap().unwrap();
    assert_eq!(row.state, "failed");
    assert_eq!(row.code.as_deref(), Some("forbidden"));
    assert_eq!(claim(&s, FP, OP, "h1", "notify").await, Claim::Seen);
}

#[tokio::test]
async fn a_terminal_row_never_changes() {
    let s = store().await;
    claim(&s, FP, OP, "h1", "notify").await;
    s.desktop_op_finish(FP, OP, Ok(Some(json!({}))), 11)
        .await
        .unwrap();
    // A later finish does nothing.
    s.desktop_op_finish(FP, OP, Err("failed"), 12)
        .await
        .unwrap();
    s.desktop_op_finish(FP, OP, Ok(Some(json!({"window_id": "x"}))), 13)
        .await
        .unwrap();
    let row = s.desktop_op_get(FP, OP).await.unwrap().unwrap();
    assert_eq!(row.state, "done");
    assert_eq!(row.result, Some(json!({})));
    assert_eq!(row.code, None);
    // A finish of a failed row does nothing either.
    claim(&s, FP, OP2, "h1", "notify").await;
    s.desktop_op_finish(FP, OP2, Err("failed"), 11)
        .await
        .unwrap();
    s.desktop_op_finish(FP, OP2, Ok(None), 12).await.unwrap();
    let row = s.desktop_op_get(FP, OP2).await.unwrap().unwrap();
    assert_eq!(
        (row.state.as_str(), row.code.as_deref()),
        ("failed", Some("failed"))
    );
}

#[tokio::test]
async fn an_unknown_key_has_no_row() {
    let s = store().await;
    assert_eq!(s.desktop_op_get(FP, OP).await.unwrap(), None);
    // Finishing a key that was never claimed creates nothing.
    s.desktop_op_finish(FP, OP, Ok(None), 1).await.unwrap();
    assert_eq!(s.desktop_op_get(FP, OP).await.unwrap(), None);
}

#[tokio::test]
async fn a_claim_survives_a_store_reopen() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("station.db");
    let det = Det::new();
    {
        let s = Store::open(&path, det.clock(), det.ids(), StoreConfig::default())
            .await
            .unwrap();
        s.recover(Vec::new()).await.unwrap();
        assert_eq!(claim(&s, FP, OP, "h1", "notify").await, Claim::New);
        assert_eq!(claim(&s, FP, OP2, "h1", "preview_open").await, Claim::New);
        s.desktop_op_finish(FP, OP2, Ok(Some(json!({"window_id": "w"}))), 11)
            .await
            .unwrap();
    }
    let s = Store::open(&path, det.clock(), det.ids(), StoreConfig::default())
        .await
        .unwrap();
    s.recover(Vec::new()).await.unwrap();
    // A claimed operation is never run again after a restart.
    assert_eq!(claim(&s, FP, OP, "h1", "notify").await, Claim::Seen);
    assert_eq!(
        claim(&s, FP, OP2, "h1", "preview_open").await,
        Claim::Done(Some(json!({"window_id": "w"})))
    );
}

#[tokio::test]
async fn the_three_input_kinds_are_claimed_once_and_bound_to_their_hash() {
    let s = store().await;
    for (n, kind) in ["input_key_press", "input_type_text", "input_pointer_move"]
        .into_iter()
        .enumerate()
    {
        let op = format!("op-{n}");
        assert_eq!(claim(&s, FP, &op, "h1", kind).await, Claim::New, "{kind}");
        let row = s.desktop_op_get(FP, &op).await.unwrap().unwrap();
        assert_eq!((row.state.as_str(), row.kind.as_str()), ("claimed", kind));
        // A repeat, with the same or a changed payload hash, never runs again.
        assert_eq!(claim(&s, FP, &op, "h1", kind).await, Claim::Seen, "{kind}");
        assert_eq!(claim(&s, FP, &op, "h2", kind).await, Claim::Seen, "{kind}");
        s.desktop_op_finish(FP, &op, Ok(Some(json!({"state": "done"}))), 11)
            .await
            .unwrap();
        // Only the same hash and kind get the stored result back.
        assert_eq!(
            claim(&s, FP, &op, "h1", kind).await,
            Claim::Done(Some(json!({"state": "done"}))),
            "{kind}"
        );
        assert_eq!(claim(&s, FP, &op, "h2", kind).await, Claim::Seen, "{kind}");
    }
}
