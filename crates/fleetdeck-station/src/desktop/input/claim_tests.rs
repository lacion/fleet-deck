//! The durable claim of an input operation (R13 P7 §6.3, §11): the claim
//! before the effect, the refusal of a changed payload, the replay of a done
//! operation, and a claim that a restart does not resend.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use fleetdeck_proto::coordinator_desktop_wire::CoordinatorContext;
use fleetdeck_proto::{SessionId, StationId};
use fleetdeck_testkit::Det;
use serde_json::json;

use super::*;
use crate::store::StoreConfig;

const FP: &str = "ab00000000000000000000000000000000000000000000000000000000000001";
const OP: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAV";

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

#[tokio::test]
async fn a_new_claim_is_written_and_a_done_one_is_replayed() {
    let s = store().await;
    let c = ctx();
    let first = StoreClaim::new(&s, FP, OP, "h1", "input_type_text", &c);
    assert!(!first.created());
    assert_eq!(first.claim().await.unwrap(), Claimed::New);
    assert!(first.created());
    let row = s.desktop_op_get(FP, OP).await.unwrap().unwrap();
    assert_eq!(
        (row.state.as_str(), row.kind.as_str()),
        ("claimed", "input_type_text")
    );
    first.finish(Ok(())).await;
    let row = s.desktop_op_get(FP, OP).await.unwrap().unwrap();
    assert_eq!(row.state, "done");
    assert_eq!(row.result, Some(json!({})));
    // The same operation, hash and kind again: a replay, no new claim, and
    // the replayed call ends nothing.
    let again = StoreClaim::new(&s, FP, OP, "h1", "input_type_text", &c);
    assert_eq!(again.claim().await.unwrap(), Claimed::Replay);
    assert!(!again.created());
    again.finish(Err("input_failed")).await;
    assert_eq!(
        s.desktop_op_get(FP, OP).await.unwrap().unwrap().state,
        "done"
    );
}

#[tokio::test]
async fn a_changed_payload_or_kind_under_the_same_operation_is_refused() {
    let s = store().await;
    let c = ctx();
    let first = StoreClaim::new(&s, FP, OP, "h1", "input_type_text", &c);
    first.claim().await.unwrap();
    first.finish(Ok(())).await;
    for (hash, kind) in [("h2", "input_type_text"), ("h1", "input_key_press")] {
        let other = StoreClaim::new(&s, FP, OP, hash, kind, &c);
        let e = other.claim().await.unwrap_err();
        assert_eq!(e.code, "authority_unknown", "{hash} {kind}");
        assert!(!other.created());
    }
}

#[tokio::test]
async fn a_claimed_operation_is_never_resumed_and_never_ended_by_a_later_call() {
    let s = store().await;
    let c = ctx();
    // The first run claimed and then the station stopped: the row stays
    // `claimed`. A restart sees the row and does not run the input again.
    let crashed = StoreClaim::new(&s, FP, OP, "h1", "input_key_press", &c);
    assert_eq!(crashed.claim().await.unwrap(), Claimed::New);
    let after_restart = StoreClaim::new(&s, FP, OP, "h1", "input_key_press", &c);
    let e = after_restart.claim().await.unwrap_err();
    assert_eq!(e.code, "authority_unknown");
    after_restart.finish(Err("input_failed")).await;
    let row = s.desktop_op_get(FP, OP).await.unwrap().unwrap();
    assert_eq!(row.state, "claimed");
    assert_eq!(row.code, None);
}

#[tokio::test]
async fn a_failure_keeps_its_fixed_code() {
    let s = store().await;
    let c = ctx();
    let claim = StoreClaim::new(&s, FP, OP, "h1", "input_pointer_move", &c);
    claim.claim().await.unwrap();
    claim.finish(Err("focus_changed")).await;
    let row = s.desktop_op_get(FP, OP).await.unwrap().unwrap();
    assert_eq!(row.state, "failed");
    assert_eq!(row.code.as_deref(), Some("focus_changed"));
    assert_eq!(row.result, None);
}
