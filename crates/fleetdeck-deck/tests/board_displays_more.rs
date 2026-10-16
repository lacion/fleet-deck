//! The display principal over loopback HTTPS (R13 P2a, DL-201): the reduced
//! cards, the display WebSocket, the owner's settings and revoke, and the
//! session limits. The pairing and the guard are in `board_displays.rs`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
mod displays_support;
mod share_support;

use std::collections::BTreeSet;
use std::time::Duration;

use common::board::BoardClient;
use displays_support::*;
use fleetdeck_core::Secret;
use fleetdeck_core::identity::Identity;
use fleetdeck_deck::store::AuditResult;
use fleetdeck_deck::store::displays::DISPLAY_IDLE_MS;
use fleetdeck_deck::store::stations::NewStation;
use fleetdeck_proto::event::{RequestKind, ToolKind};
use fleetdeck_proto::link::Scope;
use fleetdeck_proto::session::{OpenRequest, RunningTool, SessionError};
use fleetdeck_proto::{IncarnationId, StationId};
use serde_json::{Value, json};
use share_support::*;

/// A card with a sentinel in every field that a display must not get.
fn secret_card(id: &str) -> fleetdeck_proto::SessionSummary {
    let mut s = card(id);
    s.cwd = Some(format!("/home/{SECRET}/app"));
    s.digest = Some(format!("digest {SECRET}"));
    s.branch = Some(format!("branch-{SECRET}"));
    s.worktree = Some(format!("/wt/{SECRET}"));
    s.title = Some("Fix the build".to_owned());
    s.repo = Some(fleetdeck_proto::session::RepoInfo {
        root: format!("/srv/{SECRET}/repo"),
        origin: Some(format!("github.com/{SECRET}")),
    });
    s.open_requests = vec![OpenRequest {
        request_id: "r1".into(),
        node_id: Some("n1".into()),
        request_kind: RequestKind::Command,
        title: format!("Run {SECRET} {}", "x".repeat(80)),
        details: format!("rm -rf {SECRET}"),
        input: Some(json!({ "command": SECRET })),
        options: Vec::new(),
        response: fleetdeck_proto::event::ResponseChannel::RpcReply,
        requested_at: 1234,
    }];
    s.running_tools = vec![RunningTool {
        node_id: "n2".into(),
        tool_kind: ToolKind::Shell,
        title: "Bash".to_owned(),
        input_summary: format!("curl {SECRET}"),
    }];
    s.last_error = Some(SessionError {
        code: "boom".to_owned(),
        message: format!("failed {SECRET}"),
        retry_at: None,
    });
    s
}

async fn second_station(c: &BoardClient) -> StationId {
    let rec = c
        .store
        .pin_station(NewStation {
            key: Identity::generate().unwrap().public_key(),
            name: "other".to_owned(),
            scopes: BTreeSet::from([Scope::Read]),
            dial_url: Some(format!("https://{SECRET}.example:1")),
        })
        .await
        .unwrap();
    c.store
        .apply_snapshot(
            rec.station_id.clone(),
            IncarnationId::from("i2"),
            0,
            vec![card("outside")],
        )
        .await
        .unwrap();
    rec.station_id
}

#[tokio::test]
async fn a_display_gets_reduced_cards_and_only_the_stations_of_its_ceiling() {
    let (f, c, _h, st) = setup_deck().await;
    f.store
        .apply_snapshot(f.station.clone(), f.inc.clone(), 0, vec![secret_card("s1")])
        .await
        .unwrap();
    let other = second_station(&c).await;
    let (_, cookie) = display_login(&c, "Wall", &[&st]).await;

    let r = get_with(&c, "/api/sessions", &cookie).await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert!(!r.text.contains(SECRET), "{}", r.text);
    let cards = r.body.as_array().unwrap();
    assert_eq!(cards.len(), 1, "{}", r.body);
    assert_eq!(cards[0]["summary"]["session_id"], "s1");
    assert_eq!(cards[0]["summary"]["title"], "Fix the build");
    assert_eq!(
        cards[0]["summary"]["repo"],
        json!({ "root": "repo", "origin": null })
    );
    assert_eq!(
        cards[0]["summary"]["open_requests"][0]["request_kind"],
        "command"
    );
    assert_eq!(
        cards[0]["summary"]["open_requests"][0]["title"],
        "Approve a command"
    );
    assert_eq!(
        cards[0]["summary"]["open_requests"][0]["requested_at"],
        1234
    );
    assert_eq!(cards[0]["summary"]["running_tools"], json!([]));
    assert_eq!(cards[0]["summary"]["cwd"], Value::Null);
    assert!(cards.iter().all(|c| c["station_id"] != other.0.as_str()));

    let r = get_with(&c, "/api/stations", &cookie).await;
    assert!(!r.text.contains(SECRET), "{}", r.text);
    let stations = r.body.as_array().unwrap();
    assert_eq!(stations.len(), 1, "{}", r.body);
    assert_eq!(stations[0]["station_id"], st.as_str());
    assert_eq!(stations[0]["key"], "");
    assert_eq!(stations[0]["dial_url"], Value::Null);
    assert_eq!(stations[0]["info"], Value::Null);

    // The owner still gets the full card.
    let h2 = owner(&c).await;
    let full = get(&c, &h2, "/api/sessions").await;
    assert!(full.text.contains(SECRET));
}

#[tokio::test]
async fn the_display_socket_sends_reduced_snapshots_and_deltas_and_refuses_every_client_message() {
    let (f, c, h, st) = setup_deck().await;
    f.store
        .apply_snapshot(f.station.clone(), f.inc.clone(), 0, vec![secret_card("s1")])
        .await
        .unwrap();
    second_station(&c).await;
    let (display_id, cookie) = display_login(&c, "Wall", &[&st]).await;
    let t = c
        .post("/api/ws/ticket", &[("Cookie", &cookie)], json!({}))
        .await;
    let ticket = t.body["ticket"].as_str().unwrap().to_owned();
    let mut ws = c
        .ws(&format!("/api/ws?ticket={ticket}"), &[])
        .await
        .expect("the display socket opens");
    assert_eq!(ws.next().await.unwrap()["type"], "hello");
    let snap = ws.next().await.unwrap();
    assert!(!snap.to_string().contains(SECRET), "{snap}");
    assert_eq!(snap["sessions"].as_array().unwrap().len(), 1);
    assert_eq!(snap["stations"].as_array().unwrap().len(), 1);
    assert_eq!(snap["stations"][0]["key"], "");

    // A change of the card reaches the display, reduced.
    let mut changed = secret_card("s1");
    changed.title = Some("Fix the tests".to_owned());
    f.store
        .apply_snapshot(f.station.clone(), f.inc.clone(), 1, vec![changed])
        .await
        .unwrap();
    let delta = ws.next_of("delta").await;
    assert!(!delta.to_string().contains(SECRET), "{delta}");
    assert_eq!(delta["sessions"][0]["summary"]["title"], "Fix the tests");

    // Subscribe and presence are refused; the socket stays open.
    ws.send(
        json!({ "type": "subscribe", "station_id": st, "session_id": "s1",
                    "incarnation": null, "after": null }),
    )
    .await;
    assert_eq!(ws.next_of("error").await["code"], "display_read_only");
    ws.send(json!({ "type": "presence", "station_id": st, "session_id": "s1" }))
        .await;
    assert_eq!(ws.next_of("error").await["code"], "display_read_only");

    // People chat never reaches a display.
    let r = post(
        &c,
        &h,
        &format!("/api/sessions/{st}/s1/chat"),
        json!({ "text": "hi" }),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    ws.quiet(Duration::from_millis(600)).await;

    // A revoke closes the socket within 5 s.
    let r = post(
        &c,
        &h,
        &format!("/api/displays/{display_id}/revoke"),
        json!({}),
    )
    .await;
    assert_eq!(r.status, 204, "{}", r.body);
    let started = std::time::Instant::now();
    loop {
        if let Err(code) = ws.next().await {
            assert_eq!(code, Some(4401));
            break;
        }
    }
    assert!(started.elapsed() < Duration::from_secs(5));
    // The ticket route and the cookie are dead.
    expect(
        &c.post("/api/ws/ticket", &[("Cookie", &cookie)], json!({}))
            .await,
        401,
        "login_required",
    );
}

#[tokio::test]
async fn a_ticket_of_a_display_that_is_revoked_before_use_is_refused() {
    let (_f, c, h, st) = setup_deck().await;
    let (display_id, cookie) = display_login(&c, "Wall", &[&st]).await;
    let t = c
        .post("/api/ws/ticket", &[("Cookie", &cookie)], json!({}))
        .await;
    let ticket = t.body["ticket"].as_str().unwrap().to_owned();
    post(
        &c,
        &h,
        &format!("/api/displays/{display_id}/revoke"),
        json!({}),
    )
    .await;
    assert_eq!(
        c.ws(&format!("/api/ws?ticket={ticket}"), &[]).await.err(),
        Some(401)
    );
}

#[tokio::test]
async fn the_owner_changes_and_revokes_a_display() {
    let (_f, c, h, st) = setup_deck().await;
    let other = second_station(&c).await;
    let (id, cookie) = display_login(&c, "Wall", &[&st]).await;
    let path = format!("/api/displays/{id}/settings");
    // A name and the quiet hours need no elevation.
    let r = post(
        &c,
        &h,
        &path,
        json!({ "name": " Lobby ", "quiet_hours": "22:00-07:00", "wake_on_needs_you": false }),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(
        (
            &r.body["name"],
            &r.body["quiet_hours"],
            &r.body["wake_on_needs_you"]
        ),
        (&json!("Lobby"), &json!("22:00-07:00"), &json!(false))
    );
    // `null` clears the quiet hours.
    let r = post(&c, &h, &path, json!({ "quiet_hours": null })).await;
    assert_eq!(r.body["quiet_hours"], Value::Null, "{}", r.body);
    for bad in [
        json!({ "name": "" }),
        json!({ "quiet_hours": "25:00-07:00" }),
        json!({ "ceiling": [] }),
        json!({ "ceiling": [st, st] }),
    ] {
        expect(&post(&c, &h, &path, bad).await, 400, "invalid_display");
    }
    expect(
        &post(&c, &h, &path, json!({ "nope": 1 })).await,
        400,
        "invalid_body",
    );
    // More diffs and a larger ceiling need an elevation; a smaller one does not.
    expect(
        &post(&c, &h, &path, json!({ "show_diffs": true })).await,
        400,
        "elevation_required",
    );
    expect(
        &post(&c, &h, &path, json!({ "ceiling": [st, other.0] })).await,
        400,
        "elevation_required",
    );
    let e = elevate(&c, &h).await;
    let r = post(
        &c,
        &h,
        &path,
        json!({ "show_diffs": true, "ceiling": [st, other.0], "elevation": e }),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body["show_diffs"], true);
    let r = post(
        &c,
        &h,
        &path,
        json!({ "ceiling": [st], "show_diffs": false }),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    // The display sees the new name through its own routes.
    assert_eq!(
        get_with(&c, "/api/displays/self", &cookie).await.body["name"],
        "Lobby"
    );
    expect(
        &post(
            &c,
            &h,
            "/api/displays/nope/settings",
            json!({ "name": "x" }),
        )
        .await,
        404,
        "display_unknown",
    );

    // The display is listed, then revoked: no list entry, no login, 404 after.
    let list = get(&c, &h, "/api/displays").await;
    assert_eq!(list.body["displays"][0]["display_id"], id.as_str());
    let rv = post(&c, &h, &format!("/api/displays/{id}/revoke"), json!({})).await;
    assert_eq!(rv.status, 204, "{}", rv.body);
    expect(
        &get_with(&c, "/api/displays/self", &cookie).await,
        401,
        "login_required",
    );
    assert_eq!(
        get(&c, &h, "/api/displays").await.body["displays"],
        json!([])
    );
    expect(
        &post(&c, &h, &format!("/api/displays/{id}/revoke"), json!({})).await,
        404,
        "display_unknown",
    );
    expect(
        &post(&c, &h, &path, json!({ "name": "x" })).await,
        404,
        "display_unknown",
    );
    assert!(
        !c.audited("display.revoke", AuditResult::Ok)
            .await
            .is_empty()
    );
    assert!(
        !c.audited("display.settings", AuditResult::Ok)
            .await
            .is_empty()
    );
    // A display cannot use the owner rows.
    let (_, dcookie) = display_login(&c, "Wall2", &[&st]).await;
    let dh: Headers = vec![("Cookie".to_owned(), dcookie)];
    expect(
        &post(&c, &dh, &path, json!({})).await,
        403,
        "display_read_only",
    );
}

#[tokio::test]
async fn a_display_session_ends_90_days_after_it_started_and_30_days_without_use() {
    let (f, c, _h, st) = setup_deck().await;
    let (_, cookie) = display_login(&c, "Wall", &[&st]).await;
    let value = Secret::new(cookie.split('=').nth(1).unwrap().to_owned());
    // Used every day: the idle limit never ends it, the absolute one does.
    let day = Duration::from_secs(24 * 60 * 60);
    for d in 1..=89 {
        f.det.advance(day);
        assert!(
            c.store.display_session(&value).await.unwrap().is_some(),
            "day {d}"
        );
    }
    f.det.advance(day + Duration::from_secs(1));
    assert!(c.store.display_session(&value).await.unwrap().is_none());

    // Unused for 30 days: gone.
    let (_, cookie) = display_login(&c, "Wall3", &[&st]).await;
    let value = Secret::new(cookie.split('=').nth(1).unwrap().to_owned());
    f.det
        .advance(Duration::from_millis(DISPLAY_IDLE_MS as u64 + 1));
    assert!(c.store.display_session(&value).await.unwrap().is_none());
}

const TOKEN: &str = "sentinel-7f3a";

/// A card with one open request whose title, details and input hold `TOKEN`.
fn deploy_card() -> fleetdeck_proto::SessionSummary {
    let mut s = card("s1");
    s.open_requests = vec![OpenRequest {
        request_id: "r1".into(),
        node_id: None,
        request_kind: RequestKind::Command,
        title: format!("TOKEN={TOKEN} ./deploy"),
        details: format!("TOKEN={TOKEN} ./deploy"),
        input: Some(json!({ "command": format!("TOKEN={TOKEN} ./deploy") })),
        options: Vec::new(),
        response: fleetdeck_proto::event::ResponseChannel::RpcReply,
        requested_at: 1,
    }];
    s
}

#[tokio::test]
async fn a_request_title_on_a_display_is_a_fixed_label_in_every_projection() {
    let (f, c, _h, st) = setup_deck().await;
    f.store
        .apply_snapshot(f.station.clone(), f.inc.clone(), 0, vec![card("s1")])
        .await
        .unwrap();
    let (_, cookie) = display_login(&c, "Wall", &[&st]).await;
    let t = c
        .post("/api/ws/ticket", &[("Cookie", &cookie)], json!({}))
        .await;
    let ticket = t.body["ticket"].as_str().unwrap().to_owned();
    let mut ws = c
        .ws(&format!("/api/ws?ticket={ticket}"), &[])
        .await
        .expect("the display socket opens");
    assert_eq!(ws.next().await.unwrap()["type"], "hello");
    let snap = ws.next().await.unwrap();
    assert_eq!(snap["type"], "snapshot");

    // The request arrives in a delta.
    f.store
        .apply_snapshot(f.station.clone(), f.inc.clone(), 1, vec![deploy_card()])
        .await
        .unwrap();
    let delta = ws.next_of("delta").await;
    assert!(!delta.to_string().contains(TOKEN), "{delta}");
    assert_eq!(
        delta["sessions"][0]["summary"]["open_requests"][0]["title"],
        "Approve a command"
    );

    // A new socket gets it in the snapshot, the list route in the answer.
    let t = c
        .post("/api/ws/ticket", &[("Cookie", &cookie)], json!({}))
        .await;
    let ticket = t.body["ticket"].as_str().unwrap().to_owned();
    let mut ws2 = c
        .ws(&format!("/api/ws?ticket={ticket}"), &[])
        .await
        .unwrap();
    ws2.next_of("hello").await;
    let snap = ws2.next_of("snapshot").await;
    assert!(!snap.to_string().contains(TOKEN), "{snap}");
    assert_eq!(
        snap["sessions"][0]["summary"]["open_requests"][0]["title"],
        "Approve a command"
    );
    let r = get_with(&c, "/api/sessions", &cookie).await;
    assert!(!r.text.contains(TOKEN), "{}", r.text);
    assert_eq!(
        r.body[0]["summary"]["open_requests"][0]["title"],
        "Approve a command"
    );
}

#[tokio::test]
async fn a_ceiling_change_reaches_an_idle_display_socket_at_once() {
    let (f, c, h, st) = setup_deck().await;
    f.store
        .apply_snapshot(f.station.clone(), f.inc.clone(), 0, vec![card("s1")])
        .await
        .unwrap();
    let other = second_station(&c).await;
    let (display_id, cookie) = display_login(&c, "Wall", &[st.as_str(), other.as_str()]).await;
    let t = c
        .post("/api/ws/ticket", &[("Cookie", &cookie)], json!({}))
        .await;
    let ticket = t.body["ticket"].as_str().unwrap().to_owned();
    let mut ws = c
        .ws(&format!("/api/ws?ticket={ticket}"), &[])
        .await
        .unwrap();
    ws.next_of("hello").await;
    let snap = ws.next_of("snapshot").await;
    assert_eq!(snap["sessions"].as_array().unwrap().len(), 2, "{snap}");
    let path = format!("/api/displays/{display_id}/settings");

    // Narrowing: a delta removes the cards of the other station.
    let r = post(&c, &h, &path, json!({ "ceiling": [st] })).await;
    assert_eq!(r.status, 200, "{}", r.body);
    let delta = tokio::time::timeout(Duration::from_secs(2), ws.next_of("delta"))
        .await
        .expect("a delta within 2 s");
    let removed = delta["removed"].as_array().unwrap();
    assert_eq!(removed.len(), 1, "{delta}");
    assert_eq!(removed[0]["station_id"], other.as_str());
    assert_eq!(delta["stations"].as_array().unwrap().len(), 1, "{delta}");

    // Widening: a delta adds them again.
    let e = elevate(&c, &h).await;
    let r = post(
        &c,
        &h,
        &path,
        json!({ "ceiling": [st, other.as_str()], "elevation": e }),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    let delta = tokio::time::timeout(Duration::from_secs(2), ws.next_of("delta"))
        .await
        .expect("a delta within 2 s");
    let added = delta["sessions"].as_array().unwrap();
    assert_eq!(added.len(), 1, "{delta}");
    assert_eq!(added[0]["station_id"], other.as_str());
}

#[tokio::test]
async fn a_display_that_never_reads_is_cut_off_after_a_revoke() {
    use tokio_tungstenite::tungstenite::Message;
    let (f, c, h, st) = setup_deck().await;
    f.store
        .apply_snapshot(f.station.clone(), f.inc.clone(), 0, vec![card("s1")])
        .await
        .unwrap();
    let (display_id, cookie) = display_login(&c, "Wall", &[&st]).await;
    let t = c
        .post("/api/ws/ticket", &[("Cookie", &cookie)], json!({}))
        .await;
    let ticket = t.body["ticket"].as_str().unwrap().to_owned();
    let mut ws = c
        .ws(&format!("/api/ws?ticket={ticket}"), &[])
        .await
        .unwrap();
    ws.next_of("hello").await;
    ws.next_of("snapshot").await;

    // From here the client reads nothing. Big cards fill the buffers of the
    // transport, so that a send of the server cannot finish.
    let big = "x".repeat(8 * 1024 * 1024);
    for round in 1..=4 {
        let mut s = card("s1");
        s.title = Some(format!("{round}{big}"));
        f.store
            .apply_snapshot(f.station.clone(), f.inc.clone(), round, vec![s])
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let r = post(
        &c,
        &h,
        &format!("/api/displays/{display_id}/revoke"),
        json!({}),
    )
    .await;
    assert_eq!(r.status, 204, "{}", r.body);

    // A server that still holds the socket takes the small messages of the
    // client (its own buffer has room); a server that dropped it does not.
    let started = std::time::Instant::now();
    let ping = json!({ "type": "unsubscribe", "station_id": st, "session_id": "s1" });
    let mut ended = false;
    {
        use futures_util::SinkExt;
        while started.elapsed() < Duration::from_secs(5) {
            let sent = ws.0.send(Message::Text(ping.to_string().into())).await;
            if sent.is_err() {
                ended = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
    assert!(
        ended,
        "the server still held the socket 5 s after the revoke"
    );
}
