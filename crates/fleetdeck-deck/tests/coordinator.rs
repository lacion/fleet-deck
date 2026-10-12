//! `coordinator.call` from a fake station (M2 P3-7b; G7, ADR-004 §9.3,
//! §9.6): the link layer needs the `coordinate` grant; the deck decides
//! the call in one transaction with a grant re-check, stores the receipt,
//! returns it for a resend, and refuses a reused `command_id`. M3 6-5: the
//! caller needs a bound grant (ADR-006 §6.2); the op checks of the grant
//! are in `src/store/coordinator_tests.rs`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::BTreeSet;

use common::*;
use fleetdeck_deck::store::NewGrant;
use fleetdeck_deck::store::stations::NewStation;
use fleetdeck_link::control::Control;
use fleetdeck_link::methods::code;
use fleetdeck_proto::command::{RETRY_WINDOW_MS, ReceiptState};
use fleetdeck_proto::link::{Outcome, Scope};
use fleetdeck_proto::mutation::{
    CoordinatorBinding, CoordinatorCall, CoordinatorCallEnvelope, CoordinatorDelivery,
    CoordinatorOp, CoordinatorTarget,
};
use fleetdeck_proto::{CommandId, IncarnationId, Receipt, RejectCode};
use serde_json::json;

/// A bound grant of `coord-1` (no binding before the first grant: the
/// binding facts are then empty and the deck refuses with `forbidden`).
async fn bound(f: &Fixture) -> CoordinatorBinding {
    let grant = f
        .store
        .create_grant(NewGrant::new(f.station.clone(), "coord-1".into()))
        .await
        .unwrap();
    let binding = CoordinatorBinding {
        binding_id: "b-1".to_owned(),
        grant_id: grant.grant_id,
        generation: 1,
        provider_instance: "claude".into(),
        native_id: "native-1".to_owned(),
    };
    assert!(f.store.bind_grant(binding.clone()).await.unwrap());
    binding
}

fn unbound() -> CoordinatorBinding {
    CoordinatorBinding {
        binding_id: "b-0".to_owned(),
        grant_id: "g-0".to_owned(),
        generation: 1,
        provider_instance: "claude".into(),
        native_id: "native-0".to_owned(),
    }
}

fn call_of(
    f: &Fixture,
    binding: &CoordinatorBinding,
    id: &str,
    text: &str,
    target: IncarnationId,
) -> CoordinatorCallEnvelope {
    CoordinatorCallEnvelope {
        command_id: CommandId::from(id),
        target_incarnation: target,
        retry_until: f.store.now_ms() + RETRY_WINDOW_MS,
        call: CoordinatorCall {
            caller: "coord-1".into(),
            binding: binding.clone(),
            idempotency_key: None,
            op: CoordinatorOp::Send {
                to: CoordinatorTarget {
                    station: f.station.clone(),
                    session_id: "worker-1".into(),
                },
                text: text.to_owned(),
                delivery: CoordinatorDelivery::FollowUp,
            },
        },
    }
}

async fn send(c: &Control, id: &str, env: &CoordinatorCallEnvelope) -> Outcome {
    let cid = CommandId::from(id);
    tokio::time::timeout(T, c.request("coordinator.call", Some(&cid), json!(env)))
        .await
        .expect("a response")
        .unwrap()
}

fn receipt(o: Outcome) -> Receipt {
    match o {
        Outcome::Ok(v) => serde_json::from_value(v).unwrap(),
        Outcome::Err(e) => panic!("refused: {e}"),
    }
}

fn coordinate() -> BTreeSet<Scope> {
    BTreeSet::from([Scope::Admin, Scope::Coordinate])
}

#[tokio::test]
async fn a_station_without_the_coordinate_grant_is_refused_by_the_link_layer() {
    let mut f = fixture().await;
    let station = f.connect().await;
    let deck_inc = f.store.incarnation().clone();
    let o = send(
        &station.control,
        "k1",
        &call_of(&f, &unbound(), "k1", "hi", deck_inc),
    )
    .await;
    assert!(
        matches!(&o, Outcome::Err(e) if e.code == code::FORBIDDEN),
        "{o:?}"
    );
    assert_eq!(f.store.coordinator_receipts().await.unwrap(), 0);
}

#[tokio::test]
async fn a_call_is_decided_once_and_a_resend_returns_the_stored_receipt() {
    let mut f = fixture_with(coordinate()).await;
    let station = f.connect().await;
    let c = &station.control;
    let deck_inc = f.store.incarnation().clone();
    let b = bound(&f).await;

    // The target session is not in the read model: a stored rejection.
    let env = call_of(&f, &b, "k1", "hi", deck_inc.clone());
    let first = receipt(send(c, "k1", &env).await);
    assert_eq!(first.state, ReceiptState::Rejected);
    assert_eq!(
        first.reject.as_ref().unwrap().code,
        RejectCode::UnknownSession
    );
    let again = receipt(send(c, "k1", &env).await);
    assert_eq!(again, first);
    // A resend after `retry_until` still returns the stored receipt.
    f.det.advance(std::time::Duration::from_secs(10 * 60));
    let late = receipt(send(c, "k1", &env).await);
    assert_eq!(late, first);
    assert_eq!(f.store.coordinator_receipts().await.unwrap(), 1);

    // The same id with another payload: refused, not stored.
    let other = call_of(&f, &b, "k1", "another text", deck_inc.clone());
    let r = receipt(send(c, "k1", &other).await);
    assert_eq!(r.reject.unwrap().code, RejectCode::CommandIdReused);

    // Another deck incarnation and a passed `retry_until`: not stored.
    let r = receipt(send(c, "k2", &call_of(&f, &b, "k2", "hi", "old-deck".into())).await);
    assert_eq!(r.reject.unwrap().code, RejectCode::IncarnationMismatch);
    let mut late = call_of(&f, &b, "k3", "hi", deck_inc.clone());
    late.retry_until = f.store.now_ms() - 3 * 60 * 1000;
    let r = receipt(send(c, "k3", &late).await);
    assert_eq!(r.reject.unwrap().code, RejectCode::Expired);
    // A frame id that differs from the envelope's: not stored.
    let r = receipt(send(c, "k4", &call_of(&f, &b, "k5", "hi", deck_inc)).await);
    assert_eq!(r.reject.unwrap().code, RejectCode::InvalidCommand);
    assert_eq!(f.store.coordinator_receipts().await.unwrap(), 1);

    // `coordinator.query` (M3 6-5): a malformed query is a protocol error;
    // the store tests cover the answers.
    let o = tokio::time::timeout(T, c.request("coordinator.query", None, json!({})))
        .await
        .unwrap()
        .unwrap();
    assert!(
        matches!(&o, Outcome::Err(e) if e.code == "protocol_error"),
        "{o:?}"
    );
}

#[tokio::test]
async fn a_grant_removed_in_the_store_refuses_a_call_in_the_decision_transaction() {
    let mut f = fixture_with(coordinate()).await;
    let station = f.connect().await;
    let deck_inc = f.store.incarnation().clone();
    // The owner's grant exists first (creating it adds `coordinate`,
    // DL-211). Then the stored station loses `coordinate` while the link
    // layer's cache still has it: the decision transaction refuses the call.
    let b = bound(&f).await;
    let rec = f.store.station(f.station.clone()).await.unwrap().unwrap();
    f.store
        .pin_station(NewStation {
            key: rec.key,
            name: rec.name,
            scopes: BTreeSet::from([Scope::Admin]),
            dial_url: rec.dial_url,
        })
        .await
        .unwrap();
    let env = call_of(&f, &b, "k1", "hi", deck_inc);
    let r = receipt(send(&station.control, "k1", &env).await);
    assert_eq!(r.reject.unwrap().code, RejectCode::Forbidden);
    assert_eq!(f.store.coordinator_receipts().await.unwrap(), 0);
}

/// M3 6-5 (ADR-006 §6.1, §6.6): the station's `LinkForward` targets the
/// deck incarnation that the deck's `hello` named; a tool call over a real
/// link comes back as the deck's stored receipt, and a read tool as the
/// deck's query answer.
#[tokio::test]
async fn a_station_forwards_tool_calls_to_the_deck_incarnation_of_its_link() {
    use fleetdeck_station::coordinator::{Caller, Forward, LinkForward};
    let mut f = fixture_with(coordinate()).await;
    let station = f.connect().await;
    assert_eq!(&station.peer_incarnation, f.store.incarnation());
    let b = bound(&f).await;
    let store = f.store.clone();
    let forward = LinkForward::with_clock(std::sync::Arc::new(move || store.now_ms()));
    let caller = Caller {
        binding_id: b.binding_id.clone(),
        grant_id: b.grant_id.clone(),
        session_id: "coord-1".into(),
        provider_instance: "claude".to_owned(),
        native_id: b.native_id.clone(),
        generation: b.generation,
    };
    let serial = forward.connect(station.control.clone(), station.peer_incarnation.clone());
    // An unknown target: a stored `unknown_session` receipt, not an error.
    let args = json!({"to": {"station": f.station.0, "session_id": "nobody"}, "text": "hi"});
    let v = forward
        .call(caller.clone(), "send_mail", args)
        .await
        .unwrap();
    let r: Receipt = serde_json::from_value(v).unwrap();
    assert_eq!(r.state, ReceiptState::Rejected);
    let reject = r.reject.unwrap();
    assert_eq!(reject.code, RejectCode::UnknownSession, "{reject:?}");
    assert_eq!(f.store.coordinator_receipts().await.unwrap(), 1);
    // A read tool: the deck's answer, filtered to the grant.
    let v = forward
        .call(caller.clone(), "list_sessions", json!({}))
        .await
        .unwrap();
    assert!(v["sessions"].is_array(), "{v}");
    // A stale binding generation: the deck's rejection, nothing stored.
    let stale = Caller {
        generation: 9,
        ..caller.clone()
    };
    let v = forward
        .call(stale, "list_sessions", json!({}))
        .await
        .unwrap();
    assert_eq!(v["reject"]["code"], "binding_stale");
    // After the link ends, a call fails at once.
    forward.disconnect(serial);
    let e = forward.call(caller, "list_sessions", json!({})).await;
    assert!(e.unwrap_err().starts_with("deck_unreachable"));
}
