//! The helpers of the share tests: a deck with a linked station, the owner,
//! the join of a member and the commands that the station accepts.

#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use crate::common::board::{BoardClient, Http, board};
use crate::common::*;
use fleetdeck_proto::SessionSummary;
use fleetdeck_proto::event::{ProviderKind, SessionState};
use serde_json::{Value, json};

pub fn card(id: &str) -> SessionSummary {
    let mut s = SessionSummary::new(id.into());
    s.state = SessionState::Running;
    s.cwd = Some("/p/app".to_owned());
    s.provider = Some(ProviderKind::Claude);
    s
}

pub type Headers = Vec<(String, String)>;

pub fn refs(h: &Headers) -> Vec<(&str, &str)> {
    h.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect()
}

pub async fn owner(c: &BoardClient) -> Headers {
    let (cookie, csrf) = c.login().await;
    vec![
        ("Cookie".to_owned(), cookie),
        ("X-CSRF-Token".to_owned(), csrf),
    ]
}

pub async fn post(c: &BoardClient, h: &Headers, path: &str, body: Value) -> Http {
    c.post(path, &refs(h), body).await
}

pub async fn get(c: &BoardClient, h: &Headers, path: &str) -> Http {
    c.get(path, &refs(h)).await
}

pub fn expect(r: &Http, status: u16, code: &str) {
    assert_eq!((r.status, r.code()), (status, code), "{}", r.body);
}

/// A deck with a linked station that holds the sessions `s1` and `s2`.
pub async fn setup() -> (Fixture, fleetdeck_link::LiveLink, BoardClient, Headers) {
    let mut f = fixture().await;
    let station = f.connect().await;
    f.store
        .apply_snapshot(
            f.station.clone(),
            f.inc.clone(),
            0,
            vec![card("s1"), card("s2")],
        )
        .await
        .unwrap();
    let c = board(Arc::clone(&f.ctx)).await;
    let h = owner(&c).await;
    (f, station, c, h)
}

/// The owner makes a share; a person joins. Returns the member's headers.
pub async fn join(
    c: &BoardClient,
    owner: &Headers,
    share: Value,
    name: &str,
) -> (Headers, String, Value) {
    let r = post(c, owner, "/api/shares", share).await;
    assert_eq!(r.status, 200, "{}", r.body);
    let share_id = r.body["share_id"].as_str().unwrap().to_owned();
    let url = r.body["url"].as_str().unwrap();
    let token = url.split("#join=").nth(1).unwrap();
    let j = c
        .post(
            "/api/auth/join",
            &[],
            json!({ "token": token, "display_name": name }),
        )
        .await;
    assert_eq!(j.status, 200, "{}", j.body);
    let cookie = j
        .header("set-cookie")
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let csrf = j.body["csrf"].as_str().unwrap().to_owned();
    (
        vec![
            ("Cookie".to_owned(), cookie),
            ("X-CSRF-Token".to_owned(), csrf),
        ],
        share_id,
        j.body,
    )
}

/// Posts a command that the station accepts; returns the answer and the
/// request that the station got.
pub async fn send_ok(
    c: &BoardClient,
    h: &Headers,
    station: &mut fleetdeck_link::LiveLink,
    method: &str,
    body: Value,
) -> (Http, fleetdeck_link::control::InboundRequest) {
    let path = format!("/api/commands/{method}");
    let call = post(c, h, &path, body);
    tokio::join!(call, Fixture::answer(station, accepted))
}

pub fn prompt(st: &str, id: &str, session: &str) -> Value {
    json!({ "station": st, "command_id": id,
            "command": { "op": "prompt", "session_id": session, "input": { "text": "hi" } } })
}
