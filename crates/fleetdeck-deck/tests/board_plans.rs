//! The plan library (M3 6-6, DL-301): a folded `plan.proposed` event
//! becomes one library row; a replay or a new incarnation keeps one row
//! and the owner's status; `GET /api/plans` needs `read`, `POST
//! /api/plans/mark` needs `drive` and the CSRF token and is audited.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::BTreeSet;

use common::board::{BoardClient, board};
use common::*;
use fleetdeck_deck::store::AuditResult;
use fleetdeck_proto::event::{Event, EventBody};
use fleetdeck_proto::link::Scope;
use fleetdeck_proto::plans::PlanStatus;
use fleetdeck_proto::{EventEnvelope, IncarnationId, SessionSummary};
use serde_json::json;

const DAY: i64 = 24 * 60 * 60 * 1000;

fn env(seq: u64, session: &str, at: i64, event: Event) -> EventEnvelope {
    EventEnvelope {
        station_seq: seq,
        session_id: session.into(),
        session_seq: seq,
        at,
        proto_version: 1,
        raw_ref: None,
        event: EventBody::Known(event),
    }
}

fn plan(seq: u64, session: &str, id: &str, text: &str) -> EventEnvelope {
    env(
        seq,
        session,
        1_000 + i64::try_from(seq).unwrap(),
        Event::PlanProposed {
            plan_id: id.into(),
            turn_id: Some("t1".into()),
            markdown: text.to_owned(),
        },
    )
}

async fn bearer(c: &BoardClient, name: &str, scopes: &[Scope]) -> (String, String) {
    let (_, t) = c
        .store
        .create_token(
            name.into(),
            scopes.iter().copied().collect::<BTreeSet<_>>(),
            DAY,
        )
        .await
        .unwrap();
    ("Authorization".to_owned(), format!("Bearer {}", t.expose()))
}

#[tokio::test]
async fn a_proposed_plan_enters_the_library_once_and_keeps_the_owner_status() {
    let f = fixture().await;
    let c = board(std::sync::Arc::clone(&f.ctx)).await;
    let s = &f.store;
    s.apply_snapshot(
        f.station.clone(),
        f.inc.clone(),
        0,
        vec![SessionSummary::new("s1".into())],
    )
    .await
    .unwrap();
    s.apply_events(
        f.station.clone(),
        f.inc.clone(),
        vec![
            plan(1, "s1", "p1", "# First\n1. a"),
            plan(2, "s1", "p2", "# Second"),
        ],
    )
    .await
    .unwrap();
    // A replay of the same events adds nothing.
    s.apply_events(
        f.station.clone(),
        f.inc.clone(),
        vec![plan(2, "s1", "p2", "# Second")],
    )
    .await
    .unwrap();
    let list = s.plans(false).await.unwrap();
    assert_eq!(
        list.iter().map(|p| p.plan_id.as_str()).collect::<Vec<_>>(),
        ["p2", "p1"],
        "newest first"
    );
    assert_eq!(list[1].markdown, "# First\n1. a");
    assert_eq!(list[1].status, PlanStatus::Proposed);
    assert_eq!(list[1].turn_id.as_ref().map(|t| t.as_str()), Some("t1"));

    // The owner archives p1 over HTTP (drive + CSRF), audited.
    let (cookie, csrf) = c.login().await;
    let h = [("Cookie", cookie.as_str()), ("X-CSRF-Token", csrf.as_str())];
    let id = list[1].id.clone();
    let r = c
        .post(
            "/api/plans/mark",
            &h,
            json!({"id": id, "status": "archived"}),
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body["status"], "archived");
    assert_eq!(
        c.audited("plans.mark", AuditResult::Ok).await,
        [Some("archived".to_owned())]
    );
    let r = c.get("/api/plans", &h[..1]).await;
    assert_eq!(r.status, 200);
    assert_eq!(r.body.as_array().unwrap().len(), 1, "archived is hidden");
    let r = c.get("/api/plans?archived=true", &h[..1]).await;
    assert_eq!(r.body.as_array().unwrap().len(), 2);

    // A new incarnation (station database replaced) keeps the library and
    // the status; the same plan id of the new incarnation stays one row.
    let inc2 = IncarnationId::from("inc-2");
    s.apply_snapshot(f.station.clone(), inc2.clone(), 0, vec![])
        .await
        .unwrap();
    s.apply_events(
        f.station.clone(),
        inc2,
        vec![plan(1, "s1", "p1", "# First again")],
    )
    .await
    .unwrap();
    let all = s.plans(true).await.unwrap();
    assert_eq!(all.len(), 2);
    let p1 = all.iter().find(|p| p.plan_id.as_str() == "p1").unwrap();
    assert_eq!(
        (p1.status, p1.markdown.as_str()),
        (PlanStatus::Archived, "# First\n1. a")
    );
}

#[tokio::test]
async fn plan_marks_need_drive_csrf_and_a_known_plan() {
    let f = fixture().await;
    let c = board(std::sync::Arc::clone(&f.ctx)).await;
    let read = bearer(&c, "reader", &[Scope::Read]).await;
    let read = [(read.0.as_str(), read.1.as_str())];
    assert_eq!(c.get("/api/plans", &read).await.status, 200);
    let r = c
        .post(
            "/api/plans/mark",
            &read,
            json!({"id": "x", "status": "archived"}),
        )
        .await;
    assert_eq!(r.status, 403, "{}", r.body);

    let drive = bearer(&c, "driver", &[Scope::Read, Scope::Drive]).await;
    let drive = [(drive.0.as_str(), drive.1.as_str())];
    let r = c
        .post(
            "/api/plans/mark",
            &drive,
            json!({"id": "st/s/p", "status": "executed"}),
        )
        .await;
    assert_eq!((r.status, r.code()), (404, "unknown_plan"), "{}", r.body);
    let r = c
        .post(
            "/api/plans/mark",
            &drive,
            json!({"id": "st/s/p", "status": "done"}),
        )
        .await;
    assert_eq!((r.status, r.code()), (400, "invalid_body"), "{}", r.body);

    // A cookie login without the CSRF token is refused.
    let (cookie, _) = c.login().await;
    let r = c
        .post(
            "/api/plans/mark",
            &[("Cookie", cookie.as_str())],
            json!({"id": "st/s/p", "status": "executed"}),
        )
        .await;
    assert_eq!(r.status, 403, "{}", r.body);
    assert_eq!(c.get("/api/plans", &[]).await.status, 401);
}
