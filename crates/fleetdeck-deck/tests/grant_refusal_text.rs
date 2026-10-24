//! The link refusal of `coordinate` says why the grant ended (G6, design
//! `grant-idle.md` §3.6): the deck gives the text through
//! `GrantSource::refusal`, over a real link.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::time::Duration;

use common::Fixture;
use common::fleet::{Rig, bound_edit, query, rig, stop};
use fleetdeck_deck::store::GrantClass;
use fleetdeck_proto::CommandId;
use fleetdeck_proto::command::{GrantScope, RETRY_WINDOW_MS};
use fleetdeck_proto::link::{Outcome, WireError};
use fleetdeck_proto::mutation::{
    CoordinatorBinding, CoordinatorCall, CoordinatorCallEnvelope, CoordinatorQuery,
};
use serde_json::json;

const IDLE_TEXT: &str =
    "Your fleet grant expired (idle for 1 second). The owner can renew it on the board.";
const LIFETIME_TEXT: &str =
    "Your fleet grant expired (its lifetime ended). The owner can give a new grant on the board.";
const REVOKED_TEXT: &str =
    "Your fleet grant was revoked. The owner can give a new grant on the board.";

/// A bound fleet grant of `coord-1` with the given times.
async fn grant(r: &Rig, ttl_ms: i64, idle_ttl_ms: i64) -> CoordinatorBinding {
    bound_edit(&r.f, &r.f.station, "coord-1", &[GrantClass::Drive], |g| {
        g.scope = GrantScope::Fleet;
        g.ttl_ms = ttl_ms;
        g.idle_ttl_ms = idle_ttl_ms;
    })
    .await
}

/// The call envelope of `b` that stops `w1`.
fn call_envelope(f: &Fixture, b: &CoordinatorBinding, id: &str) -> CoordinatorCallEnvelope {
    CoordinatorCallEnvelope {
        command_id: CommandId::from(id),
        target_incarnation: f.store.incarnation().clone(),
        retry_until: f.store.now_ms() + RETRY_WINDOW_MS,
        call: CoordinatorCall {
            caller: b.native_id.strip_prefix("n-").unwrap().into(),
            binding: b.clone(),
            idempotency_key: None,
            op: stop(f, "w1"),
        },
    }
}

/// The sweep, then the refresh of the link cache that follows its notes.
async fn sweep(r: &Rig) {
    r.f.store.sweep_grants().await.unwrap();
    r.f.ctx.peers.refresh_changed().await.unwrap();
}

fn refused(outcome: Outcome) -> WireError {
    match outcome {
        Outcome::Err(e) => e,
        Outcome::Ok(v) => panic!("the request must be refused, got {v}"),
    }
}

/// `coordinator.query` of `b` from the fake station over the link.
async fn link_query(r: &Rig, b: &CoordinatorBinding) -> WireError {
    let env = query(&r.f, b, CoordinatorQuery::ListSessions);
    let out = r
        .station
        .control
        .request("coordinator.query", None, json!(env))
        .await
        .unwrap();
    refused(out)
}

#[tokio::test]
async fn a_link_refused_query_of_an_idle_expired_grant_says_why() {
    let r = rig().await;
    let b = grant(&r, 600_000, 1_000).await;
    r.f.det.advance(Duration::from_secs(2));
    sweep(&r).await;
    let e = link_query(&r, &b).await;
    assert_eq!(e.code, "forbidden");
    assert_eq!(e.message, IDLE_TEXT);
}

#[tokio::test]
async fn a_link_refused_call_of_a_lifetime_ended_or_revoked_grant_says_why() {
    let r = rig().await;
    let b = grant(&r, 5_000, 600_000).await;
    r.f.det.advance(Duration::from_millis(5_001));
    sweep(&r).await;
    let id = CommandId::from("k1");
    let env = call_envelope(&r.f, &b, "k1");
    let out = r
        .station
        .control
        .request("coordinator.call", Some(&id), json!(env))
        .await
        .unwrap();
    let e = refused(out);
    assert_eq!(
        (e.code.as_str(), e.message.as_str()),
        ("forbidden", LIFETIME_TEXT)
    );

    let grant_id = b.grant_id.clone();
    assert!(
        r.f.store
            .revoke_grant(grant_id, "owner".to_owned())
            .await
            .unwrap()
    );
    r.f.ctx.peers.refresh_changed().await.unwrap();
    let id = CommandId::from("k2");
    let out = r
        .station
        .control
        .request(
            "coordinator.call",
            Some(&id),
            json!(call_envelope(&r.f, &b, "k2")),
        )
        .await
        .unwrap();
    assert_eq!(refused(out).message, REVOKED_TEXT);
}

#[tokio::test]
async fn a_caller_without_a_grant_keeps_the_generic_text() {
    let r = rig().await;
    let b = grant(&r, 600_000, 1_000).await;
    r.f.det.advance(Duration::from_secs(2));
    sweep(&r).await;
    let mut other = query(&r.f, &b, CoordinatorQuery::ListSessions);
    other.caller = "w1".into();
    let out = r
        .station
        .control
        .request("coordinator.query", None, json!(other))
        .await
        .unwrap();
    let e = refused(out);
    assert_eq!(e.code, "forbidden");
    assert_eq!(
        e.message,
        "`coordinator.query` needs scope coordinate, which the pairing did not grant"
    );
}

#[tokio::test]
async fn a_link_refused_query_after_an_idle_end_and_then_the_lifetime_says_the_lifetime_ended() {
    let r = rig().await;
    let b = grant(&r, 10_000, 1_000).await;
    r.f.det.advance(Duration::from_secs(2));
    sweep(&r).await;
    assert_eq!(link_query(&r, &b).await.message, IDLE_TEXT);
    // No scope change: the cached end is classified again at the refusal.
    r.f.det.advance(Duration::from_secs(10));
    sweep(&r).await;
    assert_eq!(link_query(&r, &b).await.message, LIFETIME_TEXT);
}
