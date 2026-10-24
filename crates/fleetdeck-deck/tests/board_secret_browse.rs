//! The browse of a home for the secret files (Fleet Sync): `GET
//! /api/projects/{id}/secret-files/browse`. Owner's board login only, a ready
//! home, the negotiated feature, and the answer of the station passes through.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
mod share_support;

use std::sync::{Arc, Mutex};

use common::fleet_git_fake::{FakeGit, FakeOptions, Setup, bearer, expect, synced};
use fleetdeck_proto::link::{Outcome, Scope, WireError};
use serde_json::{Value, json};

fn url(s: &Setup, query: &str) -> String {
    format!(
        "/api/projects/{}/secret-files/browse{query}",
        s.app.project_id
    )
}

fn canned() -> Value {
    json!({
        "dir": "config",
        "entries": [
            { "name": ".hidden", "kind": "dir", "eligible": false },
            { "name": ".env", "kind": "file", "eligible": true, "size": 12 },
            { "name": "README.md", "kind": "file", "eligible": false,
              "reason": "not_member_hidden", "size": 3 }
        ],
        "truncated": false
    })
}

/// Makes the fake answer `project.secret_browse`: a `dir` that starts with
/// `err-` is refused with the rest as the code, `garbage` gets an answer that
/// does not parse, any other `dir` gets [`canned`].
fn install(fake: &FakeGit) -> Arc<Mutex<Vec<Value>>> {
    let seen: Arc<Mutex<Vec<Value>>> = Arc::default();
    let record = Arc::clone(&seen);
    *fake.secret.lock().unwrap() = Some(Box::new(move |method, p: &Value| {
        assert_eq!(method, "project.secret_browse");
        record.lock().unwrap().push(p.clone());
        let dir = p["dir"].as_str().unwrap_or_default();
        if let Some(code) = dir.strip_prefix("err-") {
            return Outcome::Err(WireError::new(code, "The station text."));
        }
        if dir == "garbage" {
            return Outcome::Ok(json!({ "nope": 1 }));
        }
        Outcome::Ok(canned())
    }));
    seen
}

#[tokio::test]
async fn the_answer_of_the_station_passes_through_and_dir_defaults_to_empty() {
    let (s, fake) = synced().await;
    let seen = install(&fake);
    let station = s.f.station.clone();
    let home = s.app().await.homes[0].clone();
    let dest = s.app().await.remote_dest.expect("a destination");
    let r =
        s.c.get(&url(&s, &format!("?station={}", station.as_str())), &s.h())
            .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body, canned());
    let r =
        s.c.get(
            &url(
                &s,
                &format!("?station={}&dir=config%2Fsub%20dir", station.as_str()),
            ),
            &s.h(),
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    let seen: Vec<Value> = seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 2);
    assert_eq!(
        seen[0],
        json!({
            "project_id": s.app.project_id,
            "generation": home.generation,
            "dest": dest,
            "remote_name": "origin",
            "dir": ""
        })
    );
    assert_eq!(seen[1]["dir"], "config/sub dir");
    // A read: no audit row.
    let rows = s.f.store.audit_log(100).await.unwrap();
    assert!(
        rows.iter().all(|row| !row.action.contains("secret_browse")),
        "{rows:?}"
    );
}

#[tokio::test]
async fn the_route_is_for_the_owner_login_only() {
    let (s, fake) = synced().await;
    let seen = install(&fake);
    let path = url(&s, &format!("?station={}", s.f.station.as_str()));
    let admin = bearer(&s.c, "admin", &[Scope::Read, Scope::Admin]).await;
    let admin = [(admin.0.as_str(), admin.1.as_str())];
    expect(&s.c.get(&path, &admin).await, 403, "owner_only");
    let owner = vec![
        ("Cookie".to_owned(), s.cookie.clone()),
        ("X-CSRF-Token".to_owned(), s.csrf.clone()),
    ];
    let (m, _, _) = share_support::join(
        &s.c,
        &owner,
        json!({ "kind": "board", "role": "approver" }),
        "Ann",
    )
    .await;
    let m = share_support::refs(&m);
    expect(&s.c.get(&path, &m).await, 403, "owner_only");
    assert_eq!(s.c.get(&path, &[]).await.status, 401);
    assert!(seen.lock().unwrap().is_empty());
}

#[tokio::test]
async fn the_project_and_the_station_are_checked() {
    let (s, fake) = synced().await;
    let seen = install(&fake);
    let r =
        s.c.get(
            &format!(
                "/api/projects/nope/secret-files/browse?station={}",
                s.f.station.as_str()
            ),
            &s.h(),
        )
        .await;
    expect(&r, 404, "unknown_project");
    for query in ["", "?station=", "?dir=config"] {
        let r = s.c.get(&url(&s, query), &s.h()).await;
        expect(&r, 400, "invalid_station");
        assert_eq!(r.body["message"], "Choose a station.", "{query}");
    }
    // A station id that is not a home of the project.
    let r = s.c.get(&url(&s, "?station=nope"), &s.h()).await;
    expect(&r, 409, "not_a_home");
    assert_eq!(
        r.body["message"],
        "This station has no ready home of the project."
    );
    assert!(seen.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_paired_station_without_a_home_is_not_a_home() {
    let (mut s, fake) = synced().await;
    let seen = install(&fake);
    let (other, _fake) = FakeGit::connect_second(&mut s.f, "beta", FakeOptions::default()).await;
    let r =
        s.c.get(&url(&s, &format!("?station={}", other.as_str())), &s.h())
            .await;
    expect(&r, 409, "not_a_home");
    assert!(seen.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_station_without_the_feature_is_refused_before_a_call() {
    let (s, fake) = synced().await;
    let seen = install(&fake);
    // A live link of the station that did not negotiate the feature.
    s.f.store
        .set_link_features(&s.f.station, 77, vec!["control.v1".to_owned()]);
    let r =
        s.c.get(
            &url(&s, &format!("?station={}", s.f.station.as_str())),
            &s.h(),
        )
        .await;
    expect(&r, 409, "fleet_git_unsupported");
    assert_eq!(
        r.body["message"],
        "Update this station to browse its files."
    );
    assert!(seen.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_refusal_of_the_station_gets_a_static_code_and_its_text() {
    let (s, fake) = synced().await;
    install(&fake);
    let station = s.f.station.as_str().to_owned();
    for (code, status, shown) in [
        ("invalid_path", 400, "invalid_path"),
        ("missing", 409, "missing"),
        ("symlink", 409, "symlink"),
        ("not_a_home", 409, "not_a_home"),
        ("stale_generation", 409, "not_a_home"),
        ("not_this_repo", 409, "not_this_repo"),
        ("unreadable", 409, "unreadable"),
    ] {
        let r =
            s.c.get(
                &url(&s, &format!("?station={station}&dir=err-{code}")),
                &s.h(),
            )
            .await;
        expect(&r, status, shown);
        assert_eq!(r.body["message"], "The station text.", "{code}");
    }
    for code in ["internal", "invalid_params", "something_new"] {
        let r =
            s.c.get(
                &url(&s, &format!("?station={station}&dir=err-{code}")),
                &s.h(),
            )
            .await;
        expect(&r, 502, "station_error");
        assert_eq!(r.body["message"], "The station refused the request.");
    }
}

#[tokio::test]
async fn an_answer_that_does_not_parse_is_station_unavailable() {
    let (s, fake) = synced().await;
    install(&fake);
    let r =
        s.c.get(
            &url(
                &s,
                &format!("?station={}&dir=garbage", s.f.station.as_str()),
            ),
            &s.h(),
        )
        .await;
    expect(&r, 502, "station_unavailable");
    assert_eq!(r.body["message"], "The station did not answer.");
}

#[test]
fn the_board_method_is_in_the_method_table() {
    use fleetdeck_deck::http::methods::api;
    let entry = api()
        .find(|m| m.name == "projects.secret_browse")
        .expect("projects.secret_browse");
    assert_eq!(entry.method.as_str(), "GET");
    assert_eq!(entry.path, "/api/projects/{project}/secret-files/browse");
    assert_eq!(entry.scope, Some(Scope::Admin));
    assert!(!entry.mutating);
    assert!(!entry.member);
}
