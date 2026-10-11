//! The command methods of the board API over loopback HTTPS (M2 P3-9a;
//! PLAN §9, §8.4, L15): a command reaches the fake station once, a resend
//! with the same `command_id` sends nothing new, per-method scopes, CSRF,
//! the refusals, a command for an unreachable station waits `pending` and
//! goes out once when the link returns, and every call is audited.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::BTreeSet;
use std::time::Duration;

use common::board::{BoardClient, Http, board};
use common::*;
use fleetdeck_deck::store::AuditResult;
use fleetdeck_proto::command::{ApprovalDecision, DeckCommandState, ReceiptState};
use fleetdeck_proto::link::Scope;
use fleetdeck_proto::{CommandId, RejectCode};
use serde_json::{Value, json};

const DAY: i64 = 24 * 60 * 60 * 1000;

fn interrupt_body(id: &str, session: &str) -> Value {
    json!({
        "station": "fake",
        "command_id": id,
        "command": { "op": "interrupt", "session_id": session },
    })
}

async fn owner(c: &BoardClient) -> Vec<(String, String)> {
    let (cookie, csrf) = c.login().await;
    vec![
        ("Cookie".to_owned(), cookie),
        ("X-CSRF-Token".to_owned(), csrf),
    ]
}

async fn post(c: &BoardClient, h: &[(String, String)], method: &str, body: Value) -> Http {
    let h: Vec<(&str, &str)> = h.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    c.post(&format!("/api/commands/{method}"), &h, body).await
}

async fn token(c: &BoardClient, name: &str, scopes: &[Scope]) -> Vec<(String, String)> {
    let (_, t) = c
        .store
        .create_token(
            name.into(),
            scopes.iter().copied().collect::<BTreeSet<_>>(),
            DAY,
        )
        .await
        .unwrap();
    vec![("Authorization".to_owned(), format!("Bearer {}", t.expose()))]
}

#[tokio::test]
async fn a_command_reaches_the_station_once_and_a_resend_returns_the_stored_command() {
    let mut f = fixture().await;
    let mut station = f.connect().await;
    let c = board(std::sync::Arc::clone(&f.ctx)).await;
    let h = owner(&c).await;

    let call = post(&c, &h, "session.interrupt", interrupt_body("c1", "s1"));
    let (r, req) = tokio::join!(call, Fixture::answer(&mut station, accepted));
    assert_eq!(req.method, "session.interrupt");
    assert_eq!(req.command_id, Some(CommandId::from("c1")));
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body["state"], "done");
    assert_eq!(r.body["receipt"]["state"], "accepted");

    // The board resends after a lost response: the stored command, and
    // nothing goes to the station.
    let again = post(&c, &h, "session.interrupt", interrupt_body("c1", "s1")).await;
    assert_eq!(again.status, 200);
    assert_eq!(again.body, r.body);
    no_request(&mut station, Duration::from_millis(300)).await;
    let polled = c
        .get("/api/commands/c1", &[(h[0].0.as_str(), h[0].1.as_str())])
        .await;
    assert_eq!(polled.body, r.body);

    // The same id for another command: refused.
    let other = post(&c, &h, "session.interrupt", interrupt_body("c1", "s2")).await;
    assert_eq!((other.status, other.code()), (409, "command_id_reused"));

    let ok = c.audited("session.interrupt", AuditResult::Ok).await;
    assert_eq!(ok, vec![Some("duplicate".to_owned()), None]);
    let rows = f.store.audit_log(10).await.unwrap();
    let first = rows
        .iter()
        .find(|e| e.action == "session.interrupt")
        .unwrap();
    assert_eq!(first.actor, "owner");
    assert_eq!(first.target.as_deref(), Some(f.station.0.as_str()));
    assert_eq!(f.store.commands().await.unwrap().len(), 1);
}

#[tokio::test]
async fn bad_requests_are_refused_before_anything_is_stored_and_are_audited() {
    let mut f = fixture().await;
    let mut station = f.connect().await;
    let c = board(std::sync::Arc::clone(&f.ctx)).await;
    let h = owner(&c).await;

    let cases = [
        (
            "session.stop",
            interrupt_body("b1", "s1"),
            400,
            "wrong_method",
        ),
        (
            "session.interrupt",
            interrupt_body("a b", "s1"),
            400,
            "invalid_command_id",
        ),
        (
            "session.interrupt",
            json!({ "station": "nope", "command_id": "b3",
                    "command": { "op": "interrupt", "session_id": "s1" } }),
            404,
            "unknown_station",
        ),
        (
            "session.interrupt",
            json!({ "station": "fake", "command_id": "b4", "extra": 1,
                    "command": { "op": "interrupt", "session_id": "s1" } }),
            400,
            "invalid_body",
        ),
        ("session.nope", interrupt_body("b5", "s1"), 404, "not_found"),
    ];
    for (method, body, status, code) in cases {
        let r = post(&c, &h, method, body).await;
        assert_eq!((r.status, r.code()), (status, code), "{method}: {}", r.body);
    }
    // A cookie login without the CSRF token.
    let r = post(&c, &h[..1], "session.interrupt", interrupt_body("b6", "s1")).await;
    assert_eq!((r.status, r.code()), (403, "csrf"));

    no_request(&mut station, Duration::from_millis(300)).await;
    assert!(f.store.commands().await.unwrap().is_empty());
    let refused = c.audited("session.interrupt", AuditResult::Refused).await;
    assert_eq!(
        refused,
        vec![
            Some("csrf".to_owned()),
            Some("invalid_body".to_owned()),
            Some("unknown_station".to_owned()),
            Some("invalid_command_id".to_owned()),
        ]
    );
}

#[tokio::test]
async fn each_command_method_needs_its_own_scope() {
    let mut f = fixture().await;
    let mut station = f.connect().await;
    let c = board(std::sync::Arc::clone(&f.ctx)).await;
    let read = token(&c, "viewer", &[Scope::Read]).await;
    let drive = token(&c, "driver", &[Scope::Read, Scope::Drive]).await;
    let answer = json!({
        "station": "fake",
        "command_id": "s2",
        "command": {
            "op": "answer",
            "session_id": "s1",
            "request_id": "r1",
            "decision": ApprovalDecision {
                option_id: "allow".to_owned(),
                updated_input: None,
                text: None,
            },
        },
    });

    let r = post(&c, &read, "session.interrupt", interrupt_body("s1", "s1")).await;
    assert_eq!((r.status, r.code()), (403, "forbidden"));
    let r = post(&c, &drive, "approval.answer", answer.clone()).await;
    assert_eq!((r.status, r.code()), (403, "forbidden"));
    no_request(&mut station, Duration::from_millis(300)).await;

    // A bearer client needs no CSRF token; `drive` routes an interrupt.
    let call = post(&c, &drive, "session.interrupt", interrupt_body("s3", "s1"));
    let (r, _) = tokio::join!(
        call,
        Fixture::answer(&mut station, |id| rejected(id, RejectCode::UnknownSession))
    );
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body["receipt"]["state"], "rejected");
    let approve = token(&c, "approver", &[Scope::Approve]).await;
    let call = post(&c, &approve, "approval.answer", answer);
    let (r, req) = tokio::join!(call, Fixture::answer(&mut station, accepted));
    assert_eq!(req.method, "approval.answer");
    assert_eq!(r.body["state"], "done");
    let rows = f.store.audit_log(20).await.unwrap();
    assert!(
        rows.iter()
            .any(|e| e.actor == "token:approver" && e.action == "approval.answer")
    );
}

#[tokio::test]
async fn a_command_for_an_unreachable_station_waits_pending_and_goes_out_once_on_reconnect() {
    let mut f = fixture().await;
    let c = board(std::sync::Arc::clone(&f.ctx)).await;
    let h = owner(&c).await;
    // The deck has never synced the station: no incarnation to target.
    let r = post(&c, &h, "session.interrupt", interrupt_body("p0", "s1")).await;
    assert_eq!((r.status, r.code()), (409, "not_synced"));

    let station = f.connect().await;
    f.drop_link(station).await;
    let r = post(&c, &h, "session.interrupt", interrupt_body("p1", "s1")).await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body["state"], "pending");
    // A resend while the station is away stores nothing new.
    let r = post(&c, &h, "session.interrupt", interrupt_body("p1", "s1")).await;
    assert_eq!(r.body["state"], "pending");

    let mut station = f.connect().await;
    let req = Fixture::answer(&mut station, accepted).await;
    assert_eq!(req.command_id, Some(CommandId::from("p1")));
    let cmd = f.settled(&CommandId::from("p1")).await;
    assert_eq!(cmd.state, DeckCommandState::Done);
    assert_eq!(cmd.receipt.unwrap().state, ReceiptState::Accepted);
    no_request(&mut station, Duration::from_millis(500)).await;
    assert_eq!(f.store.commands().await.unwrap().len(), 1);
}
