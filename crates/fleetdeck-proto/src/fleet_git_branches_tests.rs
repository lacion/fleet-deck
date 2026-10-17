use serde_json::json;

use super::*;
use crate::mutation::{CoordinatorOp, CoordinatorQuery};
use crate::version::feature;

fn branch(slug: Option<&str>) -> RemoteBranch {
    RemoteBranch {
        name: "fd/alpha/ada".into(),
        slug: slug.map(str::to_owned),
        callsign: "ada".into(),
        head: "a".repeat(40),
        ahead: 2,
        behind: 1,
        at: 1_767_225_600_000,
        subject: "fix: a thing".into(),
    }
}

#[test]
fn a_remote_branch_without_a_slug_round_trips_and_omits_the_slug() {
    let b = branch(None);
    let v = serde_json::to_value(&b).unwrap();
    assert!(v.get("slug").is_none(), "{v}");
    assert_eq!(serde_json::from_value::<RemoteBranch>(v).unwrap(), b);
    let with = serde_json::to_value(branch(Some("alpha"))).unwrap();
    assert_eq!(with["slug"], "alpha");
}

#[test]
fn remote_branches_round_trip_with_fetched_at_and_default_oid_absent() {
    let a = RemoteBranches {
        remote: "origin".into(),
        default_branch: "main".into(),
        default_oid: None,
        fetched_at: None,
        branches: vec![branch(Some("alpha")), branch(None)],
        truncated: false,
    };
    let v = serde_json::to_value(&a).unwrap();
    assert!(v.get("fetched_at").is_none() && v.get("default_oid").is_none());
    assert_eq!(serde_json::from_value::<RemoteBranches>(v).unwrap(), a);
    let full = RemoteBranches {
        default_oid: Some("b".repeat(40)),
        fetched_at: Some(5),
        truncated: true,
        ..a
    };
    let v = serde_json::to_value(&full).unwrap();
    assert_eq!(v["fetched_at"], 5);
    assert_eq!(serde_json::from_value::<RemoteBranches>(v).unwrap(), full);
}

#[test]
fn the_answers_accept_unknown_fields_of_a_newer_station() {
    let a: RemoteBranches = serde_json::from_value(json!({
        "remote": "origin", "default_branch": "main", "branches": [], "truncated": false,
        "newer": 1
    }))
    .unwrap();
    assert!(a.branches.is_empty());
    let f: FetchAnswer = serde_json::from_value(json!({"fetched_at": 1, "newer": true})).unwrap();
    assert_eq!(f.fetched_at, Some(1));
}

#[test]
fn a_fetch_answer_round_trips_with_and_without_a_code() {
    let ok = FetchAnswer {
        fetched_at: Some(9),
        code: None,
        text: None,
    };
    assert_eq!(serde_json::to_value(&ok).unwrap(), json!({"fetched_at": 9}));
    let bad = FetchAnswer {
        fetched_at: None,
        code: Some("fetch_failed".into()),
        text: Some("The fetch failed.".into()),
    };
    let v = serde_json::to_value(&bad).unwrap();
    assert_eq!(serde_json::from_value::<FetchAnswer>(v).unwrap(), bad);
}

#[test]
fn the_limits_are_the_design_values() {
    assert_eq!(REMOTE_BRANCHES_MAX, 200);
    assert_eq!(REMOTE_BRANCH_SUBJECT_MAX, 200);
}

#[test]
fn the_branches_feature_is_offered_with_its_exact_name() {
    assert_eq!(feature::FLEET_GIT_BRANCHES, "fleet_git.branches.v1");
    assert!(feature::ALL.contains(&feature::FLEET_GIT_BRANCHES));
}

#[test]
fn the_coordinator_variants_have_their_wire_names_and_refuse_unknown_fields() {
    let q: CoordinatorQuery =
        serde_json::from_value(json!({"query": "project_branches", "project_id": "p1"})).unwrap();
    assert_eq!(
        q,
        CoordinatorQuery::ProjectBranches {
            project_id: "p1".into()
        }
    );
    let q: CoordinatorQuery =
        serde_json::from_value(json!({"query": "project_fetch", "project_id": "p1"})).unwrap();
    assert_eq!(
        serde_json::to_value(&q).unwrap(),
        json!({"query": "project_fetch", "project_id": "p1"})
    );
    assert!(
        serde_json::from_value::<CoordinatorQuery>(
            json!({"query": "project_fetch", "project_id": "p1", "x": 1})
        )
        .is_err()
    );
    let s = "01J0000000000000000000000S";
    let op: CoordinatorOp =
        serde_json::from_value(json!({"op": "land", "station": "alpha", "session_id": s})).unwrap();
    assert!(matches!(op, CoordinatorOp::Land { .. }));
    assert_eq!(
        serde_json::to_value(&op).unwrap(),
        json!({"op": "land", "station": "alpha", "session_id": s})
    );
    // `move` became an op in T4.1b (FG-9); an unknown op is still refused.
    let unknown = json!({"op": "teleport", "station": "alpha", "session_id": s});
    assert!(serde_json::from_value::<CoordinatorOp>(unknown).is_err());
}
