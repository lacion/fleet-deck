//! The answers of a fleet-control coordinator (ADR-008 §5.3 "Owner
//! visibility") over loopback HTTPS: `GET
//! /api/sessions/{station}/{session}/fleet-answers` lists the last 50
//! `coordinator.answer` audit rows of the coordinator session, counts the
//! successful answers of the last hour and shows the cap of its grant. Only
//! the owner reads it, and a read writes no audit row.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use common::board::{BoardClient, Http, board};
use common::*;
use fleetdeck_deck::store::{AuditResult, GrantClass, NewAudit, NewGrant};
use fleetdeck_proto::SessionSummary;
use fleetdeck_proto::command::GrantScope;
use fleetdeck_proto::link::Scope;
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
    c: BoardClient,
    h: Headers,
}

async fn setup() -> T {
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
    let h = owner(&c).await;
    T { f, c, h }
}

const COORD: &str = "coord-1";

/// An audit row of a `coordinator.answer` call of `session`.
async fn answered(t: &T, session: &str, result: AuditResult, detail: &str) {
    let actor = format!("coordinator:{}/{session}", t.f.station.0);
    let row = NewAudit::new(actor, "coordinator.answer", result)
        .target(format!("{}/w1", t.f.station.0))
        .detail(detail);
    t.f.store.audit_or_log(row).await;
}

async fn grant(t: &T, session: &str, classes: &[GrantClass], fleet: bool, cap: Option<u32>) {
    let mut g = NewGrant::new(t.f.station.clone(), session.into());
    g.classes = classes.iter().copied().collect();
    if fleet {
        g.scope = GrantScope::Fleet;
    }
    g.answer_cap = cap;
    t.f.store.create_grant(g).await.unwrap();
}

fn path(station: &str, session: &str) -> String {
    format!("/api/sessions/{station}/{session}/fleet-answers")
}

#[tokio::test]
async fn a_fleet_control_grant_lists_its_answers_newest_first_and_counts_the_last_hour() {
    let t = setup().await;
    grant(
        &t,
        COORD,
        &[GrantClass::Read, GrantClass::Answer],
        true,
        Some(7),
    )
    .await;
    // An older answer, more than one hour before the two others.
    answered(&t, COORD, AuditResult::Ok, "grant=old").await;
    t.f.det.advance(Duration::from_secs(2 * 3600));
    answered(&t, COORD, AuditResult::Ok, "grant=g1").await;
    t.f.det.advance(Duration::from_secs(60));
    answered(
        &t,
        COORD,
        AuditResult::Refused,
        "grant=g1 reject=answer_cap_reached",
    )
    .await;
    // Another coordinator and another op do not count.
    answered(&t, "coord-2", AuditResult::Ok, "grant=other").await;
    let other = NewAudit::new(
        format!("coordinator:{}/{COORD}", t.f.station.0),
        "coordinator.mail",
        AuditResult::Ok,
    );
    t.f.store.audit_or_log(other).await;

    let r = t.c.get(&path("fake", COORD), &refs(&t.h)).await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body["fleet_control"], true);
    assert_eq!(r.body["answer_cap"], 7);
    assert_eq!(r.body["answer_used"], 0);
    assert_eq!(r.body["last_hour"], 1);
    let rows = r.body["answers"].as_array().unwrap();
    let got: Vec<(&str, &str)> = rows
        .iter()
        .map(|a| (a["result"].as_str().unwrap(), a["detail"].as_str().unwrap()))
        .collect();
    assert_eq!(
        got,
        [
            ("refused", "grant=g1 reject=answer_cap_reached"),
            ("ok", "grant=g1"),
            ("ok", "grant=old"),
        ]
    );
    assert_eq!(rows[0]["target"], json!(format!("{}/w1", t.f.station.0)));
    assert!(rows[0]["at"].as_i64().unwrap() > rows[1]["at"].as_i64().unwrap());
    // The station id works as the station segment too, and a read writes no row.
    let before = t.f.store.audit_log(100).await.unwrap().len();
    let by_id = path(&t.f.station.0, COORD);
    let r = t.c.get(&by_id, &refs(&t.h)).await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body["last_hour"], 1);
    assert_eq!(t.f.store.audit_log(100).await.unwrap().len(), before);
}

#[tokio::test]
async fn the_list_holds_the_last_50_answers() {
    let t = setup().await;
    grant(&t, COORD, &[GrantClass::Answer], true, None).await;
    for i in 0..55 {
        answered(&t, COORD, AuditResult::Ok, &format!("n={i}")).await;
    }
    let r = t.c.get(&path("fake", COORD), &refs(&t.h)).await;
    assert_eq!(r.status, 200, "{}", r.body);
    let rows = r.body["answers"].as_array().unwrap();
    assert_eq!(rows.len(), 50);
    assert_eq!(rows[0]["detail"], "n=54");
    assert_eq!(r.body["last_hour"], 55);
}

#[tokio::test]
async fn a_lineage_grant_without_fleet_classes_is_not_fleet_control() {
    let t = setup().await;
    grant(
        &t,
        COORD,
        &[GrantClass::Read, GrantClass::Mail],
        false,
        None,
    )
    .await;
    let r = t.c.get(&path("fake", COORD), &refs(&t.h)).await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body["fleet_control"], false);
    assert_eq!(r.body["answer_cap"], Value::Null);
    assert_eq!(r.body["answer_used"], 0);
    assert_eq!(r.body["last_hour"], 0);
    assert_eq!(r.body["answers"], json!([]));
}

#[tokio::test]
async fn a_session_without_a_grant_is_not_fleet_control_and_has_no_rows() {
    let t = setup().await;
    let r = t.c.get(&path("fake", "nobody"), &refs(&t.h)).await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(
        r.body,
        json!({"fleet_control": false, "answer_cap": null, "answer_used": 0,
            "last_hour": 0, "answers": []})
    );
}

#[tokio::test]
async fn an_old_grant_that_answered_still_shows_its_rows() {
    let t = setup().await;
    answered(&t, COORD, AuditResult::Ok, "grant=gone").await;
    let r = t.c.get(&path("fake", COORD), &refs(&t.h)).await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body["fleet_control"], false);
    assert_eq!(r.body["answers"].as_array().unwrap().len(), 1);
    assert_eq!(r.body["last_hour"], 1);
}

#[tokio::test]
async fn a_member_and_a_token_get_owner_only() {
    let t = setup().await;
    let m = member(&t.c, &t.h, "fake").await;
    let admin = token(&t.c, &[Scope::Read, Scope::Drive, Scope::Admin]).await;
    for who in [&m, &admin] {
        let r = t.c.get(&path("fake", COORD), &refs(who)).await;
        expect(&r, 403, "owner_only");
    }
}

#[tokio::test]
async fn an_unknown_station_is_404() {
    let t = setup().await;
    let r = t.c.get(&path("nope", COORD), &refs(&t.h)).await;
    expect(&r, 404, "unknown_station");
}
