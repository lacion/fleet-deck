//! `coordinator.call` from a fake station (M2 P3-7b; G7, ADR-004 §9.3,
//! §9.6): the link layer needs the `coordinate` grant; the deck decides
//! the call in one transaction with a grant re-check, stores the receipt,
//! returns it for a resend, and refuses a reused `command_id`. Until PLAN
//! Phase 6 every new call is a stored rejection `unsupported` (DL-053).

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::BTreeSet;

use common::*;
use fleetdeck_deck::store::stations::NewStation;
use fleetdeck_link::control::Control;
use fleetdeck_link::methods::code;
use fleetdeck_proto::command::{RETRY_WINDOW_MS, ReceiptState};
use fleetdeck_proto::link::{Outcome, Scope};
use fleetdeck_proto::mutation::{CoordinatorCall, CoordinatorCallEnvelope, CoordinatorOp};
use fleetdeck_proto::{CommandId, IncarnationId, Receipt, RejectCode};
use serde_json::json;

fn call_of(f: &Fixture, id: &str, text: &str, target: IncarnationId) -> CoordinatorCallEnvelope {
    CoordinatorCallEnvelope {
        command_id: CommandId::from(id),
        target_incarnation: target,
        retry_until: f.store.now_ms() + RETRY_WINDOW_MS,
        call: CoordinatorCall {
            caller: "coord-1".into(),
            token_digest: "0".repeat(64),
            op: CoordinatorOp::Send {
                to: "all".to_owned(),
                text: text.to_owned(),
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
    let o = send(&station.control, "k1", &call_of(&f, "k1", "hi", deck_inc)).await;
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

    let env = call_of(&f, "k1", "hi", deck_inc.clone());
    let first = receipt(send(c, "k1", &env).await);
    assert_eq!(first.state, ReceiptState::Rejected);
    assert_eq!(first.reject.as_ref().unwrap().code, RejectCode::Unsupported);
    let again = receipt(send(c, "k1", &env).await);
    assert_eq!(again, first);
    // A resend after `retry_until` still returns the stored receipt.
    f.det.advance(std::time::Duration::from_secs(10 * 60));
    let late = receipt(send(c, "k1", &env).await);
    assert_eq!(late, first);
    assert_eq!(f.store.coordinator_receipts().await.unwrap(), 1);

    // The same id with another payload: refused, not stored.
    let other = call_of(&f, "k1", "another text", deck_inc.clone());
    let r = receipt(send(c, "k1", &other).await);
    assert_eq!(r.reject.unwrap().code, RejectCode::CommandIdReused);

    // Another deck incarnation and a passed `retry_until`: not stored.
    let r = receipt(send(c, "k2", &call_of(&f, "k2", "hi", "old-deck".into())).await);
    assert_eq!(r.reject.unwrap().code, RejectCode::IncarnationMismatch);
    let mut late = call_of(&f, "k3", "hi", deck_inc.clone());
    late.retry_until = f.store.now_ms() - 3 * 60 * 1000;
    let r = receipt(send(c, "k3", &late).await);
    assert_eq!(r.reject.unwrap().code, RejectCode::Expired);
    // A frame id that differs from the envelope's: not stored.
    let r = receipt(send(c, "k4", &call_of(&f, "k5", "hi", deck_inc)).await);
    assert_eq!(r.reject.unwrap().code, RejectCode::InvalidCommand);
    assert_eq!(f.store.coordinator_receipts().await.unwrap(), 1);

    // `coordinator.query` comes with Phase 6.
    let o = tokio::time::timeout(T, c.request("coordinator.query", None, json!({})))
        .await
        .unwrap()
        .unwrap();
    assert!(
        matches!(&o, Outcome::Err(e) if e.code == "not_implemented"),
        "{o:?}"
    );
}

#[tokio::test]
async fn a_grant_removed_in_the_store_refuses_a_call_in_the_decision_transaction() {
    let mut f = fixture_with(coordinate()).await;
    let station = f.connect().await;
    let deck_inc = f.store.incarnation().clone();
    // The stored grant loses `coordinate` while the link layer's cache
    // still has it: the decision transaction refuses the call.
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
    let env = call_of(&f, "k1", "hi", deck_inc);
    let r = receipt(send(&station.control, "k1", &env).await);
    assert_eq!(r.reject.unwrap().code, RejectCode::Forbidden);
    assert_eq!(f.store.coordinator_receipts().await.unwrap(), 0);
}
