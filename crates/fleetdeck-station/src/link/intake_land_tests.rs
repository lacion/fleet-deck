//! `fleet_git.land_status`, `fleet_git.land_abandon` and "Push now" after a
//! land (FG-8, design §11.2, §11.7).

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use fleetdeck_proto::SessionId;
use fleetdeck_proto::fleet_git::{PushNowAnswer, PushState};
use fleetdeck_proto::link::Outcome;
use fleetdeck_testkit::Det;
use serde_json::json;

use super::{land_abandon, land_status, push_now};
use crate::integrations::Integrations;
use crate::manager::fleet_git_writer::{FleetGitWriter, LandLocks, LandTimeouts};
use crate::manager::land::{Deps, start};
use crate::manager::{Adapters, Policy, SessionManager};
use crate::store::git_state::tests::{new_git, new_session};
use crate::store::{OfflineApprovalPolicy, Store, StoreConfig};
use crate::workspace::GitOptions;
use crate::workspace::branches::parse_git_version;

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .current_dir(dir)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@example.com")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@example.com")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

async fn manager(root: &Path) -> SessionManager {
    let det = Det::new();
    let store = Store::open_in_memory(det.clock(), det.ids(), StoreConfig::default())
        .await
        .unwrap();
    let policy = Policy::new(
        vec![root.to_path_buf()],
        4,
        OfflineApprovalPolicy::Wait,
        BTreeMap::new(),
    );
    SessionManager::start(
        store,
        policy,
        Adapters::new(),
        det.ids(),
        Arc::new(crate::workspace::PrivatePaths::default()),
    )
    .await
    .unwrap()
}

fn engine(manager: &SessionManager) -> tokio::sync::watch::Sender<bool> {
    let (stop, rx) = tokio::sync::watch::channel(false);
    let locks = LandLocks::new();
    let timeouts = LandTimeouts::default();
    let hub = Arc::clone(manager.policy_hub());
    let options: crate::workspace::fleet_git::OptionsProvider = Arc::new(GitOptions::isolated);
    let (writer, _task) = FleetGitWriter::start_with(
        manager.store().clone(),
        Arc::clone(&hub),
        Arc::clone(&options),
        Arc::clone(&locks),
        timeouts,
    );
    let deps = Deps {
        writer,
        locks,
        integrations: Arc::new(|| Arc::new(Integrations::default())),
        timeouts,
    };
    let (engine, _tasks) = start(manager.store().clone(), hub, options, rx, deps);
    manager.set_land_engine(engine);
    stop
}

fn answer(outcome: Outcome) -> serde_json::Value {
    match outcome {
        Outcome::Ok(value) => value,
        Outcome::Err(e) => panic!("{e:?}"),
    }
}

#[tokio::test]
async fn the_land_methods_answer_fenced_unknown_and_a_protocol_error() {
    let dir = tempfile::tempdir().unwrap();
    let manager = manager(&dir.path().canonicalize().unwrap()).await;
    // Without the engine the abandonment cannot run.
    let Outcome::Err(e) = land_abandon(&manager, json!({ "land_id": "land-x" })).await else {
        panic!("internal expected");
    };
    assert_eq!(e.code, "internal");
    let _stop = engine(&manager);
    assert_eq!(
        answer(land_status(&manager, json!({ "land_id": "land-x" })).await),
        json!({ "state": "unknown" })
    );
    assert_eq!(
        answer(land_abandon(&manager, json!({ "land_id": "land-x" })).await),
        json!({ "state": "fenced", "write": "none", "run_state": "unresolved" })
    );
    assert_eq!(
        answer(land_status(&manager, json!({ "land_id": "land-x" })).await),
        json!({ "state": "fenced" })
    );
    for params in [json!({}), json!({ "land_id": 3 })] {
        let Outcome::Err(e) = land_status(&manager, params.clone()).await else {
            panic!("protocol error expected");
        };
        assert_eq!(e.code, "protocol_error");
        let Outcome::Err(e) = land_abandon(&manager, params).await else {
            panic!("protocol error expected");
        };
        assert_eq!(e.code, "protocol_error");
    }
}

#[tokio::test]
async fn push_now_after_a_land_waits_until_the_local_branch_contains_the_published_commit() {
    let out = Command::new("git").arg("--version").output().unwrap();
    if parse_git_version(&String::from_utf8_lossy(&out.stdout)).is_none_or(|v| v < (2, 45)) {
        eprintln!("skipped: Fleet Git needs Git 2.45 or newer");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let manager = manager(&root).await;
    let home = root.join("work");
    fs::create_dir(&home).unwrap();
    git(&home, &["init", "-b", "main"]);
    fs::write(home.join("a"), "1").unwrap();
    git(&home, &["add", "."]);
    git(&home, &["commit", "-m", "one"]);
    // The old local tip of the session branch; R is on a rebased line.
    git(&home, &["checkout", "-q", "-b", "fd/alpha/s1"]);
    fs::write(home.join("b"), "2").unwrap();
    git(&home, &["add", "."]);
    git(&home, &["commit", "-m", "old work"]);
    let old = git(&home, &["rev-parse", "HEAD"]);
    git(&home, &["checkout", "-q", "main", "-b", "rebased"]);
    fs::write(home.join("c"), "3").unwrap();
    git(&home, &["add", "."]);
    git(&home, &["commit", "-m", "rebased work"]);
    let published = git(&home, &["rev-parse", "HEAD"]);
    let store = manager.store();
    store.create_session(new_session("s1")).await.unwrap();
    let mut row = new_git("s1", "p1");
    row.repo_root = home.display().to_string();
    let push = Some((
        "origin".to_owned(),
        crate::store::git_state::tests::DEST.to_owned(),
    ));
    store.git_record_session(row, push).await.unwrap();
    let id = SessionId::from("s1");
    store
        .land_test_sql(
            "UPDATE git_pushes SET state = 'stopped', stop_code = 'land_published',
                                   last_pushed_oid = ?1 WHERE session_id = 's1'",
            vec![published.clone()],
        )
        .await
        .unwrap();
    let call = || push_now(&manager, json!({ "session_id": "s1" }));
    // The local tip does not contain R: the row stays stopped.
    let state = |v: serde_json::Value| serde_json::from_value::<PushNowAnswer>(v).unwrap().state;
    assert_eq!(state(answer(call().await)), PushState::Stopped);
    // The agent rebased onto R: Push now makes the row due with the lease R.
    git(&home, &["checkout", "-q", "fd/alpha/s1"]);
    git(&home, &["reset", "-q", "--hard", &published]);
    fs::write(home.join("d"), "4").unwrap();
    git(&home, &["add", "."]);
    git(&home, &["commit", "-m", "more"]);
    let tip = git(&home, &["rev-parse", "HEAD"]);
    assert_ne!(tip, old);
    assert_eq!(state(answer(call().await)), PushState::Due);
    let due = store.git_push(id).await.unwrap().unwrap();
    assert_eq!(due.desired_oid.as_deref(), Some(tip.as_str()));
    assert_eq!(due.last_pushed_oid.as_deref(), Some(published.as_str()));
    assert_eq!(due.stop_code, None);
}
