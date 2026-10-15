//! The subagent log and the repository branches of a session over loopback
//! HTTPS (DL-191): the owner reads, a token and a member get 403
//! `owner_only`, the link feature check, the checks before the station, the
//! error mapping and the audit rows. The station is a fake that answers the
//! link methods itself.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
mod share_support;

use std::collections::BTreeSet;
use std::time::Duration;

use common::board::{BoardClient, Http};
use common::*;
use fleetdeck_deck::store::AuditResult;
use fleetdeck_link::LiveLink;
use fleetdeck_link::control::InboundRequest;
use fleetdeck_proto::link::{Outcome, Scope, WireError};
use serde_json::{Value, json};
use share_support::*;

const HEAD: &str = "0123456789abcdef0123456789abcdef01234567";

fn log_json() -> Value {
    json!({
        "items": [{"id": "i1", "at": 1000, "kind": "user", "text": "work", "tool": null}],
        "state": {"kind": "complete"},
    })
}

fn branches_json() -> Value {
    json!({
        "head": HEAD, "branch": "main", "truncated": false,
        "branches": [{"name": "feat", "head": HEAD, "ahead": 2, "behind": 0,
                      "at": 1700000000, "subject": "add"}],
    })
}

fn diff_json() -> Value {
    json!({
        "session_id": "s1", "base_oid": HEAD,
        "summary": {"files_changed": 1, "insertions": 2, "deletions": 0},
        "patch": "diff --git a/a b/a\n", "truncated": false, "scope": "committed",
    })
}

fn subagent_path(st: &str, session: &str, tool: &str) -> String {
    format!("/api/sessions/{st}/{session}/subagents/{tool}")
}

fn branches_path(st: &str, session: &str) -> String {
    format!("/api/sessions/{st}/{session}/branches")
}

fn diff_path(st: &str, session: &str, head: &str) -> String {
    format!("/api/sessions/{st}/{session}/branches/{head}/diff")
}

/// One GET that the fake station answers with `outcome`.
async fn read_with(
    c: &BoardClient,
    h: &Headers,
    station: &mut LiveLink,
    path: &str,
    outcome: Outcome,
) -> (Http, InboundRequest) {
    let call = get(c, h, path);
    let answer = async {
        let req = next_request(station).await;
        station.control.respond(req.id, outcome).await.unwrap();
        req
    };
    tokio::join!(call, answer)
}

async fn quiet(station: &mut LiveLink) {
    no_request(station, Duration::from_millis(300)).await;
}

#[tokio::test]
async fn the_owner_reads_the_subagent_log_the_branches_and_a_branch_diff() {
    let (f, mut station, c, h) = setup().await;
    let st = f.station.0.clone();

    let (r, req) = read_with(
        &c,
        &h,
        &mut station,
        &subagent_path(&st, "s1", "toolu_01AbC-x_y"),
        Outcome::Ok(log_json()),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body, log_json());
    assert_eq!(req.method, "session.subagent");
    assert_eq!(
        req.params,
        json!({ "session_id": "s1", "tool_use_id": "toolu_01AbC-x_y" })
    );

    let (r, req) = read_with(
        &c,
        &h,
        &mut station,
        &branches_path(&st, "s2"),
        Outcome::Ok(branches_json()),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body, branches_json());
    assert_eq!(req.method, "session.branches");
    assert_eq!(req.params, json!({ "session_id": "s2" }));

    let (r, req) = read_with(
        &c,
        &h,
        &mut station,
        &diff_path(&st, "s1", HEAD),
        Outcome::Ok(diff_json()),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body, diff_json());
    assert_eq!(req.method, "files.branch_diff");
    assert_eq!(req.params, json!({ "session_id": "s1", "head": HEAD }));
    // A 64-digit head is valid too.
    let long = "ab".repeat(32);
    let (r, req) = read_with(
        &c,
        &h,
        &mut station,
        &diff_path(&st, "s1", &long),
        Outcome::Ok(diff_json()),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(req.params["head"], long.as_str());
}

fn paths(st: &str) -> Vec<String> {
    vec![
        subagent_path(st, "s1", "toolu_1"),
        branches_path(st, "s1"),
        diff_path(st, "s1", HEAD),
    ]
}

#[tokio::test]
async fn a_token_gets_owner_only_on_all_three_routes() {
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
    for path in paths(&st) {
        let r = c.get(&path, &[("Authorization", auth.as_str())]).await;
        expect(&r, 403, "owner_only");
    }
    quiet(&mut station).await;
}

#[tokio::test]
async fn a_member_gets_owner_only_on_all_three_routes() {
    let (f, mut station, c, h) = setup().await;
    let st = f.station.0.clone();
    let share = json!({ "kind": "session", "target": format!("{st}/s1"), "role": "viewer" });
    let (member, _, _) = join(&c, &h, share, "Alice").await;
    for path in paths(&st) {
        let r = get(&c, &member, &path).await;
        expect(&r, 403, "owner_only");
    }
    quiet(&mut station).await;
}

#[tokio::test]
async fn a_station_without_the_feature_gets_409_without_a_forward() {
    let (f, mut station, c, h) = setup().await;
    let st = f.station.0.clone();
    f.store.set_link_features(&f.station, 98, vec![]);
    let text = "This station runs an older Fleet Deck. On that computer, install the new version, then run `fleetdeck service start`.";
    let r = get(&c, &h, &subagent_path(&st, "s1", "toolu_1")).await;
    expect(&r, 409, "subagent_unsupported");
    assert_eq!(r.body["message"], text);
    for path in [branches_path(&st, "s1"), diff_path(&st, "s1", HEAD)] {
        let r = get(&c, &h, &path).await;
        expect(&r, 409, "branches_unsupported");
        assert_eq!(r.body["message"], text);
    }
    quiet(&mut station).await;
}

#[tokio::test]
async fn station_errors_map_to_fixed_codes() {
    let (f, mut station, c, h) = setup().await;
    let st = f.station.0.clone();
    let cases = [
        ("unknown_session", 404, "unknown_session"),
        ("invalid_params", 400, "invalid_params"),
        ("busy", 503, "busy"),
        ("missing_workspace", 502, "station_error"),
        ("outside_roots", 502, "station_error"),
        ("boom", 502, "station_error"),
    ];
    for path in paths(&st) {
        for (wire, status, code) in cases {
            let err = WireError::new(wire, "STATION-TEXT-MUST-NOT-LEAK /secret/path");
            let (r, _) = read_with(&c, &h, &mut station, &path, Outcome::Err(err)).await;
            assert_eq!((r.status, r.code()), (status, code), "{wire}: {}", r.body);
            assert!(!r.text.contains("STATION-TEXT-MUST-NOT-LEAK"), "{}", r.text);
        }
    }
    // `git_too_old` has a text for the board.
    for path in [branches_path(&st, "s1"), diff_path(&st, "s1", HEAD)] {
        let err = WireError::new("git_too_old", "STATION-TEXT-MUST-NOT-LEAK");
        let (r, _) = read_with(&c, &h, &mut station, &path, Outcome::Err(err)).await;
        expect(&r, 409, "git_too_old");
        assert_eq!(
            r.body["message"],
            "Update Git on this station (2.45 or newer) to see the branches."
        );
    }
    // `not_a_repository`, `no_common_history` and `platform_unsupported` have fixed texts too.
    let fixed = [
        (
            "not_a_repository",
            "This session folder is not a Git repository.",
            vec![branches_path(&st, "s1"), diff_path(&st, "s1", HEAD)],
        ),
        (
            "no_common_history",
            "This branch has no history in common with the session.",
            vec![diff_path(&st, "s1", HEAD)],
        ),
        (
            "platform_unsupported",
            "The branch view needs a Linux station for now.",
            vec![branches_path(&st, "s1"), diff_path(&st, "s1", HEAD)],
        ),
    ];
    for (wire, text, routes) in fixed {
        for path in routes {
            let err = WireError::new(wire, "STATION-TEXT-MUST-NOT-LEAK");
            let (r, _) = read_with(&c, &h, &mut station, &path, Outcome::Err(err)).await;
            expect(&r, 409, wire);
            assert_eq!(r.body["message"], text);
        }
    }
    // An answer that is not the expected shape is a 502, never the station's text.
    for path in paths(&st) {
        let bad = Outcome::Ok(json!({ "items": "STATION-TEXT-MUST-NOT-LEAK" }));
        let (r, _) = read_with(&c, &h, &mut station, &path, bad).await;
        expect(&r, 502, "station_error");
        assert!(!r.text.contains("STATION-TEXT-MUST-NOT-LEAK"), "{}", r.text);
    }
}

#[tokio::test]
async fn bad_values_are_refused_before_the_station() {
    let (f, mut station, c, h) = setup().await;
    let st = f.station.0.clone();
    let long_tool = "t".repeat(129);
    let uris = [
        subagent_path(&st, "s1", "a%20b"),
        subagent_path(&st, "s1", "a.b"),
        subagent_path(&st, "s1", &long_tool),
        subagent_path(&st, &"s".repeat(129), "toolu_1"),
        branches_path(&st, &"s".repeat(129)),
        diff_path(&st, "s1", "HEAD"),
        diff_path(&st, "s1", "abc"),
        diff_path(&st, "s1", &"g".repeat(40)),
        diff_path(&st, "s1", &"a".repeat(41)),
        diff_path(&st, &"s".repeat(129), HEAD),
    ];
    for uri in uris {
        let r = get(&c, &h, &uri).await;
        expect(&r, 400, "invalid_params");
    }
    quiet(&mut station).await;
}

#[tokio::test]
async fn an_unknown_session_or_station_is_a_404_without_a_forward() {
    let (f, mut station, c, h) = setup().await;
    let st = f.station.0.clone();
    for path in [
        subagent_path(&st, "nope", "toolu_1"),
        branches_path(&st, "nope"),
        diff_path(&st, "nope", HEAD),
    ] {
        let r = get(&c, &h, &path).await;
        expect(&r, 404, "unknown_session");
    }
    let r = get(&c, &h, &branches_path("no-such-station", "s1")).await;
    expect(&r, 404, "unknown_station");
    quiet(&mut station).await;
}

#[tokio::test]
async fn the_audit_has_a_row_for_each_failure_and_none_for_an_owner_read() {
    let (f, mut station, c, h) = setup().await;
    let rows = |f: &Fixture| {
        let store = f.store.clone();
        async move {
            let mut rows = store.audit_log(100).await.unwrap();
            rows.retain(|r| {
                r.action.starts_with("sessions.sub") || r.action.starts_with("sessions.branch")
            });
            rows
        }
    };
    let (r, _) = read_with(
        &c,
        &h,
        &mut station,
        &branches_path("fake", "s1"),
        Outcome::Ok(branches_json()),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert!(rows(&f).await.is_empty());

    let err = WireError::new("boom", "STATION-TEXT-MUST-NOT-LEAK");
    let (r, _) = read_with(
        &c,
        &h,
        &mut station,
        &subagent_path("fake", "s1", "toolu_1"),
        Outcome::Err(err),
    )
    .await;
    expect(&r, 502, "station_error");
    let r = get(&c, &h, &diff_path("fake", "s1", "abc")).await;
    expect(&r, 400, "invalid_params");
    let r = get(&c, &h, &branches_path("fake", "nope")).await;
    expect(&r, 404, "unknown_session");

    let all = rows(&f).await;
    assert_eq!(all.len(), 3, "{all:?}");
    let find = |action: &str| all.iter().find(|r| r.action == action).unwrap();
    let failed = find("sessions.subagent");
    assert_eq!(failed.result, AuditResult::Failed);
    assert_eq!(failed.detail.as_deref(), Some("station_error"));
    assert_eq!(failed.actor, "owner");
    assert_eq!(failed.target.as_deref(), Some("fake/s1"));
    let refused = find("sessions.branch_diff");
    assert_eq!(refused.result, AuditResult::Refused);
    assert_eq!(refused.detail.as_deref(), Some("invalid_params"));
    let missing = find("sessions.branches");
    assert_eq!(missing.result, AuditResult::Refused);
    assert_eq!(missing.detail.as_deref(), Some("unknown_session"));
    assert!(!format!("{all:?}").contains("STATION-TEXT-MUST-NOT-LEAK"));
}
