//! Fleet mail (M3 6-2, DL-205) and tasks (6-4, DL-206) over loopback
//! HTTPS: the deck stores the intent once, the outbox delivers the framed
//! text to the target session as a native follow-up or steer, a resend
//! sends nothing new, a task completes once and its waiter sees the end,
//! and the methods need the drive scope.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::BTreeSet;
use std::time::Duration;

use common::board::{BoardClient, Http, board};
use common::*;
use fleetdeck_deck::store::AuditResult;
use fleetdeck_proto::event::{Event, EventBody, ProviderKind, SessionState};
use fleetdeck_proto::link::Scope;
use fleetdeck_proto::session::Activity;
use fleetdeck_proto::{EventEnvelope, SessionSummary};
use serde_json::{Value, json};

fn card(id: &str) -> SessionSummary {
    let mut s = SessionSummary::new(id.into());
    s.state = SessionState::Running;
    s.cwd = Some("/p/app".to_owned());
    s.provider = Some(ProviderKind::Claude);
    s
}

fn idle_card(id: &str, provider: ProviderKind) -> SessionSummary {
    let mut s = card(id);
    s.provider = Some(provider);
    s.activity = Activity::Idle;
    s
}

async fn owner(c: &BoardClient) -> Vec<(String, String)> {
    let (cookie, csrf) = c.login().await;
    vec![
        ("Cookie".to_owned(), cookie),
        ("X-CSRF-Token".to_owned(), csrf),
    ]
}

async fn post(c: &BoardClient, h: &[(String, String)], path: &str, body: Value) -> Http {
    let h: Vec<(&str, &str)> = h.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    c.post(path, &h, body).await
}

async fn get(c: &BoardClient, h: &[(String, String)], path: &str) -> Http {
    let h: Vec<(&str, &str)> = h.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    c.get(path, &h).await
}

async fn until(c: &BoardClient, h: &[(String, String)], path: &str, key: &str, want: &str) {
    let deadline = tokio::time::Instant::now() + T;
    loop {
        let r = get(c, h, path).await;
        let rows = r.body.as_array().cloned().unwrap_or_default();
        if !rows.is_empty() && rows.iter().all(|m| m[key] == want) {
            return;
        }
        assert!(tokio::time::Instant::now() < deadline, "{}", r.body);
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
}

#[tokio::test]
async fn a_mail_is_delivered_once_as_a_framed_follow_up_or_steer() {
    let mut f = fixture().await;
    let mut station = f.connect().await;
    f.store
        .apply_snapshot(f.station.clone(), f.inc.clone(), 0, vec![card("s1")])
        .await
        .unwrap();
    let c = board(std::sync::Arc::clone(&f.ctx)).await;
    let h = owner(&c).await;
    let st = f.station.0.clone();

    let body = json!({ "mail_id": "m1", "station": st, "session_id": "s1",
                       "text": "  please rebase  " });
    let r = post(&c, &h, "/api/mail", body.clone()).await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body["sender"], "owner");
    assert_eq!(r.body["delivery"], "follow_up");
    let req = Fixture::answer(&mut station, accepted).await;
    assert_eq!(req.method, "session.follow_up");
    assert_eq!(req.params["origin"], "mail");
    assert_eq!(req.params["command"]["session_id"], "s1");
    assert_eq!(
        req.params["command"]["input"]["text"],
        "[FLEETDECK MAIL from owner] please rebase"
    );
    until(&c, &h, "/api/mail", "state", "delivered").await;

    // A resend after a lost response stores and sends nothing new.
    let again = post(&c, &h, "/api/mail", body).await;
    assert_eq!(again.status, 200, "{}", again.body);
    assert_eq!(again.body["mail_id"], "m1");
    no_request(&mut station, Duration::from_millis(300)).await;

    let r = post(
        &c,
        &h,
        "/api/mail",
        json!({ "mail_id": "m2", "station": st, "session_id": "s1",
                "text": "stop now", "delivery": "steer" }),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    let req = Fixture::answer(&mut station, accepted).await;
    assert_eq!(req.method, "session.steer");

    // Unknown targets and bad bodies are refused and store nothing.
    for (body, status, code) in [
        (
            json!({ "mail_id": "m3", "station": st, "session_id": "nope", "text": "x" }),
            404,
            "unknown_session",
        ),
        (
            json!({ "mail_id": "m3", "station": "nope", "session_id": "s1", "text": "x" }),
            404,
            "unknown_station",
        ),
        (
            json!({ "mail_id": "m 3", "station": st, "session_id": "s1", "text": "x" }),
            400,
            "invalid_id",
        ),
        (
            json!({ "mail_id": "m3", "station": st, "session_id": "s1", "text": " " }),
            400,
            "invalid_text",
        ),
    ] {
        let r = post(&c, &h, "/api/mail", body).await;
        assert_eq!((r.status, r.code()), (status, code));
    }
    assert_eq!(
        get(&c, &h, "/api/mail")
            .await
            .body
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(c.audited("mail.send", AuditResult::Ok).await.len(), 3);
    assert_eq!(c.audited("mail.send", AuditResult::Refused).await.len(), 4);
}

#[tokio::test]
async fn a_task_is_delivered_completed_once_and_its_waiter_sees_the_end() {
    let mut f = fixture().await;
    let mut station = f.connect().await;
    f.store
        .apply_snapshot(f.station.clone(), f.inc.clone(), 0, vec![card("s1")])
        .await
        .unwrap();
    let c = board(std::sync::Arc::clone(&f.ctx)).await;
    let h = owner(&c).await;
    let st = f.station.0.clone();

    let r = post(
        &c,
        &h,
        "/api/tasks",
        json!({ "task_id": "t1", "title": "write the docs", "station": st,
                "session_id": "s1" }),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body["state"], "open");
    let req = Fixture::answer(&mut station, accepted).await;
    assert_eq!(req.method, "session.follow_up");
    let text = req.params["command"]["input"]["text"].as_str().unwrap();
    assert!(
        text.starts_with("[FLEETDECK TASK t1] write the docs"),
        "{text}"
    );
    until(&c, &h, "/api/tasks", "delivery", "delivered").await;

    // A short wait on an open task returns it open at the deadline.
    let r = get(&c, &h, "/api/tasks/t1/wait?timeout_ms=50").await;
    assert_eq!((r.status, r.body["state"].as_str()), (200, Some("open")));

    // A waiter sees the completion that arrives while it waits.
    let waiter = get(&c, &h, "/api/tasks/t1/wait?timeout_ms=10000");
    let complete = async {
        tokio::time::sleep(Duration::from_millis(200)).await;
        post(
            &c,
            &h,
            "/api/tasks/t1/complete",
            json!({ "state": "done", "result": "docs merged" }),
        )
        .await
    };
    let (w, done) = tokio::join!(waiter, complete);
    assert_eq!(done.status, 200, "{}", done.body);
    assert_eq!(w.body["state"], "done", "{}", w.body);
    assert_eq!(w.body["result"], "docs merged");

    // Another completion is a conflict; the task keeps its end.
    let r = post(
        &c,
        &h,
        "/api/tasks/t1/complete",
        json!({ "state": "failed" }),
    )
    .await;
    assert_eq!((r.status, r.code()), (409, "task_ended"));
    let r = post(&c, &h, "/api/tasks/t9/complete", json!({ "state": "done" })).await;
    assert_eq!((r.status, r.code()), (404, "unknown_task"));
    let r = post(&c, &h, "/api/tasks/t1/complete", json!({ "state": "open" })).await;
    assert_eq!((r.status, r.code()), (400, "invalid_state"));
    let r = get(&c, &h, "/api/tasks/t9/wait?timeout_ms=10").await;
    assert_eq!(r.status, 404);
    no_request(&mut station, Duration::from_millis(200)).await;
    assert_eq!(c.audited("tasks.create", AuditResult::Ok).await.len(), 1);
    assert_eq!(c.audited("tasks.complete", AuditResult::Ok).await.len(), 1);
}

#[tokio::test]
async fn a_group_mail_reaches_each_live_session_of_the_group_once() {
    let mut f = fixture().await;
    let mut station = f.connect().await;
    let cards = vec![
        idle_card("s1", ProviderKind::Claude),
        idle_card("s2", ProviderKind::Codex),
    ];
    f.store
        .apply_snapshot(f.station.clone(), f.inc.clone(), 0, cards)
        .await
        .unwrap();
    let c = board(std::sync::Arc::clone(&f.ctx)).await;
    let h = owner(&c).await;

    let body = json!({ "mail_id": "g1", "group": "@claude", "text": " pause " });
    let r = post(&c, &h, "/api/mail/group", body.clone()).await;
    assert_eq!(r.status, 200, "{}", r.body);
    let rows = r.body.as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(
        (&rows[0]["mail_id"], &rows[0]["session_id"]),
        (&json!("g1-1"), &json!("s1"))
    );
    let req = Fixture::answer(&mut station, accepted).await;
    assert_eq!(req.method, "session.follow_up");
    assert_eq!(req.params["command"]["session_id"], "s1");
    assert_eq!(
        req.params["command"]["input"]["text"],
        "[FLEETDECK MAIL from owner] pause"
    );
    // A resend stores and sends nothing new.
    let again = post(&c, &h, "/api/mail/group", body).await;
    assert_eq!(
        (again.status, again.body.as_array().map(Vec::len)),
        (200, Some(1))
    );
    no_request(&mut station, Duration::from_millis(300)).await;

    // `@idle` names both sessions; `steer` goes into the turn.
    let body = json!({ "mail_id": "g2", "group": "@idle", "text": "status?", "delivery": "steer" });
    let r = post(&c, &h, "/api/mail/group", body).await;
    assert_eq!(r.body.as_array().map(Vec::len), Some(2), "{}", r.body);
    for _ in 0..2 {
        let req = Fixture::answer(&mut station, accepted).await;
        assert_eq!(req.method, "session.steer");
    }
    // `@codex` names the one Codex session.
    let r = post(
        &c,
        &h,
        "/api/mail/group",
        json!({ "mail_id": "g3", "group": "@codex", "text": "x" }),
    )
    .await;
    assert_eq!(r.body.as_array().map(Vec::len), Some(1));
    Fixture::answer(&mut station, accepted).await;

    let long = "m".repeat(57);
    for (body, code) in [
        (
            json!({ "mail_id": "g4", "group": "@everyone", "text": "x" }),
            "invalid_body",
        ),
        (
            json!({ "mail_id": long, "group": "@idle", "text": "x" }),
            "invalid_id",
        ),
        (
            json!({ "mail_id": "g4", "group": "@idle", "text": " " }),
            "invalid_text",
        ),
    ] {
        let r = post(&c, &h, "/api/mail/group", body).await;
        assert_eq!((r.status, r.code()), (400, code));
    }
    assert_eq!(c.audited("mail.send_group", AuditResult::Ok).await.len(), 4);
    assert_eq!(
        c.audited("mail.send_group", AuditResult::Refused)
            .await
            .len(),
        3
    );
}

#[tokio::test]
async fn blocked_and_question_completions_wake_a_wait_over_several_tasks() {
    let mut f = fixture().await;
    let mut station = f.connect().await;
    f.store
        .apply_snapshot(f.station.clone(), f.inc.clone(), 0, vec![card("s1")])
        .await
        .unwrap();
    let c = board(std::sync::Arc::clone(&f.ctx)).await;
    let h = owner(&c).await;
    let st = f.station.0.clone();
    for id in ["t1", "t2"] {
        let body = json!({ "task_id": id, "title": "work", "station": st, "session_id": "s1" });
        assert_eq!(post(&c, &h, "/api/tasks", body).await.status, 200);
        Fixture::answer(&mut station, accepted).await;
    }

    // All open: the wait ends at its deadline with no task.
    let r = get(&c, &h, "/api/tasks/wait?ids=t1,t2&timeout_ms=50").await;
    assert_eq!(
        (r.status, r.body["task"].is_null()),
        (200, true),
        "{}",
        r.body
    );
    for (path, code) in [
        ("/api/tasks/wait?timeout_ms=10", "invalid_ids"),
        ("/api/tasks/wait?ids=t1,,t2", "invalid_ids"),
    ] {
        let r = get(&c, &h, path).await;
        assert_eq!((r.status, r.code()), (400, code), "{path}");
    }
    let r = get(&c, &h, "/api/tasks/wait?ids=t1,nope&timeout_ms=10").await;
    assert_eq!((r.status, r.code()), (404, "unknown_task"));

    // A waiter sees the question that arrives while it waits.
    let waiter = get(&c, &h, "/api/tasks/wait?ids=t1,t2&timeout_ms=10000");
    let ask = async {
        tokio::time::sleep(Duration::from_millis(200)).await;
        post(
            &c,
            &h,
            "/api/tasks/t2/complete",
            json!({ "state": "question", "reason": "stuck", "question": "which branch?" }),
        )
        .await
    };
    let (w, asked) = tokio::join!(waiter, ask);
    assert_eq!(asked.status, 200, "{}", asked.body);
    assert_eq!(w.body["task"]["task_id"], "t2", "{}", w.body);
    assert_eq!(w.body["task"]["state"], "question");
    assert_eq!(w.body["task"]["question"], "which branch?");
    assert_eq!(w.body["task"]["completion_reason"], "stuck");

    // The question does not end the task: it can end as blocked, once.
    let r = post(
        &c,
        &h,
        "/api/tasks/t2/complete",
        json!({ "state": "blocked", "reason": "no access" }),
    )
    .await;
    assert_eq!((r.status, r.body["state"].as_str()), (200, Some("blocked")));
    let r = post(&c, &h, "/api/tasks/t2/complete", json!({ "state": "done" })).await;
    assert_eq!((r.status, r.code()), (409, "task_ended"));
    for body in [
        json!({ "state": "blocked" }),
        json!({ "state": "question", "reason": "r" }),
        json!({ "state": "done", "reason": "r" }),
    ] {
        let r = post(&c, &h, "/api/tasks/t1/complete", body).await;
        assert_eq!((r.status, r.code()), (400, "invalid_state"));
    }

    // The worker of `t1` exits without a completion: the deck ends `t1`.
    let exited = EventEnvelope {
        station_seq: 1,
        session_id: "s1".into(),
        session_seq: 1,
        at: 1_000,
        proto_version: 1,
        raw_ref: None,
        event: EventBody::Known(Event::SessionStateChanged {
            state: SessionState::Exited,
            detail: None,
        }),
    };
    f.store
        .apply_events(f.station.clone(), f.inc.clone(), vec![exited])
        .await
        .unwrap();
    let r = get(&c, &h, "/api/tasks/t1/wait?timeout_ms=10").await;
    assert_eq!(r.body["state"], "blocked", "{}", r.body);
    assert_eq!(
        r.body["completion_reason"],
        "the worker session ended without a completion (exited)"
    );
}

#[tokio::test]
async fn mail_and_tasks_need_the_drive_scope() {
    let mut f = fixture().await;
    let _station = f.connect().await;
    let st = f.station.0.clone();
    let c = board(std::sync::Arc::clone(&f.ctx)).await;
    let (_, t) = c
        .store
        .create_token(
            "viewer".into(),
            BTreeSet::from([Scope::Read]),
            24 * 60 * 60 * 1000,
        )
        .await
        .unwrap();
    let h = vec![("Authorization".to_owned(), format!("Bearer {}", t.expose()))];
    let body = json!({ "mail_id": "m1", "station": st, "session_id": "s1", "text": "x" });
    assert_eq!(post(&c, &h, "/api/mail", body).await.status, 403);
    let body = json!({ "task_id": "t1", "title": "x", "station": st, "session_id": "s1" });
    assert_eq!(post(&c, &h, "/api/tasks", body).await.status, 403);
    assert_eq!(get(&c, &h, "/api/tasks").await.status, 200);
    assert_eq!(get(&c, &h, "/api/mail").await.status, 200);
}

#[tokio::test]
async fn the_owner_lists_and_revokes_a_coordinator_grant_and_a_viewer_cannot_revoke() {
    let mut f = fixture().await;
    let _station = f.connect().await;
    let c = board(std::sync::Arc::clone(&f.ctx)).await;
    let g = c
        .store
        .create_grant(fleetdeck_deck::store::NewGrant::new(
            fleetdeck_proto::StationId::from(f.station.0.clone()),
            "coord-1".into(),
        ))
        .await
        .unwrap();
    let h = owner(&c).await;
    let r = get(&c, &h, "/api/coordinator/grants").await;
    assert_eq!(r.status, 200);
    assert_eq!(r.body[0]["grant_id"], g.grant_id.as_str());
    assert_eq!(r.body[0]["classes"], json!(["read", "mail"]));
    assert_eq!(r.body[0]["state"], "active");
    assert!(r.body[0]["generation"].is_null());
    // No credential or digest in the view.
    assert!(!r.body.to_string().contains("digest"));
    let (_, t) = c
        .store
        .create_token(
            "viewer".into(),
            BTreeSet::from([Scope::Read]),
            24 * 60 * 60 * 1000,
        )
        .await
        .unwrap();
    let viewer = vec![("Authorization".to_owned(), format!("Bearer {}", t.expose()))];
    // Renewal (ADR-006 test 25b): a viewer is refused, an active grant is
    // not renewable, an unknown grant is 404.
    let renew = format!("/api/coordinator/grants/{}/renew", g.grant_id);
    assert_eq!(post(&c, &viewer, &renew, json!({})).await.status, 403);
    let r = post(&c, &h, &renew, json!({})).await;
    assert_eq!(r.status, 409, "{}", r.body);
    assert_eq!(r.body["error"], "not_renewable", "{}", r.body);
    let r = post(&c, &h, "/api/coordinator/grants/nope/renew", json!({})).await;
    assert_eq!(r.status, 404);
    let path = format!("/api/coordinator/grants/{}/revoke", g.grant_id);
    assert_eq!(post(&c, &viewer, &path, json!({})).await.status, 403);
    let r = post(&c, &h, &path, json!({})).await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body["state"], "revoked");
    assert_eq!(r.body["revoked_reason"], "owner");
    // A second revoke keeps it revoked; an unknown grant is 404.
    assert_eq!(
        post(&c, &h, &path, json!({})).await.body["state"],
        "revoked"
    );
    let r = post(&c, &h, "/api/coordinator/grants/nope/revoke", json!({})).await;
    assert_eq!(r.status, 404);
    assert!(
        !c.audited("coordinator.revoke", AuditResult::Ok)
            .await
            .is_empty()
    );
}

/// DL-137: revoke and renew are owner actions (ADR-006 §3). A bearer token
/// is refused with `owner_only` whatever its scopes, and the grant stays as
/// it was.
#[tokio::test]
async fn a_bearer_token_cannot_revoke_or_renew_a_coordinator_grant() {
    let mut f = fixture().await;
    let _station = f.connect().await;
    let c = board(std::sync::Arc::clone(&f.ctx)).await;
    let g = c
        .store
        .create_grant(fleetdeck_deck::store::NewGrant::new(
            fleetdeck_proto::StationId::from(f.station.0.clone()),
            "coord-1".into(),
        ))
        .await
        .unwrap();
    let scopes = [
        BTreeSet::from([Scope::Drive]),
        BTreeSet::from([Scope::Read, Scope::Drive]),
        BTreeSet::from([Scope::Read, Scope::Drive, Scope::Approve, Scope::Admin]),
    ];
    for (n, set) in scopes.into_iter().enumerate() {
        let (_, t) = c
            .store
            .create_token(format!("t{n}"), set, 24 * 60 * 60 * 1000)
            .await
            .unwrap();
        let h = vec![("Authorization".to_owned(), format!("Bearer {}", t.expose()))];
        for action in ["revoke", "renew"] {
            let path = format!("/api/coordinator/grants/{}/{action}", g.grant_id);
            let r = post(&c, &h, &path, json!({})).await;
            assert_eq!(r.status, 403, "{action} with token {n}: {}", r.body);
            assert_eq!(r.body["error"], "owner_only", "{}", r.body);
        }
    }
    let after = c.store.grant(g.grant_id.clone()).await.unwrap().unwrap();
    assert_eq!(after.state, g.state);
    assert_eq!(after.revoked_reason, g.revoked_reason);
    assert!(
        c.audited("coordinator.revoke", AuditResult::Ok)
            .await
            .is_empty()
    );
    // The owner still revokes it.
    let h = owner(&c).await;
    let path = format!("/api/coordinator/grants/{}/revoke", g.grant_id);
    let r = post(&c, &h, &path, json!({})).await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body["state"], "revoked");
}
