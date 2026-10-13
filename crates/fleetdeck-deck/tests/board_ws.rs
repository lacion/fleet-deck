//! The board WebSocket over loopback HTTPS (M2 P3-9b; PLAN §9, §7.1, L15,
//! DL-056): a versioned snapshot then deltas, per-session timeline
//! subscriptions with a cursor and an incarnation reset, single-use
//! tickets for bearer clients, the Origin check for cookie sockets, and
//! the close when the login ends.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use common::board::{BoardClient, Ws, board};
use common::*;
use fleetdeck_deck::http::ws::CLOSE_LOGIN_ENDED;
use fleetdeck_proto::event::{Event, EventBody, RunOrigin};
use fleetdeck_proto::link::Scope;
use fleetdeck_proto::{EventEnvelope, IncarnationId, SessionSummary};
use serde_json::{Value, json};

const DAY: i64 = 24 * 60 * 60 * 1000;

fn env(seq: u64, session: &str, event: Event) -> EventEnvelope {
    EventEnvelope {
        station_seq: seq,
        session_id: session.into(),
        session_seq: seq,
        at: 1_000,
        proto_version: 1,
        raw_ref: None,
        event: EventBody::Known(event),
    }
}

fn run(seq: u64, session: &str) -> EventEnvelope {
    env(
        seq,
        session,
        Event::RunStarted {
            author: None,
            run_id: format!("r{seq}").into(),
            input_summary: "hi".to_owned(),
            origin: RunOrigin::Board,
        },
    )
}

fn delta(seq: u64, session: &str) -> EventEnvelope {
    env(
        seq,
        session,
        Event::MessageDelta {
            turn_id: "t".into(),
            parent_node_id: None,
            text: "x".to_owned(),
        },
    )
}

/// A deck whose fake station has synced session `s1` (incarnation
/// `f.inc`, watermark 0), and its board.
async fn setup() -> (Fixture, BoardClient) {
    let f = fixture().await;
    f.store
        .apply_snapshot(
            f.station.clone(),
            f.inc.clone(),
            0,
            vec![SessionSummary::new("s1".into())],
        )
        .await
        .unwrap();
    let c = board(Arc::clone(&f.ctx)).await;
    (f, c)
}

/// A cookie socket with the board's own Origin; returns it and the
/// (cookie, csrf) of its login.
async fn open(c: &BoardClient) -> (Ws, (String, String)) {
    let login = c.login().await;
    let origin = c.origin();
    let ws = c
        .ws("/api/ws", &[("Cookie", &login.0), ("Origin", &origin)])
        .await
        .expect("the socket opens");
    (ws, login)
}

fn seqs(m: &Value) -> Vec<u64> {
    m["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["station_seq"].as_u64().unwrap())
        .collect()
}

fn ids(list: &Value) -> Vec<String> {
    list.as_array()
        .unwrap()
        .iter()
        .map(|c| c["summary"]["session_id"].as_str().unwrap().to_owned())
        .collect()
}

#[tokio::test]
async fn a_socket_gets_a_versioned_snapshot_then_deltas_of_what_changed() {
    let (f, c) = setup().await;
    let (mut ws, _) = open(&c).await;
    let hello = ws.next().await.unwrap();
    assert_eq!(hello["type"], "hello");
    assert_eq!(hello["protocol"], 1);
    assert_eq!(
        hello["deck_incarnation"],
        json!(f.store.incarnation().clone())
    );
    let snap = ws.next().await.unwrap();
    assert_eq!(snap["type"], "snapshot");
    assert_eq!(ids(&snap["sessions"]), vec!["s1"]);
    assert_eq!(snap["sessions"][0]["status"], "unreachable");
    assert_eq!(snap["stations"][0]["name"], "fake");
    let v0 = snap["version"].as_u64().unwrap();

    // The station links: the station view and the card status change.
    f.store.set_reachable(&f.station, true);
    let d = ws.next_of("delta").await;
    assert!(d["version"].as_u64().unwrap() > v0);
    assert_eq!(d["stations"][0]["reachable"], true);
    assert_eq!(ids(&d["sessions"]), vec!["s1"]);
    assert_ne!(d["sessions"][0]["status"], "unreachable");

    // A new session: only that card is in the delta.
    f.store
        .apply_events(f.station.clone(), f.inc.clone(), vec![run(1, "s2")])
        .await
        .unwrap();
    let d = ws.next_of("delta").await;
    assert_eq!(ids(&d["sessions"]), vec!["s2"]);
    // The station view carries the read model's cursor (freshness).
    assert_eq!(d["stations"][0]["cursor"], 1);
    assert_eq!(d["removed"], json!([]));

    // Removing the station removes its cards.
    f.store
        .revoke_station(f.station_node.identity.public_key())
        .await
        .unwrap();
    let d = ws.next_of("delta").await;
    let removed: BTreeSet<String> = d["removed"]
        .as_array()
        .unwrap()
        .iter()
        .map(|k| k["session_id"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(removed, BTreeSet::from(["s1".to_owned(), "s2".to_owned()]));
    assert_eq!(d["stations"], json!([]));
}

#[tokio::test]
async fn a_timeline_subscription_resumes_from_its_cursor_and_resets_on_a_new_incarnation() {
    let (f, c) = setup().await;
    let (mut ws, _) = open(&c).await;
    ws.next_of("snapshot").await;
    let apply = |events: Vec<EventEnvelope>| {
        let (store, id, inc) = (f.store.clone(), f.station.clone(), f.inc.clone());
        async move { store.apply_events(id, inc, events).await.unwrap() }
    };
    apply(vec![run(1, "s1"), delta(2, "s1"), run(3, "s2")]).await;
    ws.next_of("delta").await;

    let sub = json!({ "type": "subscribe", "station_id": f.station, "session_id": "s1" });
    ws.send(sub.clone()).await;
    let start = ws.next_of("timeline_start").await;
    assert_eq!(
        (start["reset"].clone(), start["incarnation"].clone()),
        (json!(true), json!(f.inc))
    );
    // The stored timeline keeps no streaming deltas.
    assert_eq!(seqs(&ws.next_of("timeline_events").await), vec![1]);

    // Live: every event of the subscribed session, also the deltas; none
    // of another session.
    apply(vec![run(4, "s1"), delta(5, "s1"), run(6, "s2")]).await;
    let live = ws.next_of("timeline_events").await;
    assert_eq!(live["session_id"], "s1");
    assert_eq!(seqs(&live), vec![4, 5]);

    // A new socket resumes from the client's cursor: nothing is resent.
    drop(ws);
    let (mut ws, _) = open(&c).await;
    ws.next_of("snapshot").await;
    let resume = json!({ "type": "subscribe", "station_id": f.station, "session_id": "s1",
                          "incarnation": f.inc, "after": 5 });
    ws.send(resume).await;
    assert_eq!(ws.next_of("timeline_start").await["reset"], false);
    ws.quiet(Duration::from_millis(300)).await;
    // A cursor of another incarnation is not used.
    let stale = json!({ "type": "subscribe", "station_id": f.station, "session_id": "s1",
                         "incarnation": "old", "after": 5 });
    ws.send(stale).await;
    assert_eq!(ws.next_of("timeline_start").await["reset"], true);
    assert_eq!(seqs(&ws.next_of("timeline_events").await), vec![1, 4]);

    // The station database is replaced: the subscription restarts.
    let new_inc = IncarnationId::from("inc-2");
    f.store
        .apply_snapshot(
            f.station.clone(),
            new_inc.clone(),
            0,
            vec![SessionSummary::new("s1".into())],
        )
        .await
        .unwrap();
    let start = ws.next_of("timeline_start").await;
    assert_eq!(
        (start["reset"].clone(), start["incarnation"].clone()),
        (json!(true), json!(new_inc))
    );

    // After unsubscribe, no more events of the session.
    ws.send(json!({ "type": "unsubscribe", "station_id": f.station, "session_id": "s1" }))
        .await;
    // The socket handles client messages in order: an answered bad
    // message proves the unsubscribe was handled before the next event.
    ws.send(json!({ "type": "barrier" })).await;
    assert_eq!(ws.next_of("error").await["code"], "invalid_message");
    f.store
        .apply_events(f.station.clone(), new_inc, vec![run(1, "s1")])
        .await
        .unwrap();
    let d = ws.next_of("delta").await;
    assert_eq!(ids(&d["sessions"]), vec!["s1"]);
    ws.quiet(Duration::from_millis(300)).await;

    // Bad client messages get an error and keep the socket.
    ws.send(json!({ "type": "subscribe", "station_id": "nope", "session_id": "s1" }))
        .await;
    assert_eq!(ws.next_of("error").await["code"], "unknown_station");
    ws.send(json!({ "type": "shout" })).await;
    assert_eq!(ws.next_of("error").await["code"], "invalid_message");
    ws.send(json!({ "type": "unsubscribe", "station_id": f.station, "session_id": "s1", "x": 1 }))
        .await;
    assert_eq!(ws.next_of("error").await["code"], "invalid_message");
}

#[tokio::test]
async fn cookie_sockets_need_the_board_origin_and_bearer_clients_need_a_single_use_ticket() {
    let (f, c) = setup().await;
    assert_eq!(c.ws("/api/ws", &[]).await.err(), Some(401));
    let (cookie, csrf) = c.login().await;
    // Cross-site WebSocket hijacking: no Origin, or a foreign one.
    assert_eq!(
        c.ws("/api/ws", &[("Cookie", &cookie)]).await.err(),
        Some(403)
    );
    let evil = [
        ("Cookie", cookie.as_str()),
        ("Origin", "https://evil.example"),
    ];
    assert_eq!(c.ws("/api/ws", &evil).await.err(), Some(403));

    let (_, token) = c
        .store
        .create_token("viewer".into(), BTreeSet::from([Scope::Read]), DAY)
        .await
        .unwrap();
    let bearer = format!("Bearer {}", token.expose());
    // A bearer header does not open the socket; a ticket does.
    let h = [("Authorization", bearer.as_str())];
    assert_eq!(c.ws("/api/ws", &h).await.err(), Some(401));
    let t = c.post("/api/ws/ticket", &h, json!({})).await;
    assert_eq!(t.status, 200, "{}", t.body);
    let ticket = t.body["ticket"].as_str().unwrap().to_owned();
    assert!(t.body["expires_at"].as_i64().unwrap() > c.store.now_ms());
    let path = format!("/api/ws?ticket={ticket}");
    let mut ws = c.ws(&path, &[]).await.expect("a ticket opens the socket");
    assert_eq!(ws.next().await.unwrap()["type"], "hello");
    // Single use.
    assert_eq!(c.ws(&path, &[]).await.err(), Some(401));
    // Expired after 30 s.
    let t = c.post("/api/ws/ticket", &h, json!({})).await;
    f.det.advance(Duration::from_secs(31));
    let late = format!("/api/ws?ticket={}", t.body["ticket"].as_str().unwrap());
    assert_eq!(c.ws(&late, &[]).await.err(), Some(401));

    // A cookie login asks for a ticket only with its CSRF token.
    let ck = [("Cookie", cookie.as_str())];
    assert_eq!(c.post("/api/ws/ticket", &ck, json!({})).await.status, 403);
    let ok = [("Cookie", cookie.as_str()), ("X-CSRF-Token", csrf.as_str())];
    assert_eq!(c.post("/api/ws/ticket", &ok, json!({})).await.status, 200);
}

#[tokio::test]
async fn a_socket_closes_when_its_login_or_token_ends() {
    let (_f, c) = setup().await;
    let (mut ws, (cookie, csrf)) = open(&c).await;
    ws.next_of("snapshot").await;
    let h = [("Cookie", cookie.as_str()), ("X-CSRF-Token", csrf.as_str())];
    let r = c.post("/api/auth/logout", &h, json!({})).await;
    assert_eq!(r.status, 204);
    let closed = loop {
        if let Err(code) = ws.next().await {
            break code;
        }
    };
    assert_eq!(closed, Some(CLOSE_LOGIN_ENDED));

    let (_, token) = c
        .store
        .create_token("viewer".into(), BTreeSet::from([Scope::Read]), DAY)
        .await
        .unwrap();
    let bearer = format!("Bearer {}", token.expose());
    let t = c
        .post("/api/ws/ticket", &[("Authorization", &bearer)], json!({}))
        .await;
    let path = format!("/api/ws?ticket={}", t.body["ticket"].as_str().unwrap());
    let mut ws = c.ws(&path, &[]).await.unwrap();
    ws.next_of("snapshot").await;
    assert!(c.store.revoke_token("viewer".into()).await.unwrap());
    let closed = loop {
        if let Err(code) = ws.next().await {
            break code;
        }
    };
    assert_eq!(closed, Some(CLOSE_LOGIN_ENDED));
}

#[tokio::test]
async fn a_ticket_dies_with_the_login_or_token_that_asked_for_it() {
    let (_f, c) = setup().await;

    // A cookie login asks for a ticket, then logs out: the ticket is
    // refused at the upgrade although it is unused and not expired.
    let (cookie, csrf) = c.login().await;
    let h = [("Cookie", cookie.as_str()), ("X-CSRF-Token", csrf.as_str())];
    let t = c.post("/api/ws/ticket", &h, json!({})).await;
    assert_eq!(t.status, 200, "{}", t.body);
    assert_eq!(c.post("/api/auth/logout", &h, json!({})).await.status, 204);
    let path = format!("/api/ws?ticket={}", t.body["ticket"].as_str().unwrap());
    assert_eq!(c.ws(&path, &[]).await.err(), Some(401));

    // A token asks for a ticket and is revoked.
    let scopes = BTreeSet::from([Scope::Read]);
    let (_, token) = c
        .store
        .create_token("viewer".into(), scopes.clone(), DAY)
        .await
        .unwrap();
    let bearer = format!("Bearer {}", token.expose());
    let t = c
        .post("/api/ws/ticket", &[("Authorization", &bearer)], json!({}))
        .await;
    assert!(c.store.revoke_token("viewer".into()).await.unwrap());
    let path = format!("/api/ws?ticket={}", t.body["ticket"].as_str().unwrap());
    assert_eq!(c.ws(&path, &[]).await.err(), Some(401));

    // A token revoked and created again under the same name (same clock
    // time) is another principal: the old token's ticket stays refused.
    let (_, old) = c
        .store
        .create_token("viewer2".into(), scopes.clone(), DAY)
        .await
        .unwrap();
    let bearer = format!("Bearer {}", old.expose());
    let t = c
        .post("/api/ws/ticket", &[("Authorization", &bearer)], json!({}))
        .await;
    assert!(c.store.revoke_token("viewer2".into()).await.unwrap());
    c.store
        .create_token("viewer2".into(), scopes, DAY)
        .await
        .unwrap();
    let path = format!("/api/ws?ticket={}", t.body["ticket"].as_str().unwrap());
    assert_eq!(c.ws(&path, &[]).await.err(), Some(401));

    // A valid login's ticket still opens the socket.
    let (cookie, csrf) = c.login().await;
    let h = [("Cookie", cookie.as_str()), ("X-CSRF-Token", csrf.as_str())];
    let t = c.post("/api/ws/ticket", &h, json!({})).await;
    let path = format!("/api/ws?ticket={}", t.body["ticket"].as_str().unwrap());
    let mut ws = c.ws(&path, &[]).await.expect("a live login's ticket opens");
    assert_eq!(ws.next().await.unwrap()["type"], "hello");
}
