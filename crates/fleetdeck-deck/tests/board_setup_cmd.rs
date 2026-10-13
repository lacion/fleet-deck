//! A session start with a setup command (DL-148) over loopback HTTPS: only
//! the owner sends one, and only to a station that reports
//! `allow_setup_cmd`. A refusal stores and sends nothing.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use common::board::{BoardClient, Http, board};
use common::*;
use fleetdeck_proto::link::{Scope, StationInfo};
use serde_json::{Value, json};

const DAY: i64 = 24 * 60 * 60 * 1000;

fn start_body(id: &str) -> Value {
    json!({
        "station": "fake",
        "command_id": id,
        "command": {
            "op": "start", "prompt": null, "setup_cmd": "make setup",
            "spec": {
                "provider_instance": "claude", "cwd": "/w", "mode": null,
                "model": null, "title": null,
            },
        },
    })
}

fn info(allow_setup_cmd: Option<bool>) -> StationInfo {
    StationInfo {
        name: "fake".to_owned(),
        incarnation: "fake-station-db".to_owned().into(),
        head: 0,
        live_sessions: 0,
        version: None,
        max_sessions: Some(4),
        allow_shell: None,
        allow_setup_cmd,
        providers: Vec::new(),
        assist_tokens_today: None,
        assist_cap: None,
    }
}

async fn owner(c: &BoardClient) -> Vec<(String, String)> {
    let (cookie, csrf) = c.login().await;
    vec![
        ("Cookie".to_owned(), cookie),
        ("X-CSRF-Token".to_owned(), csrf),
    ]
}

async fn post(c: &BoardClient, h: &[(String, String)], body: Value) -> Http {
    let h: Vec<(&str, &str)> = h.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    c.post("/api/commands/session.start", &h, body).await
}

#[tokio::test]
async fn a_token_cannot_start_a_session_with_a_setup_command() {
    let mut f = fixture().await;
    let mut station = f.connect().await;
    f.store.set_station_info(&f.station, info(Some(true)));
    let c = board(Arc::clone(&f.ctx)).await;
    let (_, t) = c
        .store
        .create_token(
            "ops".into(),
            BTreeSet::from([Scope::Read, Scope::Drive]),
            DAY,
        )
        .await
        .unwrap();
    let bearer = vec![("Authorization".to_owned(), format!("Bearer {}", t.expose()))];
    let r = post(&c, &bearer, start_body("s1")).await;
    assert_eq!((r.status, r.code()), (403, "owner_only"), "{}", r.body);
    assert_eq!(
        r.body["message"],
        "only the owner's board login starts a session with a setup command"
    );
    assert!(f.store.commands().await.unwrap().is_empty());
    no_request(&mut station, Duration::from_millis(300)).await;
}

#[tokio::test]
async fn a_station_that_does_not_report_setup_support_refuses_with_409() {
    let mut f = fixture().await;
    let mut station = f.connect().await;
    let c = board(Arc::clone(&f.ctx)).await;
    let h = owner(&c).await;
    // No `station.info` yet, then an old station (absent), then off.
    for (n, kept) in [None, Some(info(None)), Some(info(Some(false)))]
        .into_iter()
        .enumerate()
    {
        if let Some(kept) = kept {
            f.store.set_station_info(&f.station, kept);
        }
        let r = post(&c, &h, start_body(&format!("n{n}"))).await;
        assert_eq!(
            (r.status, r.code()),
            (409, "setup_not_supported"),
            "{n}: {}",
            r.body
        );
        assert_eq!(
            r.body["message"],
            "this station does not run setup commands"
        );
    }
    assert!(f.store.commands().await.unwrap().is_empty());
    no_request(&mut station, Duration::from_millis(300)).await;
}

#[tokio::test]
async fn the_owner_sends_a_setup_command_to_a_station_that_runs_it() {
    let mut f = fixture().await;
    let mut station = f.connect().await;
    f.store.set_station_info(&f.station, info(Some(true)));
    let c = board(Arc::clone(&f.ctx)).await;
    let h = owner(&c).await;
    let call = post(&c, &h, start_body("ok1"));
    let (r, req) = tokio::join!(call, Fixture::answer(&mut station, accepted));
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(req.method, "session.start");
    assert_eq!(req.params["command"]["setup_cmd"], "make setup");
}
