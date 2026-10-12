//! `assign auto` over loopback HTTPS (M3 6-3, PLAN §7.3, DL-204): the
//! deck sends the task to the best connected session as a framed prompt,
//! a resend returns the first answer and sends nothing new, a task that no
//! session fits is `unrouted`, the method needs the drive scope, and every
//! call is audited.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::BTreeSet;
use std::time::Duration;

use common::board::{BoardClient, Http, board};
use common::*;
use fleetdeck_deck::store::AuditResult;
use fleetdeck_proto::SessionSummary;
use fleetdeck_proto::event::{ProviderKind, SessionState};
use fleetdeck_proto::link::Scope;
use fleetdeck_proto::session::Activity;
use serde_json::{Value, json};

const DAY: i64 = 24 * 60 * 60 * 1000;

fn card(id: &str, cwd: &str, activity: Activity, callsign: &str) -> SessionSummary {
    let mut s = SessionSummary::new(id.into());
    s.state = SessionState::Running;
    s.activity = activity;
    s.cwd = Some(cwd.to_owned());
    s.provider = Some(ProviderKind::Claude);
    s.callsign = Some(callsign.to_owned());
    s
}

async fn owner(c: &BoardClient) -> Vec<(String, String)> {
    let (cookie, csrf) = c.login().await;
    vec![
        ("Cookie".to_owned(), cookie),
        ("X-CSRF-Token".to_owned(), csrf),
    ]
}

async fn assign(c: &BoardClient, h: &[(String, String)], body: Value) -> Http {
    let h: Vec<(&str, &str)> = h.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    c.post("/api/assign", &h, body).await
}

#[tokio::test]
async fn assign_auto_prompts_the_idle_session_of_the_repo_once() {
    let mut f = fixture().await;
    let mut station = f.connect().await;
    f.store
        .apply_snapshot(
            f.station.clone(),
            f.inc.clone(),
            0,
            vec![
                card("busy", "/p/app", Activity::Working, "falcon-busy"),
                card("idle", "/p/app", Activity::Idle, "otter-idle"),
                card("other", "/p/other", Activity::Idle, "raven-othr"),
                card("lib", "/p/lib", Activity::Working, "lynx-lib0"),
            ],
        )
        .await
        .unwrap();
    let c = board(std::sync::Arc::clone(&f.ctx)).await;
    let h = owner(&c).await;

    let body = json!({ "command_id": "as1", "text": "fix the login", "repo": "/p/app" });
    let call = assign(&c, &h, body.clone());
    let (r, req) = tokio::join!(call, Fixture::answer(&mut station, accepted));
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(req.method, "session.prompt");
    assert_eq!(req.params["command"]["session_id"], "idle");
    assert_eq!(
        req.params["command"]["input"]["text"],
        "[FLEETDECK ASSIGNMENT] fix the login"
    );
    assert_eq!(r.body["outcome"], "assigned");
    assert_eq!(r.body["session_id"], "idle");
    assert_eq!(r.body["callsign"], "otter-idle");
    assert_eq!(r.body["state"], "done");

    // A resend after a lost response: the same answer, nothing new.
    let again = assign(&c, &h, body).await;
    assert_eq!(again.body, r.body);
    no_request(&mut station, Duration::from_millis(300)).await;

    // No session works in the repository: unrouted, nothing sent. The fake
    // station never answered `station.info`, so no station can spawn.
    let r = assign(
        &c,
        &h,
        json!({ "command_id": "as2", "text": "x", "repo": "/p/nowhere" }),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body["outcome"], "unrouted");
    assert!(r.body["spawn_on"].is_null(), "{}", r.body);
    no_request(&mut station, Duration::from_millis(300)).await;

    // The only session of the repository is busy: a follow-up.
    let call = assign(
        &c,
        &h,
        json!({ "command_id": "as3", "text": "later", "repo": "/p/lib",
                "provider": "claude" }),
    );
    let (r, req) = tokio::join!(call, Fixture::answer(&mut station, accepted));
    assert_eq!(req.method, "session.follow_up");
    assert_eq!(r.body["callsign"], "lynx-lib0", "{}", r.body);
    let ok = c.audited("session.assign", AuditResult::Ok).await;
    assert_eq!(ok.len(), 3, "{ok:?}");
}

#[tokio::test]
async fn assign_auto_refuses_bad_requests_and_needs_the_drive_scope() {
    let mut f = fixture().await;
    let _station = f.connect().await;
    let c = board(std::sync::Arc::clone(&f.ctx)).await;
    let h = owner(&c).await;
    let cases = [
        (
            json!({ "command_id": "a b", "text": "x" }),
            400,
            "invalid_command_id",
        ),
        (
            json!({ "command_id": "b2", "text": "  " }),
            400,
            "invalid_text",
        ),
        (
            json!({ "command_id": "b3", "text": "x", "extra": 1 }),
            400,
            "invalid_body",
        ),
    ];
    for (body, status, code) in cases {
        let r = assign(&c, &h, body).await;
        assert_eq!((r.status, r.code()), (status, code), "{}", r.body);
    }
    let refused = c.audited("session.assign", AuditResult::Refused).await;
    assert_eq!(refused.len(), 2, "{refused:?}");

    let (_, t) = c
        .store
        .create_token("viewer".into(), BTreeSet::from([Scope::Read]), DAY)
        .await
        .unwrap();
    let bearer = vec![("Authorization".to_owned(), format!("Bearer {}", t.expose()))];
    let r = assign(&c, &bearer, json!({ "command_id": "b4", "text": "x" })).await;
    assert_eq!(r.status, 403, "{}", r.body);
}
