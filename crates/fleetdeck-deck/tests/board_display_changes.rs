//! The change summary route of a wall display over loopback HTTPS (R13 P2b,
//! §5A item 5, §5C item 3, §5D items 13 and 14, R11): the checks and their
//! order, `show_diffs`, the station call, the join and the busy rule, the
//! fixed error texts and the answer that the deck repairs.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
mod displays_support;
mod share_support;

use std::collections::BTreeSet;
use std::time::Duration;

use common::board::{BoardClient, Http};
use common::{Fixture, next_request, no_request};
use displays_support::*;
use fleetdeck_core::Secret;
use fleetdeck_core::identity::Identity;
use fleetdeck_deck::store::displays::DisplayChange;
use fleetdeck_deck::store::stations::NewStation;
use fleetdeck_link::LiveLink;
use fleetdeck_proto::IncarnationId;
use fleetdeck_proto::display_changes::{DisplayChanges, DisplayFileChange};
use fleetdeck_proto::event::DiffSummary;
use fleetdeck_proto::files::DiffScope;
use fleetdeck_proto::link::{Outcome, Scope, WireError};
use fleetdeck_proto::session::RepoInfo;
use serde_json::{Value, json};
use share_support::*;

const LEAK: &str = "SENTINEL-LEAK-4711";

struct Rig {
    f: Fixture,
    station: LiveLink,
    c: BoardClient,
    h: Headers,
    st: String,
    display: String,
    cookie: String,
}

fn repo_card(id: &str, root: &str) -> fleetdeck_proto::SessionSummary {
    let mut s = card(id);
    s.repo = Some(RepoInfo {
        root: root.to_owned(),
        origin: None,
    });
    s
}

/// `s1` (repo alpha) and `s2` (repo beta) on the fixture station, and a
/// display with that station in its ceiling.
async fn rig() -> Rig {
    let (f, station, c, _) = setup().await;
    c.store
        .set_password(Secret::new(PASSWORD.to_owned()))
        .await
        .unwrap();
    let h = owner(&c).await;
    let st = f.station.0.clone();
    f.store
        .apply_snapshot(
            f.station.clone(),
            f.inc.clone(),
            0,
            vec![
                repo_card("s1", "/srv/a/alpha"),
                repo_card("s2", "/srv/b/beta"),
            ],
        )
        .await
        .unwrap();
    let (display, cookie) = display_login(&c, "Wall", &[&st]).await;
    Rig {
        f,
        station,
        c,
        h,
        st,
        display,
        cookie,
    }
}

fn file(path: &str, added: u64, removed: u64, patch: Option<&str>) -> DisplayFileChange {
    DisplayFileChange {
        path: Some(path.to_owned()),
        hidden: false,
        added: Some(added),
        removed: Some(removed),
        binary: false,
        patch: patch.map(str::to_owned),
        too_large: false,
    }
}

fn changes(files: Vec<DisplayFileChange>) -> DisplayChanges {
    DisplayChanges {
        session_id: "s1".into(),
        scope: DiffScope::Worktree,
        truncated: false,
        summary: DiffSummary {
            files_changed: u32::try_from(files.len()).unwrap(),
            insertions: 2,
            deletions: 0,
        },
        files,
    }
}

fn ok(c: &DisplayChanges) -> Outcome {
    Outcome::Ok(serde_json::to_value(c).unwrap())
}

impl Rig {
    fn path(&self, station: &str, session: &str, query: &str) -> String {
        format!("/api/displays/self/sessions/{station}/{session}/changes{query}")
    }

    async fn get(&self, session: &str, query: &str) -> Http {
        let path = self.path(&self.st, session, query);
        get_with(&self.c, &path, &self.cookie).await
    }

    /// One request of the display and the answer of the fake station.
    async fn exchange(&mut self, session: &str, query: &str, reply: Outcome) -> (Http, Value) {
        let path = self.path(&self.st, session, query);
        let (c, cookie) = (&self.c, self.cookie.clone());
        let station = &mut self.station;
        let answer = async {
            let req = next_request(station).await;
            assert_eq!(req.method, "files.display_changes");
            station.control.respond(req.id, reply).await.unwrap();
            req.params
        };
        tokio::join!(get_with(c, &path, &cookie), answer)
    }

    async fn show_diffs(&self, on: bool) {
        self.c
            .store
            .display_update(
                self.display.clone(),
                DisplayChange {
                    show_diffs: Some(on),
                    ..DisplayChange::default()
                },
            )
            .await
            .unwrap();
    }

    async fn quiet(&mut self) {
        no_request(&mut self.station, Duration::from_millis(300)).await;
    }
}

#[tokio::test]
async fn a_summary_needs_no_switch_and_carries_no_store_header() {
    let mut r = rig().await;
    let sent = changes(vec![file("a.txt", 2, 0, None)]);
    let (res, params) = r.exchange("s1", "", ok(&sent)).await;
    assert_eq!(res.status, 200, "{}", res.body);
    assert_eq!(res.header("cache-control"), Some("no-store"));
    assert_eq!(res.body["files"][0]["path"], "a.txt");
    assert_eq!(res.body["summary"]["files_changed"], 1);
    // The station gets the session, no patch and no cap of the deck.
    assert_eq!(params["session_id"], "s1");
    assert_eq!(params["patch"], false);
    assert!(params.get("member").is_none());
}

#[tokio::test]
async fn a_patch_needs_show_diffs_read_live() {
    let mut r = rig().await;
    let res = r.get("s1", "?patch=1").await;
    assert_eq!(res.status, 403, "{}", res.body);
    assert_eq!(res.code(), "diffs_off", "{}", res.body);
    r.quiet().await;
    r.show_diffs(true).await;
    let sent = changes(vec![file(
        "a.txt",
        2,
        0,
        Some("diff --git a/a.txt b/a.txt\n+x\n"),
    )]);
    let (res, params) = r.exchange("s1", "?patch=1", ok(&sent)).await;
    assert_eq!(res.status, 200, "{}", res.body);
    assert_eq!(params["patch"], true);
    assert!(
        res.body["files"][0]["patch"]
            .as_str()
            .unwrap()
            .contains("+x")
    );
    // `patch=0` stays a summary with the switch on.
    let (res, params) = r.exchange("s1", "?patch=0", ok(&changes(vec![]))).await;
    assert_eq!(res.status, 200, "{}", res.body);
    assert_eq!(params["patch"], false);
}

#[tokio::test]
async fn the_switch_is_checked_again_after_the_station_answer() {
    let mut r = rig().await;
    r.show_diffs(true).await;
    let path = r.path(&r.st, "s1", "?patch=1");
    let sent = changes(vec![file(
        "a.txt",
        2,
        0,
        Some("diff --git a/a.txt b/a.txt\n+x\n"),
    )]);
    let (c, cookie, store, display) =
        (&r.c, r.cookie.clone(), r.c.store.clone(), r.display.clone());
    let station = &mut r.station;
    let answer = async {
        let req = next_request(station).await;
        // The owner turns the switch off while the station works.
        store
            .display_update(
                display,
                DisplayChange {
                    show_diffs: Some(false),
                    ..DisplayChange::default()
                },
            )
            .await
            .unwrap();
        station.control.respond(req.id, ok(&sent)).await.unwrap();
    };
    let (res, ()) = tokio::join!(get_with(c, &path, &cookie), answer);
    assert_eq!(res.status, 403, "{}", res.body);
    assert!(!res.body.to_string().contains("a.txt"), "{}", res.body);
}

#[tokio::test]
async fn every_cause_of_a_missing_session_gives_the_same_404() {
    let mut r = rig().await;
    // A station outside the ceiling with a session of the same name.
    let rec =
        r.c.store
            .pin_station(NewStation {
                key: Identity::generate().unwrap().public_key(),
                name: "other".to_owned(),
                scopes: BTreeSet::from([Scope::Read]),
                dial_url: None,
            })
            .await
            .unwrap();
    r.c.store
        .apply_snapshot(
            rec.station_id.clone(),
            IncarnationId::from("i2"),
            0,
            vec![repo_card("o1", "/srv/a/alpha")],
        )
        .await
        .unwrap();
    let outside = get_with(&r.c, &r.path(&rec.station_id.0, "o1", ""), &r.cookie).await;
    let unknown = r.get("nope", "").await;
    let by_name = get_with(&r.c, &r.path("other", "s1", ""), &r.cookie).await;
    // The directive filter narrows the scope to the repository `alpha`.
    let d = post(
        &r.c,
        &r.h,
        &format!("/api/displays/{}/directive", r.display),
        json!({ "mode": "mission", "filter": { "stations": [r.st], "repo": "alpha" } }),
    )
    .await;
    assert_eq!(d.status, 200, "{}", d.body);
    let filtered = r.get("s2", "").await;
    for res in [&outside, &unknown, &by_name, &filtered] {
        assert_eq!(res.status, 404, "{}", res.body);
        assert_eq!(res.code(), "unknown_session");
    }
    assert_eq!(outside.body, unknown.body);
    assert_eq!(unknown.body, filtered.body);
    assert_eq!(unknown.body, by_name.body);
    // The inside session still works.
    let (res, _) = r.exchange("s1", "", ok(&changes(vec![]))).await;
    assert_eq!(res.status, 200, "{}", res.body);
    r.quiet().await;
}

#[tokio::test]
async fn a_bad_patch_value_a_revoked_display_and_an_owner_login_get_no_answer() {
    let mut r = rig().await;
    for bad in ["?patch=2", "?patch=true", "?patch="] {
        let res = r.get("s1", bad).await;
        assert_eq!(res.status, 400, "{bad}: {}", res.body);
        assert_eq!(res.code(), "invalid_query");
    }
    // The owner login is not a display login.
    let path = r.path(&r.st, "s1", "");
    let res = get(&r.c, &r.h, &path).await;
    assert_ne!(res.status, 200, "{}", res.body);
    r.c.store.display_revoke(r.display.clone()).await.unwrap();
    let res = r.get("s1", "").await;
    assert_eq!(res.status, 401, "{}", res.body);
    r.quiet().await;
}

#[tokio::test]
async fn the_deck_repairs_an_answer_that_leaks_a_hidden_path_or_an_unasked_patch() {
    let mut r = rig().await;
    let mut hidden = file(&format!("{LEAK}/.env"), 3, 1, Some(LEAK));
    hidden.hidden = true;
    hidden.binary = true;
    hidden.too_large = true;
    let sent = changes(vec![
        file("a.txt", 2, 0, Some(LEAK)),
        hidden,
        file("big.txt", 9, 0, Some(&"x".repeat(70 * 1024))),
    ]);
    // No patch asked: no patch leaves the deck.
    let (res, _) = r.exchange("s1", "", ok(&sent)).await;
    assert_eq!(res.status, 200, "{}", res.body);
    assert!(!res.body.to_string().contains(LEAK), "{}", res.body);
    let files = res.body["files"].as_array().unwrap();
    assert!(
        files
            .iter()
            .all(|f| f["patch"].is_null() && f["too_large"] == false)
    );
    assert_eq!(files[1]["hidden"], true);
    assert!(files[1]["path"].is_null() && files[1]["added"].is_null());
    // A patch asked: the section above 64 KiB is left out and marked.
    r.show_diffs(true).await;
    let mut sent = sent;
    sent.files[1].patch = None;
    sent.files[0].patch = Some("diff --git a/a.txt b/a.txt\n".to_owned());
    let (res, _) = r.exchange("s1", "?patch=1", ok(&sent)).await;
    assert_eq!(res.status, 200, "{}", res.body);
    let files = res.body["files"].as_array().unwrap();
    assert!(
        files[0]["patch"]
            .as_str()
            .unwrap()
            .starts_with("diff --git")
    );
    assert!(files[2]["patch"].is_null() && files[2]["too_large"] == true);
    assert!(files[1]["path"].is_null() && files[1]["too_large"] == false);
}

#[tokio::test]
async fn identical_requests_join_one_station_call() {
    let mut r = rig().await;
    let path = r.path(&r.st, "s1", "");
    let (c, cookie) = (&r.c, r.cookie.clone());
    let station = &mut r.station;
    let answer = async {
        let req = next_request(station).await;
        // Both requests are inside before the answer.
        tokio::time::sleep(Duration::from_millis(400)).await;
        station
            .control
            .respond(req.id, ok(&changes(vec![file("a.txt", 2, 0, None)])))
            .await
            .unwrap();
    };
    let (a, b, ()) = tokio::join!(
        get_with(c, &path, &cookie),
        get_with(c, &path, &cookie),
        answer
    );
    assert_eq!((a.status, b.status), (200, 200), "{} {}", a.body, b.body);
    assert_eq!(a.body, b.body);
    // One station call only.
    r.quiet().await;
}

#[tokio::test]
async fn a_different_computation_of_a_busy_display_gets_429_and_the_place_is_freed() {
    let mut r = rig().await;
    r.show_diffs(true).await;
    let slow = r.path(&r.st, "s1", "");
    let other = r.path(&r.st, "s1", "?patch=1");
    let (c, cookie) = (&r.c, r.cookie.clone());
    let station = &mut r.station;
    let answer = async {
        let req = next_request(station).await;
        tokio::time::sleep(Duration::from_millis(500)).await;
        station
            .control
            .respond(req.id, ok(&changes(vec![])))
            .await
            .unwrap();
    };
    let second = async {
        tokio::time::sleep(Duration::from_millis(200)).await;
        get_with(c, &other, &cookie).await
    };
    let (first, busy, ()) = tokio::join!(get_with(c, &slow, &cookie), second, answer);
    assert_eq!(first.status, 200, "{}", first.body);
    assert_eq!(busy.status, 429, "{}", busy.body);
    assert_eq!(busy.code(), "busy");
    // The place is free again.
    let (res, _) = r.exchange("s1", "?patch=1", ok(&changes(vec![]))).await;
    assert_eq!(res.status, 200, "{}", res.body);
    r.quiet().await;
}

#[tokio::test]
async fn a_station_error_gets_a_fixed_text_without_the_station_message() {
    let mut r = rig().await;
    let cases = [
        ("no_base", 409, "no_base"),
        ("no_diff", 404, "no_diff"),
        ("unknown_session", 404, "unknown_session"),
        ("busy", 429, "busy"),
        ("timeout", 504, "timeout"),
        ("feature_not_negotiated", 409, "changes_unsupported"),
        ("outside_roots", 502, "station_error"),
        ("internal", 502, "station_error"),
    ];
    for (station_code, status, code) in cases {
        let reply = Outcome::Err(WireError::new(
            station_code,
            format!("{LEAK} {station_code}"),
        ));
        let (res, _) = r.exchange("s1", "", reply).await;
        assert_eq!(res.status, status, "{station_code}: {}", res.body);
        assert_eq!(res.code(), code, "{station_code}");
        assert!(!res.body.to_string().contains(LEAK), "{}", res.body);
    }
    // An answer that is not a change list.
    let (res, _) = r
        .exchange("s1", "", Outcome::Ok(json!({ "nope": LEAK })))
        .await;
    assert_eq!(res.status, 502, "{}", res.body);
    assert!(!res.body.to_string().contains(LEAK), "{}", res.body);
}

#[tokio::test]
async fn a_station_without_a_link_gives_409() {
    let r = rig().await;
    let Rig {
        f,
        station,
        c,
        cookie,
        st,
        ..
    } = r;
    f.drop_link(station).await;
    let path = format!("/api/displays/self/sessions/{st}/s1/changes");
    let res = get_with(&c, &path, &cookie).await;
    assert_eq!(res.status, 409, "{}", res.body);
    assert_eq!(res.code(), "station_unreachable");
}

#[tokio::test]
async fn a_scope_that_shrinks_during_the_station_call_refuses_the_answer() {
    let mut r = rig().await;
    let rec =
        r.c.store
            .pin_station(NewStation {
                key: Identity::generate().unwrap().public_key(),
                name: "other".to_owned(),
                scopes: BTreeSet::from([Scope::Read]),
                dial_url: None,
            })
            .await
            .unwrap();
    let path = r.path(&r.st, "s1", "");
    let (c, cookie, store, display) =
        (&r.c, r.cookie.clone(), r.c.store.clone(), r.display.clone());
    let station = &mut r.station;
    let answer = async {
        let req = next_request(station).await;
        // The ceiling moves to another station while the station works.
        store
            .display_update(
                display,
                DisplayChange {
                    ceiling: Some(vec![rec.station_id.clone()]),
                    ..DisplayChange::default()
                },
            )
            .await
            .unwrap();
        station
            .control
            .respond(req.id, ok(&changes(vec![file("a.txt", 2, 0, None)])))
            .await
            .unwrap();
    };
    let (res, ()) = tokio::join!(get_with(c, &path, &cookie), answer);
    assert_eq!(res.status, 404, "{}", res.body);
    assert!(!res.body.to_string().contains("a.txt"), "{}", res.body);
}
