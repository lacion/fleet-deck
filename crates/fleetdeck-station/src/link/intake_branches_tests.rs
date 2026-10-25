//! `project.branches` and `project.fetch` on the radar home (FG-10, design
//! §13.1, §13.3).

use std::fs;

use fleetdeck_proto::fleet_git::{FetchAnswer, RemoteBranches};
use fleetdeck_proto::link::Outcome;
use serde_json::json;

use super::tests::{Rig, commit, git, refused, runtime};
use super::{branches, fetch};

fn params() -> serde_json::Value {
    json!({ "project_id": "app" })
}

#[tokio::test]
async fn both_calls_answer_on_the_radar_home_only_and_name_their_refusals() {
    let Some(rig) = Rig::new().await else { return };
    let runtime = runtime();
    // The project is unknown here.
    let (code, _) = refused(branches(&rig.manager, &runtime, params()).await);
    assert_eq!(code, "not_synced");
    // This station is not the radar home.
    let home = rig.radar_home(false).await;
    for outcome in [
        branches(&rig.manager, &runtime, params()).await,
        fetch(&rig.manager, &runtime, params()).await,
    ] {
        let (code, text) = refused(outcome);
        assert_eq!(code, "not_synced");
        assert_eq!(text, "This station is not the radar home of this project.");
    }
    // The params are checked first.
    for outcome in [
        branches(&rig.manager, &runtime, json!({ "project_id": 7 })).await,
        fetch(&rig.manager, &runtime, json!({})).await,
    ] {
        assert_eq!(refused(outcome).0, "protocol_error");
    }
    // The home is not verified any more.
    let mut project = rig.radar_view(&home);
    project.radar_home = true;
    fs::remove_dir_all(home.join(".git")).unwrap();
    assert!(rig.set(2, vec![project]).await.stored);
    for outcome in [
        branches(&rig.manager, &runtime, params()).await,
        fetch(&rig.manager, &runtime, params()).await,
    ] {
        let (code, text) = refused(outcome);
        assert_eq!(code, "not_a_home");
        assert_eq!(text, "This folder is not a verified home of the project.");
    }
}

#[tokio::test]
async fn the_listing_has_no_fetch_and_the_fetch_answers_its_time() {
    let Some(rig) = Rig::new().await else { return };
    let runtime = runtime();
    let home = rig.radar_home(true).await;
    let Outcome::Ok(value) = branches(&rig.manager, &runtime, params()).await else {
        panic!("an answer expected");
    };
    let list: RemoteBranches = serde_json::from_value(value).unwrap();
    assert_eq!(list.remote, "origin");
    assert_eq!(list.default_branch, "main");
    assert_eq!(list.branches.len(), 1);
    assert_eq!(list.branches[0].name, "fd/alpha/one");
    assert_eq!((list.branches[0].ahead, list.branches[0].behind), (1, 0));
    assert_eq!(list.fetched_at, None);
    // A branch that only the remote has: the listing does not see it.
    let url = rig.root.join("app.git").display().to_string();
    git(&home, &["checkout", "-q", "-b", "two"]);
    commit(&home, "two");
    git(&home, &["push", &url, "two:refs/heads/fd/bravo/two"]);
    git(&home, &["checkout", "-q", "main"]);
    let Outcome::Ok(value) = branches(&rig.manager, &runtime, params()).await else {
        panic!("an answer expected");
    };
    let again: RemoteBranches = serde_json::from_value(value).unwrap();
    assert_eq!(again.branches.len(), 1);
    // The fetch brings it and sets the time.
    let Outcome::Ok(value) = fetch(&rig.manager, &runtime, params()).await else {
        panic!("an answer expected");
    };
    let answer: FetchAnswer = serde_json::from_value(value).unwrap();
    let at = answer.fetched_at.expect("a fetch time");
    assert_eq!((answer.code, answer.text), (None, None));
    let Outcome::Ok(value) = branches(&rig.manager, &runtime, params()).await else {
        panic!("an answer expected");
    };
    let after: RemoteBranches = serde_json::from_value(value).unwrap();
    assert_eq!(after.branches.len(), 2);
    assert_eq!(after.fetched_at, Some(at));
}

#[tokio::test]
async fn a_failed_fetch_is_an_answer_with_a_code_and_not_a_wire_error() {
    let Some(rig) = Rig::new().await else { return };
    let runtime = runtime();
    let home = rig.radar_home(true).await;
    let missing = rig.root.join("missing.git").display().to_string();
    git(&home, &["remote", "set-url", "origin", &missing]);
    let Outcome::Ok(value) = fetch(&rig.manager, &runtime, params()).await else {
        panic!("an answer expected");
    };
    let answer: FetchAnswer = serde_json::from_value(value).unwrap();
    assert_eq!(answer.fetched_at, None);
    assert_eq!(answer.code.as_deref(), Some("fetch_failed"));
    assert!(answer.text.unwrap().starts_with("The fetch failed: "));
}
