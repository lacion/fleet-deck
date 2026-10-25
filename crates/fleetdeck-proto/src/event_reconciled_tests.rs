//! `session.execution_reconciled` (the owner) and `session.execution_settled`
//! (the station's own proof, DL-432) across deck versions.

use super::*;
use serde_json::json;

/// The decode of a deck built before DL-432: it knows the owner's kind and
/// not the station's kind (the rule of `EventBody`: an unknown kind is kept
/// verbatim, a known kind ignores extra fields).
fn older_deck_store(wire: &Value) -> Value {
    #[derive(Serialize, Deserialize)]
    #[serde(tag = "kind")]
    enum OlderEvent {
        #[serde(rename = "session.execution_reconciled")]
        ExecutionReconciled { shim_id: String },
    }
    let kind = wire["kind"].as_str().unwrap();
    if kind == "session.execution_reconciled" {
        let known: OlderEvent = serde_json::from_value(wire.clone()).unwrap();
        serde_json::to_value(known).unwrap()
    } else {
        wire.clone()
    }
}

#[test]
fn the_owner_and_the_station_proof_have_separate_kinds() {
    let owner = Event::ExecutionReconciled {
        shim_id: "s".into(),
    };
    let owner = serde_json::to_value(owner).unwrap();
    assert_eq!(
        owner,
        json!({"kind": "session.execution_reconciled", "shim_id": "s"})
    );
    let station = Event::ExecutionSettled {
        shim_id: "s".into(),
    };
    let station = serde_json::to_value(station).unwrap();
    assert_eq!(
        station,
        json!({"kind": "session.execution_settled", "shim_id": "s"})
    );
}

#[test]
fn a_station_proof_stored_by_an_older_deck_stays_a_station_proof() {
    let wire = serde_json::to_value(Event::ExecutionSettled {
        shim_id: "s".into(),
    })
    .unwrap();
    // The older deck stores the event as an unknown kind, verbatim.
    let stored = older_deck_store(&wire);
    assert_eq!(stored, wire);
    let envelope = EventEnvelope {
        station_seq: 7,
        session_id: SessionId::from("01JSESSION"),
        session_seq: 3,
        at: 1,
        proto_version: 1,
        raw_ref: None,
        event: serde_json::from_value(stored).unwrap(),
    };
    let saved = serde_json::to_string(&envelope).unwrap();
    // After the deck upgrade, the saved row reads as the station's proof,
    // never as the owner's reconciliation.
    let read: EventEnvelope = serde_json::from_str(&saved).unwrap();
    assert_eq!(
        read.event,
        EventBody::Known(Event::ExecutionSettled {
            shim_id: "s".into()
        })
    );
    assert!(!matches!(
        read.event.known(),
        Some(Event::ExecutionReconciled { .. })
    ));
}

#[test]
fn an_owner_reconciliation_stored_by_an_older_deck_is_unchanged() {
    let wire = serde_json::to_value(Event::ExecutionReconciled {
        shim_id: "s".into(),
    })
    .unwrap();
    let stored = older_deck_store(&wire);
    assert_eq!(stored, wire);
    let read: EventBody = serde_json::from_value(stored).unwrap();
    assert_eq!(
        read,
        EventBody::Known(Event::ExecutionReconciled {
            shim_id: "s".into()
        })
    );
}
