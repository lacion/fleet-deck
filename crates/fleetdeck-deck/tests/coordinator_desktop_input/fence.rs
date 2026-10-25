//! The `attempted` step of an input operation with the board sign-out (R13
//! P7 §7.6): the exposure snapshot, the sessions that are deleted before the
//! send, and the revoke that does not finish.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::coordinator_desktop_support::*;
use fleetdeck_deck::board_fence::{SessionKind, SocketKind};
use fleetdeck_deck::store::coordinator_desktop::OpState;
use fleetdeck_deck::store::{AuditResult, LoginMethod};
use serde_json::json;

use super::UNKNOWN_TEXT;

/// The machine addresses of the fake target in these tests.
const TARGET_ADDR: &str = "10.7.7.7";

/// A world whose target link has the peer address `TARGET_ADDR` and reports
/// `host_addresses` (a routable one, a loopback one and a link-local one).
async fn fence_world() -> World {
    let w = input_world(10, 100).await;
    w.f.store
        .set_link_peer(&w.station(), 1_000, TARGET_ADDR.parse().unwrap());
    let mut desktop = info();
    desktop.host_addresses = ["10.9.9.9", "127.0.0.1", "fe80::1"]
        .map(str::to_owned)
        .to_vec();
    let mut station_info = crate::common::fleet::claude_info();
    station_info.desktop = Some(desktop);
    w.f.store.set_station_info(&w.station(), station_info);
    w
}

async fn audit_of_deck(w: &World, action: &str) -> Vec<(String, String)> {
    w.f.store
        .audit_log(200)
        .await
        .unwrap()
        .into_iter()
        .filter(|e| e.action == action)
        .map(|e| {
            assert_eq!(e.result, AuditResult::Ok);
            (e.target.unwrap_or_default(), e.detail.unwrap_or_default())
        })
        .collect()
}

#[tokio::test]
async fn the_sessions_of_the_target_machine_are_deleted_before_the_send_and_the_snapshot_is_stored()
{
    let w = fence_world().await;
    let store = w.f.store.clone();
    let on_target = store
        .create_login_at(LoginMethod::Password, Some(TARGET_ADDR))
        .await
        .unwrap();
    let on_host = store
        .create_login_at(LoginMethod::Password, Some("10.9.9.9"))
        .await
        .unwrap();
    let elsewhere = store
        .create_login_at(LoginMethod::Password, Some("10.8.8.8"))
        .await
        .unwrap();
    // The fake station looks at the sessions at the moment of the send.
    let seen: Arc<Mutex<Vec<(bool, bool, bool)>>> = Arc::default();
    let cookies = Arc::new((on_target.cookie, on_host.cookie, elsewhere.cookie));
    let (probe, log, held) = (store.clone(), Arc::clone(&seen), Arc::clone(&cookies));
    w.fake.set(handler(move |req| {
        let (probe, log, held) = (probe.clone(), Arc::clone(&log), Arc::clone(&held));
        async move {
            if req.method == "desktop.act" {
                let state = (
                    probe.login_session(&held.0).await.unwrap().is_some(),
                    probe.login_session(&held.1).await.unwrap().is_some(),
                    probe.login_session(&held.2).await.unwrap().is_some(),
                );
                log.lock().unwrap().push(state);
            }
            Some(input_answer(&req))
        }
    }));
    let r = w.call("c1", key_call("k1", "a", None)).await;
    assert_eq!(r["status"], "ok", "{r}");
    // Before the send: the two sessions of the machine were gone already,
    // the session of another address was there.
    assert_eq!(*seen.lock().unwrap(), vec![(false, false, true)]);
    assert!(store.login_session(&cookies.2).await.unwrap().is_some());
    // The snapshot is the link peer and the reported routable address.
    let o = op_of(&w, "k1").await;
    let (attempted, finished, addrs) = op_times(&w, &o.operation_id).await;
    assert!(attempted.is_some() && finished >= attempted);
    let snapshot: Vec<String> = serde_json::from_str(&addrs.unwrap()).unwrap();
    assert_eq!(snapshot, [TARGET_ADDR, "10.9.9.9"]);
    // One audit row for each deleted session, with the operation and no
    // address.
    let rows = audit_of_deck(&w, "board.signed_out_for_input").await;
    assert_eq!(rows.len(), 2, "{rows:?}");
    for (target, detail) in &rows {
        assert!(target.starts_with("owner:"), "{target}");
        assert_eq!(*detail, format!("rule=1 op={}", o.operation_id));
        assert!(!format!("{target}{detail}").contains("10."));
    }
    // The exposure is live: the address of the machine is exposed now.
    store.invalidate_exposure();
    let exposed = store.exposed_addresses(store.now_ms()).await.unwrap();
    assert!(exposed.contains(TARGET_ADDR) && exposed.contains("10.9.9.9"));
    assert!(!exposed.contains("10.8.8.8"));
}

#[tokio::test]
async fn the_local_station_exposes_the_loopback_addresses_of_the_deck() {
    let w = input_world(10, 100).await;
    w.f.store.set_local_station(w.station());
    let r = w.call("c1", key_call("k1", "a", None)).await;
    assert_eq!(r["status"], "ok", "{r}");
    let o = op_of(&w, "k1").await;
    let addrs = op_times(&w, &o.operation_id).await.2.unwrap();
    let snapshot: Vec<String> = serde_json::from_str(&addrs).unwrap();
    assert!(snapshot.contains(&"127.0.0.1".to_owned()), "{snapshot:?}");
    assert!(snapshot.contains(&"::1".to_owned()), "{snapshot:?}");
    assert!(snapshot.len() <= 40);
}

#[tokio::test]
async fn a_relay_that_does_not_end_in_two_seconds_fails_the_input_before_any_send() {
    let w = fence_world().await;
    let store = w.f.store.clone();
    // A live board socket from the machine whose relay never ends.
    let ticket = store.registry().register(
        "some-session-hash",
        SessionKind::Owner,
        SocketKind::Board,
        TARGET_ADDR,
    );
    assert!(store.registry().go_live(&ticket, || true));
    let started = Instant::now();
    let r = w.call("c1", text_call("t1", "hello", None)).await;
    let took = started.elapsed();
    assert_eq!(code(&r), "input_interrupted");
    assert_eq!(
        r["message"],
        "Input was interrupted. Some input may already have been sent. Do not retry automatically."
    );
    assert!(took >= Duration::from_millis(1_900), "{took:?}");
    // Nothing was sent after the policies; the operation failed.
    assert_eq!(w.fake.count("desktop.act"), 0);
    let o = op_of(&w, "t1").await;
    assert_eq!(r["operation_id"], o.operation_id.as_str());
    assert_eq!(
        (o.state, o.code.as_deref()),
        (OpState::Failed, Some("input_interrupted"))
    );
    // The attempt and the snapshot are durable: the machine stays exposed.
    let (attempted, finished, addrs) = op_times(&w, &o.operation_id).await;
    assert!(attempted.is_some() && finished.is_some());
    let snapshot: Vec<String> = serde_json::from_str(&addrs.unwrap()).unwrap();
    assert_eq!(snapshot, [TARGET_ADDR, "10.9.9.9"]);
    // The socket was revoked; once the relay ends the entry is gone.
    assert!(ticket.is_revoked());
    // Nothing is refunded and the audit holds no text.
    assert_eq!(input_counters(&w).await, (1, 1, 5, 1));
    assert!(!w.audit_text().await.contains("hello"));
    // The status tool tells a failed operation.
    let s = w.query(status_query("t1")).await;
    assert_eq!(s["result"]["state"], "failed");
    assert_eq!(s["result"]["error"], "input_interrupted");
    drop(ticket);
}

#[tokio::test]
async fn the_unknown_text_is_the_one_of_the_contract() {
    // The constant that the tests share is the text of the contract.
    assert_eq!(
        UNKNOWN_TEXT,
        fleetdeck_proto::coordinator_desktop_text::INPUT_OUTCOME_UNKNOWN_TEXT
    );
    assert_eq!(json!(UNKNOWN_TEXT).as_str(), Some(UNKNOWN_TEXT));
}
