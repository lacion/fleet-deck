//! `GET /api/analytics` over loopback HTTPS (B29, `docs/v2/design/analytics.md`
//! 3.9): the audience from the principal, the page loop, the member defense
//! in depth and the station states. The station is a fake that answers
//! `station.analytics` itself.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
mod displays_support;
mod share_support;

use std::collections::BTreeSet;

use common::board::{BoardClient, Http};
use common::*;
use displays_support::display_login;
use fleetdeck_link::LiveLink;
use fleetdeck_link::control::InboundRequest;
use fleetdeck_proto::link::{Outcome, Scope, WireError};
use serde_json::{Value, json};
use share_support::*;

const HOUR: i64 = 3_600_000;
const SINCE: i64 = 100 * HOUR;
const UNTIL: i64 = 110 * HOUR;

fn path() -> String {
    format!("/api/analytics?since={SINCE}&until={UNTIL}")
}

fn usage(hour: i64, session: Option<&str>, requests: u64) -> Value {
    let mut row = json!({
        "hour": hour, "provider": "claude", "model": "claude-opus-5-5",
        "conversation": "0123456789abcdef", "source": 0, "requests": requests,
        "input": 10, "cache_read": 0, "cache_write": 0, "output": 5, "reasoning": 0,
    });
    if let Some(s) = session {
        row["session_id"] = json!(s);
    }
    row
}

fn source(owner_fields: bool, state: Value) -> Value {
    let mut s = json!({
        "provider": "claude", "key": "0123456789abcdef", "verified": true,
        "complete": true, "first_at": SINCE,
        "scanned_at": null, "files": null, "pending_bytes": null,
        "malformed": null, "keeps_days": null,
    });
    s["state"] = state["state"].clone();
    if let Some(reason) = state.get("reason") {
        s["reason"] = reason.clone();
    }
    if owner_fields {
        s["scanned_at"] = json!(5);
        s["files"] = json!(7);
        s["pending_bytes"] = json!(8);
        s["malformed"] = json!(9);
        s["keeps_days"] = json!(30);
    }
    s
}

fn page_json(usage: Vec<Value>, activity: Vec<Value>, next: Option<i64>) -> Value {
    let mut v = json!({
        "read_at": 1, "price_table": "2026-10-24",
        "sources": [source(false, json!({ "state": "ok" }))],
        "coverage": { "events_since": SINCE, "kept_since": SINCE },
        "usage": usage, "activity": activity,
    });
    if let Some(n) = next {
        v["next_before"] = json!(n);
    }
    v
}

fn activity(hour: i64, session: &str) -> Value {
    json!({ "hour": hour, "session_id": session, "counts": { "turns.done": 1 } })
}

/// One GET that the fake station answers with `answers`, one per request.
async fn call(
    c: &BoardClient,
    h: &Headers,
    station: &mut LiveLink,
    url: &str,
    answers: Vec<Outcome>,
) -> (Http, Vec<InboundRequest>) {
    let get = get(c, h, url);
    let serve = async {
        let mut seen = Vec::new();
        for outcome in answers {
            let req = next_request(station).await;
            station.control.respond(req.id, outcome).await.unwrap();
            seen.push(req);
        }
        seen
    };
    tokio::join!(get, serve)
}

async fn quiet(station: &mut LiveLink) {
    no_request(station, std::time::Duration::from_millis(300)).await;
}

fn rows(r: &Http, station: usize, key: &str) -> Vec<Value> {
    r.body["stations"][station]["answer"][key]
        .as_array()
        .unwrap()
        .clone()
}

#[tokio::test]
async fn the_owner_gets_every_station_with_unattributed_rows() {
    let (_f, mut station, c, h) = setup().await;
    let answer = page_json(
        vec![usage(SINCE, Some("s1"), 3), usage(SINCE, None, 4)],
        vec![activity(SINCE, "s1")],
        None,
    );
    let (r, seen) = call(&c, &h, &mut station, &path(), vec![Outcome::Ok(answer)]).await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body["owner"], true);
    assert_eq!(r.body["stations"].as_array().unwrap().len(), 1);
    assert_eq!(r.body["stations"][0]["state"], "ok");
    assert_eq!(rows(&r, 0, "usage").len(), 2);
    assert_eq!(rows(&r, 0, "activity").len(), 1);
    assert_eq!(seen[0].method, "station.analytics");
    assert_eq!(
        seen[0].params,
        json!({ "since": SINCE, "until": UNTIL, "before": null,
                "sessions": null, "audience": "owner" })
    );
}

#[tokio::test]
async fn a_member_with_sessions_gets_only_their_stations_and_rows() {
    let (f, mut station, c, h) = setup().await;
    let st = f.station.0.clone();
    let share = json!({ "kind": "session", "target": format!("{st}/s1"), "role": "viewer" });
    let (member, _, _) = join(&c, &h, share, "Alice").await;
    // The fake station wrongly sends the rows of two sessions and a row
    // without a session.
    let answer = page_json(
        vec![
            usage(SINCE, Some("s1"), 3),
            usage(SINCE, Some("s2"), 4),
            usage(SINCE, None, 5),
        ],
        vec![activity(SINCE, "s1"), activity(SINCE, "s2")],
        None,
    );
    let (r, seen) = call(
        &c,
        &member,
        &mut station,
        &path(),
        vec![Outcome::Ok(answer)],
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body["owner"], false);
    let usage_rows = rows(&r, 0, "usage");
    assert_eq!(usage_rows.len(), 1, "{}", r.body);
    assert_eq!(usage_rows[0]["session_id"], "s1");
    let activity_rows = rows(&r, 0, "activity");
    assert_eq!(activity_rows.len(), 1);
    assert_eq!(activity_rows[0]["session_id"], "s1");
    assert_eq!(seen[0].params["audience"], "member");
    assert_eq!(seen[0].params["sessions"], json!(["s1"]));
}

#[tokio::test]
async fn a_board_share_member_gets_no_unattributed_rows() {
    let (_f, mut station, c, h) = setup().await;
    let share = json!({ "kind": "board", "role": "viewer" });
    let (member, _, _) = join(&c, &h, share, "Bob").await;
    let answer = page_json(
        vec![usage(SINCE, Some("s2"), 3), usage(SINCE, None, 5)],
        vec![activity(SINCE, "s2")],
        None,
    );
    let (r, seen) = call(
        &c,
        &member,
        &mut station,
        &path(),
        vec![Outcome::Ok(answer)],
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body["owner"], false);
    let usage_rows = rows(&r, 0, "usage");
    assert_eq!(usage_rows.len(), 1);
    assert_eq!(usage_rows[0]["session_id"], "s2");
    assert_eq!(seen[0].params["audience"], "member");
    assert_eq!(seen[0].params["sessions"], json!(null));
}

#[tokio::test]
async fn a_member_without_history_gets_the_same_query() {
    let (f, mut station, c, h) = setup().await;
    let st = f.station.0.clone();
    let mut sent = Vec::new();
    for (name, history) in [("Alice", false), ("Carol", true)] {
        let share = json!({ "kind": "session", "target": format!("{st}/s1"),
                            "role": "viewer", "history": history });
        let (member, _, _) = join(&c, &h, share, name).await;
        let answer = page_json(vec![usage(SINCE, Some("s1"), 1)], vec![], None);
        let (r, seen) = call(
            &c,
            &member,
            &mut station,
            &path(),
            vec![Outcome::Ok(answer)],
        )
        .await;
        assert_eq!(r.status, 200, "{}", r.body);
        sent.push(seen[0].params.clone());
    }
    assert_eq!(sent[0], sent[1]);
}

#[tokio::test]
async fn the_deck_clears_owner_fields_for_a_member() {
    let (_f, mut station, c, h) = setup().await;
    let share = json!({ "kind": "board", "role": "viewer" });
    let (member, _, _) = join(&c, &h, share, "Dan").await;
    let mut answer = page_json(vec![], vec![], None);
    answer["sources"] = json!([source(
        true,
        json!({ "state": "unavailable", "reason": "home_not_trusted" })
    )]);
    let (r, _) = call(
        &c,
        &member,
        &mut station,
        &path(),
        vec![Outcome::Ok(answer)],
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    let s = &r.body["stations"][0]["answer"]["sources"][0];
    assert_eq!(s["state"], "unavailable");
    assert_eq!(s["reason"], "unavailable");
    for key in [
        "scanned_at",
        "files",
        "pending_bytes",
        "malformed",
        "keeps_days",
    ] {
        assert!(s[key].is_null(), "{key}: {s}");
    }
}

#[tokio::test]
async fn pages_are_read_until_next_before_is_none() {
    let (_f, mut station, c, h) = setup().await;
    let mut p2 = page_json(
        vec![usage(SINCE + 5 * HOUR, Some("s1"), 2)],
        vec![],
        Some(SINCE + 5 * HOUR),
    );
    p2["partial_hour"] = json!(SINCE + 6 * HOUR);
    let answers = vec![
        Outcome::Ok(page_json(
            vec![usage(SINCE + 8 * HOUR, Some("s1"), 1)],
            vec![activity(SINCE + 8 * HOUR, "s1")],
            Some(SINCE + 8 * HOUR),
        )),
        Outcome::Ok(p2),
        Outcome::Ok(page_json(vec![usage(SINCE, None, 3)], vec![], None)),
    ];
    let (r, seen) = call(&c, &h, &mut station, &path(), answers).await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(seen.len(), 3);
    assert_eq!(seen[0].params["before"], json!(null));
    assert_eq!(seen[1].params["before"], json!(SINCE + 8 * HOUR));
    assert_eq!(seen[2].params["before"], json!(SINCE + 5 * HOUR));
    assert_eq!(rows(&r, 0, "usage").len(), 3);
    assert_eq!(rows(&r, 0, "activity").len(), 1);
    let st = &r.body["stations"][0];
    assert!(st["truncated_before"].is_null());
    assert_eq!(st["partial_hours"], json!([SINCE + 6 * HOUR]));
    assert!(st["answer"]["next_before"].is_null());
    quiet(&mut station).await;
}

#[tokio::test]
async fn the_sixteenth_page_sets_truncated_before() {
    let (_f, mut station, c, h) = setup().await;
    let answers: Vec<Outcome> = (0..16)
        .map(|i| {
            let before = UNTIL - (i + 1) * HOUR;
            Outcome::Ok(page_json(
                vec![usage(before, Some("s1"), 1)],
                vec![],
                Some(before),
            ))
        })
        .collect();
    let (r, seen) = call(&c, &h, &mut station, &path(), answers).await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(seen.len(), 16);
    let st = &r.body["stations"][0];
    assert_eq!(st["state"], "ok");
    assert_eq!(st["truncated_before"], json!(UNTIL - 16 * HOUR));
    assert_eq!(rows(&r, 0, "usage").len(), 16);
    // No seventeenth call.
    quiet(&mut station).await;
}

#[tokio::test]
async fn a_member_asking_an_uncovered_station_gets_404() {
    let (f, mut station, c, h) = setup().await;
    let share = json!({ "kind": "session", "target": "other-station/s1", "role": "viewer" });
    let (member, _, _) = join(&c, &h, share, "Eve").await;
    let r = get(&c, &member, &format!("{}&station=fake", path())).await;
    expect(&r, 404, "unknown_station");
    // The owner asking a name that does not exist gets the same answer.
    let r = get(&c, &h, &format!("{}&station=no-such", path())).await;
    expect(&r, 404, "unknown_station");
    let _ = f;
    quiet(&mut station).await;
}

#[tokio::test]
async fn a_token_and_a_display_get_403() {
    let (f, mut station, c, _h) = setup().await;
    let st = f.station.0.clone();
    let (_, token) = f
        .store
        .create_token(
            "reader".into(),
            BTreeSet::from([Scope::Read, Scope::Drive, Scope::Admin]),
            86_400_000,
        )
        .await
        .unwrap();
    let auth = format!("Bearer {}", token.expose());
    let r = c.get(&path(), &[("Authorization", auth.as_str())]).await;
    expect(&r, 403, "owner_only");
    let (_, cookie) = display_login(&c, "Wall", &[&st]).await;
    let origin = c.origin();
    let r = c
        .get(&path(), &[("Cookie", &cookie), ("Origin", &origin)])
        .await;
    assert_eq!(r.status, 403, "{}", r.body);
    quiet(&mut station).await;
}

#[tokio::test]
async fn an_offline_and_an_old_station_are_states_not_errors() {
    let (f, mut station, c, h) = setup().await;
    f.store.set_link_features(&f.station, 98, vec![]);
    let r = get(&c, &h, &path()).await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body["stations"][0]["state"], "unsupported");
    assert!(r.body["stations"][0]["answer"].is_null());
    f.store.clear_link_features(&f.station, 98);
    let r = get(&c, &h, &path()).await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body["stations"][0]["state"], "offline");
    quiet(&mut station).await;
}

#[tokio::test]
async fn a_station_timeout_is_timeout() {
    let (_f, mut station, c, h) = setup().await;
    // The station reads the request and never answers: the deck waits 15 s.
    let url = path();
    let get = get(&c, &h, &url);
    let silent = async {
        let req = next_request(&mut station).await;
        tokio::time::sleep(std::time::Duration::from_secs(20)).await;
        req
    };
    let (r, _) = tokio::join!(get, silent);
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body["stations"][0]["state"], "timeout");
    assert!(r.body["stations"][0]["answer"].is_null());
}

#[tokio::test]
async fn a_busy_or_failing_station_is_a_state() {
    let (_f, mut station, c, h) = setup().await;
    let busy = Outcome::Err(WireError::new("busy", "STATION-TEXT"));
    let (r, _) = call(&c, &h, &mut station, &path(), vec![busy]).await;
    assert_eq!(r.body["stations"][0]["state"], "busy");
    let boom = Outcome::Err(WireError::new("boom", "STATION-TEXT"));
    let (r, _) = call(&c, &h, &mut station, &path(), vec![boom]).await;
    assert_eq!(r.body["stations"][0]["state"], "error");
    assert!(!r.text.contains("STATION-TEXT"), "{}", r.text);
}

#[tokio::test]
async fn bad_params_are_400() {
    let (_f, mut station, c, h) = setup().await;
    for query in [
        format!("since={}&until={UNTIL}", SINCE + 1),
        format!("since={UNTIL}&until={SINCE}"),
        format!("since=-{HOUR}&until={UNTIL}"),
        format!("since=abc&until={UNTIL}"),
        format!("since={SINCE}"),
        format!("until={UNTIL}"),
        format!("since={SINCE}&until={UNTIL}&station=a%00b"),
    ] {
        let r = get(&c, &h, &format!("/api/analytics?{query}")).await;
        expect(&r, 400, "invalid_params");
    }
    quiet(&mut station).await;
}
