//! The previous run of a session (DL-182, `shim-resume.md` §4.2) over
//! loopback HTTPS: `GET /api/sessions/{station}/{session}/execution` asks
//! the station for `session.execution_check` (owner only), and
//! `session.reconcile_execution` goes through the generic command route
//! (owner only). The station is a fake that answers by hand.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::BTreeSet;
use std::sync::Arc;

use common::board::{BoardClient, Http, board};
use common::*;
use fleetdeck_deck::store::AuditResult;
use fleetdeck_link::LiveLink;
use fleetdeck_proto::SessionSummary;
use fleetdeck_proto::link::{Outcome, Scope, WireError};
use serde_json::{Value, json};

type Headers = Vec<(String, String)>;

const DAY: i64 = 24 * 60 * 60 * 1000;

fn refs(h: &Headers) -> Vec<(&str, &str)> {
    h.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect()
}

fn expect(r: &Http, status: u16, code: &str) {
    assert_eq!((r.status, r.code()), (status, code), "{}", r.body);
}

async fn owner(c: &BoardClient) -> Headers {
    let (cookie, csrf) = c.login().await;
    vec![
        ("Cookie".to_owned(), cookie),
        ("X-CSRF-Token".to_owned(), csrf),
    ]
}

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

/// A member of a share on session `s1`.
async fn member(c: &BoardClient, h: &Headers, station: &str) -> Headers {
    let r = c
        .post(
            "/api/shares",
            &refs(h),
            json!({ "kind": "session", "target": format!("{station}/s1"), "role": "collaborator" }),
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    let url = r.body["url"].as_str().unwrap();
    let join = url.split("#join=").nth(1).unwrap();
    let j = c
        .post(
            "/api/auth/join",
            &[],
            json!({ "token": join, "display_name": "Ann" }),
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
    vec![
        ("Cookie".to_owned(), cookie),
        (
            "X-CSRF-Token".to_owned(),
            j.body["csrf"].as_str().unwrap().to_owned(),
        ),
    ]
}

struct T {
    f: Fixture,
    station: LiveLink,
    c: BoardClient,
    h: Headers,
}

async fn setup() -> T {
    let mut f = fixture().await;
    let station = f.connect().await;
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
    let h = owner(&c).await;
    T { f, station, c, h }
}

fn check_json() -> Value {
    json!({
        "shim_id": "shim-1",
        "reason": "lost: the link broke",
        "same_boot": true,
        "identity_alive": false,
        "group_members": [{ "pid": 4242, "command": "node" }],
        "session_members": [],
        "escaped": [{ "pid": 77, "command": "sleep" }],
        "complete": true,
        "error": null,
    })
}

/// Answers the next request of the deck with `outcome`; returns the request.
async fn reply(
    station: &mut LiveLink,
    outcome: Outcome,
) -> fleetdeck_link::control::InboundRequest {
    let req = next_request(station).await;
    station.control.respond(req.id, outcome).await.unwrap();
    req
}

#[tokio::test]
async fn the_owner_gets_the_check_of_the_station_and_the_call_is_audited() {
    let mut t = setup().await;
    let path = "/api/sessions/fake/s1/execution";
    let hh = refs(&t.h);
    let call = t.c.get(path, &hh);
    let (r, req) = tokio::join!(call, reply(&mut t.station, Outcome::Ok(check_json())));
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body, check_json());
    assert_eq!(req.method, "session.execution_check");
    assert_eq!(req.params, json!({ "session_id": "s1" }));
    // The station id works as the station segment too.
    let by_id = format!("/api/sessions/{}/s1/execution", t.f.station.0);
    let hh = refs(&t.h);
    let call = t.c.get(&by_id, &hh);
    let (r, _) = tokio::join!(call, reply(&mut t.station, Outcome::Ok(check_json())));
    assert_eq!(r.status, 200, "{}", r.body);

    let rows = t.f.store.audit_log(20).await.unwrap();
    let ok: Vec<_> = rows
        .iter()
        .filter(|e| e.action == "session.execution_check" && e.result == AuditResult::Ok)
        .collect();
    assert_eq!(ok.len(), 2);
    assert_eq!(ok[0].actor, "owner");
    assert_eq!(
        ok[0].target.as_deref(),
        Some(format!("{}/s1", t.f.station.0).as_str())
    );
}

#[tokio::test]
async fn a_member_and_a_token_get_owner_only() {
    let mut t = setup().await;
    let m = member(&t.c, &t.h, "fake").await;
    let admin = token(&t.c, &[Scope::Read, Scope::Drive, Scope::Admin]).await;
    let path = "/api/sessions/fake/s1/execution";
    for who in [&m, &admin] {
        let r = t.c.get(path, &refs(who)).await;
        expect(&r, 403, "owner_only");
    }
    no_request(&mut t.station, std::time::Duration::from_millis(300)).await;
}

#[tokio::test]
async fn an_unknown_station_and_a_dead_link_and_a_station_refusal_are_reported() {
    let mut t = setup().await;
    expect(
        &t.c.get("/api/sessions/nope/s1/execution", &refs(&t.h))
            .await,
        404,
        "unknown_station",
    );
    // The station refuses: its codes keep their meaning.
    let path = "/api/sessions/fake/s1/execution";
    for (code, status, api) in [
        ("unknown_session", 404, "unknown_session"),
        ("busy", 429, "busy"),
        ("unknown_method", 502, "station_error"),
    ] {
        let hh = refs(&t.h);
        let call = t.c.get(path, &hh);
        let out = Outcome::Err(WireError::new(code, "no"));
        let (r, _) = tokio::join!(call, reply(&mut t.station, out));
        expect(&r, status, api);
    }
    // A station that sends something else than a check.
    let hh = refs(&t.h);
    let call = t.c.get(path, &hh);
    let (r, _) = tokio::join!(call, reply(&mut t.station, Outcome::Ok(json!({ "x": 1 }))));
    expect(&r, 502, "station_error");
    assert_eq!(
        r.body["message"],
        "the station sent an invalid execution check"
    );
    let failed =
        t.f.store
            .audit_log(50)
            .await
            .unwrap()
            .into_iter()
            .filter(|e| e.action == "session.execution_check" && e.result == AuditResult::Failed)
            .count();
    assert_eq!(failed, 4);

    // No live link.
    let station = t.station;
    t.f.drop_link(station).await;
    expect(
        &t.c.get(path, &refs(&t.h)).await,
        409,
        "station_unreachable",
    );
}

fn reconcile_body(id: &str) -> Value {
    json!({
        "station": "fake",
        "command_id": id,
        "command": { "op": "reconcile_execution", "session_id": "s1", "shim_id": "shim-1" },
    })
}

#[tokio::test]
async fn the_owner_reconciles_through_the_command_route_and_others_are_refused() {
    let mut t = setup().await;
    let path = "/api/commands/session.reconcile_execution";
    let hh = refs(&t.h);
    let call = t.c.post(path, &hh, reconcile_body("r1"));
    let (r, req) = tokio::join!(call, Fixture::answer(&mut t.station, accepted));
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body["receipt"]["state"], "accepted");
    assert_eq!(req.method, "session.reconcile_execution");
    assert_eq!(req.params["command"]["shim_id"], "shim-1");

    let m = member(&t.c, &t.h, "fake").await;
    let admin = token(&t.c, &[Scope::Read, Scope::Drive, Scope::Admin]).await;
    for (n, who) in [&m, &admin].into_iter().enumerate() {
        let r =
            t.c.post(path, &refs(who), reconcile_body(&format!("r-x{n}")))
                .await;
        expect(&r, 403, "owner_only");
    }
    assert_eq!(t.f.store.commands().await.unwrap().len(), 1);
    no_request(&mut t.station, std::time::Duration::from_millis(300)).await;
}
