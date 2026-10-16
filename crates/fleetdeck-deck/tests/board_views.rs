//! The views at many sessions (R6, DL-169): the read cursor
//! (`POST /api/sessions/{station}/{session}/read`) and the unread count of
//! the card, and the activity stream (`GET /api/activity`).

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::board::board;
use common::*;
use fleetdeck_deck::store::{NewMail, NewOutboxItem};
use fleetdeck_proto::event::{Event, EventBody, ProviderRefs, RunOrigin, ToolKind, ToolStatus};
use fleetdeck_proto::orchestration::MailDelivery;
use fleetdeck_proto::session::RepoInfo;
use fleetdeck_proto::{EventEnvelope, SessionId, SessionSummary};
use serde_json::json;

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

fn message(seq: u64, session: &str, at: i64, text: &str) -> EventEnvelope {
    env(
        seq,
        session,
        at,
        Event::MessageCompleted {
            turn_id: "t".into(),
            parent_node_id: None,
            text: text.to_owned(),
        },
    )
}

fn summary(id: &str, title: &str, origin: &str) -> SessionSummary {
    let mut s = SessionSummary::new(id.into());
    s.title = Some(title.to_owned());
    s.repo = Some(RepoInfo {
        root: format!("/work/{origin}"),
        origin: Some(origin.to_owned()),
    });
    s
}

async fn seed(f: &Fixture) {
    let s = &f.store;
    s.apply_snapshot(
        f.station.clone(),
        f.inc.clone(),
        0,
        vec![
            summary("s1", "Login fix", "github.com/acme/web"),
            summary("s2", "Docs", "github.com/acme/docs"),
        ],
    )
    .await
    .unwrap();
    s.apply_events(
        f.station.clone(),
        f.inc.clone(),
        vec![
            env(
                1,
                "s1",
                1_000,
                Event::RunStarted {
                    run_id: "r1".into(),
                    input_summary: "go".to_owned(),
                    origin: RunOrigin::Board,
                    author: None,
                    input_id: None,
                    input_text: None,
                },
            ),
            env(
                2,
                "s1",
                2_000,
                Event::ToolStarted {
                    node_id: "n1".into(),
                    parent_node_id: None,
                    tool_kind: ToolKind::Shell,
                    title: "Bash".to_owned(),
                    input_summary: "cargo test".to_owned(),
                    files: Vec::new(),
                    provider_refs: ProviderRefs::new(),
                },
            ),
            env(
                3,
                "s1",
                3_000,
                Event::ToolCompleted {
                    node_id: "n1".into(),
                    status: ToolStatus::Ok,
                    files: Vec::new(),
                    exit_code: Some(0),
                    provider_refs: serde_json::Map::new(),
                },
            ),
            message(4, "s1", 4_000, "Fixed the login test.\nSecond line."),
            message(5, "s2", 5_000, "Docs are ready"),
            message(6, "s1", 6_000, "One more thing"),
        ],
    )
    .await
    .unwrap();
    s.send_mail(NewMail {
        mail_id: "m1".to_owned(),
        sender: "owner".to_owned(),
        session_id: SessionId::from("s2".to_owned()),
        delivery: MailDelivery::FollowUp,
        text: "please add a changelog".to_owned(),
        item: NewOutboxItem {
            station_id: f.station.clone(),
            origin: "mail".to_owned(),
            payload: json!({ "kind": "session" }),
            expires_at: i64::MAX,
        },
    })
    .await
    .unwrap();
}

fn unread_of(body: &serde_json::Value, session: &str) -> u64 {
    body.as_array()
        .unwrap()
        .iter()
        .find(|c| c["summary"]["session_id"] == session)
        .unwrap()["unread"]
        .as_u64()
        .unwrap_or(0)
}

#[tokio::test]
async fn the_read_cursor_lowers_the_unread_count_of_the_caller() {
    let f = fixture().await;
    let c = board(std::sync::Arc::clone(&f.ctx)).await;
    seed(&f).await;
    let (cookie, csrf) = c.login().await;
    let h = [("Cookie", cookie.as_str()), ("X-CSRF-Token", csrf.as_str())];

    let r = c.get("/api/sessions", &h[..1]).await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!((unread_of(&r.body, "s1"), unread_of(&r.body, "s2")), (2, 1));

    let path = format!("/api/sessions/{}/s1/read", f.station.as_str());
    let r = c.post(&path, &h, json!({ "seq": 4 })).await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body, json!({ "seq": 4 }));
    let r = c.get("/api/sessions", &h[..1]).await;
    assert_eq!((unread_of(&r.body, "s1"), unread_of(&r.body, "s2")), (1, 1));

    // The open socket gets the changed card in its next delta.
    let mut ws = c
        .ws(
            "/api/ws",
            &[("Cookie", cookie.as_str()), ("Origin", c.origin().as_str())],
        )
        .await
        .unwrap();
    let snap = ws.next_of("snapshot").await;
    let card = snap["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["summary"]["session_id"] == "s1")
        .unwrap();
    assert_eq!(card["unread"], 1);
    let r = c.post(&path, &h, json!({ "seq": 6 })).await;
    assert_eq!(r.status, 200, "{}", r.body);
    let delta = ws.next_of("delta").await;
    let card = delta["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["summary"]["session_id"] == "s1")
        .unwrap();
    assert!(card.get("unread").is_none_or(|u| u == 0), "{card}");
}

#[tokio::test]
async fn the_read_route_checks_its_input_and_the_login() {
    let f = fixture().await;
    let c = board(std::sync::Arc::clone(&f.ctx)).await;
    seed(&f).await;
    let (cookie, csrf) = c.login().await;
    let h = [("Cookie", cookie.as_str()), ("X-CSRF-Token", csrf.as_str())];
    let st = f.station.as_str();

    let r = c
        .post(
            &format!("/api/sessions/{st}/nope/read"),
            &h,
            json!({ "seq": 1 }),
        )
        .await;
    assert_eq!((r.status, r.code()), (404, "unknown_session"), "{}", r.body);
    let r = c
        .post("/api/sessions/nowhere/s1/read", &h, json!({ "seq": 1 }))
        .await;
    assert_eq!((r.status, r.code()), (404, "unknown_station"), "{}", r.body);
    for bad in [
        json!({}),
        json!({ "seq": -1 }),
        json!({ "seq": 1, "x": 2 }),
        json!({ "seq": "a" }),
    ] {
        let r = c
            .post(&format!("/api/sessions/{st}/s1/read"), &h, bad)
            .await;
        assert_eq!((r.status, r.code()), (400, "invalid_body"), "{}", r.body);
    }
    // Without the CSRF token and without a login the route is closed.
    let r = c
        .post(
            &format!("/api/sessions/{st}/s1/read"),
            &h[..1],
            json!({ "seq": 1 }),
        )
        .await;
    assert_eq!(r.status, 403, "{}", r.body);
    let r = c
        .post(
            &format!("/api/sessions/{st}/s1/read"),
            &[],
            json!({ "seq": 1 }),
        )
        .await;
    assert_eq!(r.status, 401, "{}", r.body);
}

#[tokio::test]
async fn the_activity_stream_is_newest_first_with_filters_and_a_limit() {
    let f = fixture().await;
    let c = board(std::sync::Arc::clone(&f.ctx)).await;
    seed(&f).await;
    let (cookie, _) = c.login().await;
    let h = [("Cookie", cookie.as_str())];

    let r = c.get("/api/activity", &h).await;
    assert_eq!(r.status, 200, "{}", r.body);
    let items = r.body["items"].as_array().unwrap();
    let texts: Vec<&str> = items.iter().map(|i| i["text"].as_str().unwrap()).collect();
    // Newest first; the run start is not shown; the tool call carries its title.
    assert_eq!(items.len(), 5, "{texts:?}");
    assert_eq!(texts[0], "Mail: please add a changelog");
    assert_eq!(items[0]["kind"], "mail");
    assert_eq!(items[0]["station_seq"], serde_json::Value::Null);
    assert_eq!(texts[1], "One more thing");
    assert_eq!(texts[2], "Docs are ready");
    assert_eq!(texts[3], "Fixed the login test.");
    assert_eq!(texts[4], "Bash: cargo test (ok)");
    assert_eq!(items[4]["kind"], "tool");
    assert_eq!(items[1]["session_title"], "Login fix");
    assert_eq!(items[1]["repo"], "github.com/acme/web");

    let r = c.get("/api/activity?session=s2", &h).await;
    let texts: Vec<&str> = r.body["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["text"].as_str().unwrap())
        .collect();
    assert_eq!(texts, ["Mail: please add a changelog", "Docs are ready"]);

    let r = c.get("/api/activity?repo=github.com/acme/web", &h).await;
    assert_eq!(r.body["items"].as_array().unwrap().len(), 3, "{}", r.body);

    let r = c.get("/api/activity?since=3500&limit=2", &h).await;
    let items = r.body["items"].as_array().unwrap();
    assert_eq!(items.len(), 2, "{}", r.body);
    assert!(items.iter().all(|i| i["at"].as_i64().unwrap() > 3_500));

    for bad in ["limit=0", "limit=501", "limit=x"] {
        let r = c.get(&format!("/api/activity?{bad}"), &h).await;
        assert_eq!(r.status, 400, "{bad}: {}", r.body);
    }
    assert_eq!(c.get("/api/activity", &[]).await.status, 401);
}
