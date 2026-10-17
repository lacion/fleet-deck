//! The `!` shell commands (B8) over loopback HTTPS: `session.shell_command`
//! and `session.release_shell` go through the generic command route. Only the
//! owner's board login sends them; `session.shell_command` also needs a
//! station that reports `allow_shell`. A refusal stores and sends nothing.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
mod share_support;

use std::collections::BTreeSet;
use std::time::Duration;

use common::board::BoardClient;
use common::*;
use fleetdeck_proto::link::{Scope, StationInfo};
use serde_json::{Value, json};
use share_support::{Headers, expect, join, owner, refs, setup};

const DAY: i64 = 24 * 60 * 60 * 1000;

fn info(allow_shell: Option<bool>) -> StationInfo {
    StationInfo {
        name: "fake".to_owned(),
        incarnation: "fake-station-db".to_owned().into(),
        head: 0,
        live_sessions: 0,
        version: None,
        max_sessions: Some(4),
        allow_shell,
        allow_setup_cmd: None,
        providers: Vec::new(),
        assist_tokens_today: None,
        assist_cap: None,
        resources: None,
        desktop: None,
        traits: None,
        config: None,
        taildrive: None,
        fleet_git: None,
    }
}

fn shell_body(id: &str) -> Value {
    json!({
        "station": "fake",
        "command_id": id,
        "command": { "op": "shell_command", "session_id": "s1", "command": "pwd" },
    })
}

fn release_body(id: &str) -> Value {
    json!({
        "station": "fake",
        "command_id": id,
        "command": { "op": "release_shell", "session_id": "s1", "effect_id": "e1" },
    })
}

const SHELL: &str = "/api/commands/session.shell_command";
const RELEASE: &str = "/api/commands/session.release_shell";

async fn token(c: &BoardClient, scopes: &[Scope]) -> Headers {
    let (_, t) = c
        .store
        .create_token(
            "ops".into(),
            scopes.iter().copied().collect::<BTreeSet<_>>(),
            DAY,
        )
        .await
        .unwrap();
    vec![("Authorization".to_owned(), format!("Bearer {}", t.expose()))]
}

/// A member of a share on session `s1` that asks for the collaborator role.
async fn member(c: &BoardClient, h: &Headers, station: &str) -> Headers {
    let (m, _, _) = join(
        c,
        h,
        json!({ "kind": "session", "target": format!("{station}/s1"), "role": "collaborator" }),
        "Ann",
    )
    .await;
    m
}

#[tokio::test]
async fn the_owner_forwards_a_shell_command_and_a_release() {
    let (f, mut station, c, h) = setup().await;
    f.store.set_station_info(&f.station, info(Some(true)));
    let hh = refs(&h);
    let call = c.post(SHELL, &hh, shell_body("sh1"));
    let (r, req) = tokio::join!(call, Fixture::answer(&mut station, accepted));
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(req.method, "session.shell_command");
    assert_eq!(req.params["command"]["command"], "pwd");

    let call = c.post(RELEASE, &hh, release_body("rl1"));
    let (r, req) = tokio::join!(call, Fixture::answer(&mut station, accepted));
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(req.method, "session.release_shell");
    assert_eq!(req.params["command"]["effect_id"], "e1");
}

#[tokio::test]
async fn a_member_is_refused_with_owner_only() {
    let (f, mut station, c, h) = setup().await;
    f.store.set_station_info(&f.station, info(Some(true)));
    let m = member(&c, &h, "fake").await;
    expect(
        &c.post(SHELL, &refs(&m), shell_body("m1")).await,
        403,
        "owner_only",
    );
    expect(
        &c.post(RELEASE, &refs(&m), release_body("m2")).await,
        403,
        "owner_only",
    );
    assert!(f.store.commands().await.unwrap().is_empty());
    no_request(&mut station, Duration::from_millis(300)).await;
}

#[tokio::test]
async fn a_token_is_refused_also_with_the_admin_scope() {
    let (f, mut station, c, _h) = setup().await;
    f.store.set_station_info(&f.station, info(Some(true)));
    let admin = token(&c, &[Scope::Read, Scope::Drive, Scope::Admin]).await;
    for (n, (path, body)) in [(SHELL, shell_body("t1")), (RELEASE, release_body("t2"))]
        .into_iter()
        .enumerate()
    {
        let r = c.post(path, &refs(&admin), body).await;
        assert_eq!((r.status, r.code()), (403, "owner_only"), "{n}: {}", r.body);
    }
    assert!(f.store.commands().await.unwrap().is_empty());
    no_request(&mut station, Duration::from_millis(300)).await;
}

#[tokio::test]
async fn shells_off_or_unknown_refuse_a_shell_command_but_not_a_release() {
    let (f, mut station, c, _h) = setup().await;
    let h = owner(&c).await;
    // No `station.info` yet, then an old station (absent), then off.
    for (n, kept) in [None, Some(info(None)), Some(info(Some(false)))]
        .into_iter()
        .enumerate()
    {
        if let Some(kept) = kept {
            f.store.set_station_info(&f.station, kept);
        }
        let r = c.post(SHELL, &refs(&h), shell_body(&format!("n{n}"))).await;
        assert_eq!(
            (r.status, r.code()),
            (409, "shell_not_allowed"),
            "{n}: {}",
            r.body
        );
        assert_eq!(r.body["message"], "Shells are off on this station.");
    }
    assert!(f.store.commands().await.unwrap().is_empty());
    no_request(&mut station, Duration::from_millis(300)).await;

    // A release of a folder does not need `allow_shell`.
    let hh = refs(&h);
    let call = c.post(RELEASE, &hh, release_body("rl2"));
    let (r, req) = tokio::join!(call, Fixture::answer(&mut station, accepted));
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(req.method, "session.release_shell");
}
