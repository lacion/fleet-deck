//! `fleet_git.projects_set` and `fleet_git.push_now` (design §3.3, §5.8).

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use fleetdeck_proto::SessionId;
use fleetdeck_proto::fleet_git::{
    GitSnapshot, HomeCheckState, LocalRemotes, PushNowAnswer, PushState, SnapshotAck, StationHome,
    remote_identity,
};
use fleetdeck_proto::link::Outcome;
use fleetdeck_testkit::Det;

use super::{PauseGuard, projects_set, push_now};
use crate::manager::{Adapters, Policy, SessionManager};
use crate::store::git_state::tests::{new_git, new_session, view};
use crate::store::{OfflineApprovalPolicy, Store, StoreConfig};
use crate::workspace::branches::parse_git_version;

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .current_dir(dir)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

struct Rig {
    _dir: tempfile::TempDir,
    root: PathBuf,
    manager: SessionManager,
}

impl Rig {
    async fn new() -> Option<Self> {
        let out = Command::new("git").arg("--version").output().unwrap();
        if parse_git_version(&String::from_utf8_lossy(&out.stdout)).is_none_or(|v| v < (2, 45)) {
            eprintln!("skipped: Fleet Git needs Git 2.45 or newer");
            return None;
        }
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let det = Det::new();
        let store = Store::open_in_memory(det.clock(), det.ids(), StoreConfig::default())
            .await
            .unwrap();
        let policy = Policy::new(
            vec![root.clone()],
            4,
            OfflineApprovalPolicy::Wait,
            BTreeMap::new(),
        )
        .with_allow_local_remotes(true);
        let manager = SessionManager::start(
            store,
            policy,
            Adapters::new(),
            det.ids(),
            Arc::new(crate::workspace::PrivatePaths::default()),
        )
        .await
        .unwrap();
        Some(Self {
            _dir: dir,
            root,
            manager,
        })
    }

    /// A home whose `origin` is a new bare repository; its project view.
    fn home(&self, name: &str) -> (PathBuf, fleetdeck_proto::fleet_git::GitProjectView) {
        let (bare, home) = (self.root.join(format!("{name}.git")), self.root.join(name));
        fs::create_dir(&bare).unwrap();
        fs::create_dir(&home).unwrap();
        git(&bare, &["init", "--bare", "-b", "main"]);
        git(&home, &["init", "-b", "main"]);
        let url = format!("file://{}", bare.display());
        git(&home, &["remote", "add", "origin", &url]);
        let mut project = view(name);
        project.remote = remote_identity(&url, LocalRemotes::Allow).unwrap();
        project.clone_url = url;
        project.home_path = home.display().to_string();
        (home, project)
    }

    async fn set(
        &self,
        revision: u64,
        projects: Vec<fleetdeck_proto::fleet_git::GitProjectView>,
    ) -> SnapshotAck {
        let homes = projects
            .iter()
            .map(|p| StationHome {
                project_id: p.project_id.clone(),
                generation: 1,
                path: p.home_path.clone(),
                sync: true,
            })
            .collect();
        let snapshot = GitSnapshot {
            revision,
            projects,
            homes,
        };
        let params = serde_json::to_value(snapshot).unwrap();
        match projects_set(&self.manager, params).await {
            Outcome::Ok(value) => serde_json::from_value(value).unwrap(),
            Outcome::Err(e) => panic!("{e:?}"),
        }
    }
}

#[tokio::test]
async fn a_snapshot_is_checked_stored_and_answered() {
    let Some(rig) = Rig::new().await else { return };
    let (home, project) = rig.home("app");
    let ack = rig.set(10, vec![project.clone()]).await;
    assert!(ack.stored);
    assert_eq!(ack.revision, 10);
    assert_eq!(ack.checks.len(), 1);
    let check = &ack.checks[0];
    assert_eq!((check.project_id.as_str(), check.generation), ("app", 1));
    assert_eq!(check.state, HomeCheckState::Verified, "{check:?}");
    assert_eq!(
        check.canonical_path.as_deref(),
        Some(home.display().to_string().as_str())
    );
    let store = rig.manager.store();
    let row = store.git_project("app".into()).await.unwrap().unwrap();
    assert_eq!(row.check_state, HomeCheckState::Verified);
    assert_eq!(row.view, project);
}

#[tokio::test]
async fn a_stale_conflicting_or_repeated_snapshot_is_answered_without_a_change() {
    let Some(rig) = Rig::new().await else { return };
    let (_, one) = rig.home("one");
    let (_, two) = rig.home("two");
    assert!(rig.set(10, vec![one.clone()]).await.stored);
    // A lower revision: stale, with the station's own revision.
    let stale = rig.set(9, vec![two.clone()]).await;
    assert_eq!(
        (stale.stored, stale.revision, stale.checks.len()),
        (false, 10, 0)
    );
    // The same revision with another content: stale.
    let conflict = rig.set(10, vec![two.clone()]).await;
    assert_eq!((conflict.stored, conflict.revision), (false, 10));
    let store = rig.manager.store();
    assert!(store.git_project("two".into()).await.unwrap().is_none());
    // The same revision and content: a retry with the stored checks.
    let retry = rig.set(10, vec![one.clone()]).await;
    assert!(retry.stored);
    assert_eq!(retry.checks.len(), 1);
    assert_eq!(retry.checks[0].state, HomeCheckState::Verified);
    // A newer snapshot replaces the state: a project that is absent is gone.
    assert!(rig.set(11, vec![two]).await.stored);
    assert!(store.git_project("one".into()).await.unwrap().is_none());
    assert!(store.git_project("two".into()).await.unwrap().is_some());
}

#[tokio::test]
async fn every_home_gets_its_own_check() {
    let Some(rig) = Rig::new().await else { return };
    let (_, good) = rig.home("good");
    // A folder that is not a Git repository.
    let (plain, mut not_a_repo) = rig.home("plain");
    fs::remove_dir_all(plain.join(".git")).unwrap();
    not_a_repo.project_id = "plain".to_owned();
    // A home whose remote is another repository.
    let (home, mut other) = rig.home("other");
    git(
        &home,
        &["remote", "set-url", "origin", "file:///nowhere/else.git"],
    );
    other.project_id = "other".to_owned();
    // A missing folder, and a folder outside the allowed roots.
    let mut missing = view("missing");
    missing.home_path = rig.root.join("missing").display().to_string();
    let mut outside = view("outside");
    outside.home_path = "/".to_owned();
    let ack = rig
        .set(1, vec![good, not_a_repo, other, missing, outside])
        .await;
    let state = |id: &str| {
        let check = ack.checks.iter().find(|c| c.project_id == id).unwrap();
        (check.state, check.code.clone())
    };
    assert_eq!(state("good").0, HomeCheckState::Verified);
    assert_eq!(state("plain").0, HomeCheckState::NotARepo);
    assert_eq!(state("other").0, HomeCheckState::Refused);
    assert_eq!(state("missing").0, HomeCheckState::Missing);
    assert_eq!(
        state("outside"),
        (HomeCheckState::Refused, Some("outside_roots".to_owned()))
    );
    // The station keeps each result next to the settings.
    let store = rig.manager.store();
    let row = store.git_project("outside".into()).await.unwrap().unwrap();
    assert_eq!(row.check_state, HomeCheckState::Refused);
    assert_eq!(row.check_code.as_deref(), Some("outside_roots"));
}

#[tokio::test]
async fn a_malformed_snapshot_is_a_protocol_error() {
    let Some(rig) = Rig::new().await else { return };
    let outcome = projects_set(&rig.manager, serde_json::json!({ "revision": "x" })).await;
    assert!(matches!(outcome, Outcome::Err(_)));
}

#[tokio::test]
async fn push_now_makes_a_failing_row_due_and_names_its_two_errors() {
    let Some(rig) = Rig::new().await else { return };
    let store = rig.manager.store();
    store.create_session(new_session("s1")).await.unwrap();
    let push = Some((
        "origin".to_owned(),
        crate::store::git_state::tests::DEST.to_owned(),
    ));
    store
        .git_record_session(new_git("s1", "p1"), push)
        .await
        .unwrap();
    store.create_session(new_session("s2")).await.unwrap();
    let call = |id: &str| push_now(&rig.manager, serde_json::json!({ "session_id": id }));
    // A session that is not in a synced project.
    let Outcome::Err(e) = call("s2").await else {
        panic!("not_synced expected");
    };
    assert_eq!(
        (e.code.as_str(), e.message.as_str()),
        ("not_synced", "This session is not in a synced project.")
    );
    let Outcome::Err(e) = call("nobody").await else {
        panic!("unknown_session expected");
    };
    assert_eq!(
        (e.code.as_str(), e.message.as_str()),
        ("unknown_session", "The station does not know this session.")
    );
    // An idle row stays idle; a failing row is due at once.
    let Outcome::Ok(idle) = call("s1").await else {
        panic!("answer expected");
    };
    assert_eq!(
        serde_json::from_value::<PushNowAnswer>(idle).unwrap().state,
        PushState::Idle
    );
    let tip = "a".repeat(40);
    store.git_push_trigger("s1".into(), tip).await.unwrap();
    store
        .git_push_failed(
            "s1".into(),
            ("push_failed".to_owned(), "Push failed".to_owned()),
            false,
            crate::workspace::fleet_git::backoff_delay,
        )
        .await
        .unwrap();
    let failing = store
        .git_push(SessionId::from("s1"))
        .await
        .unwrap()
        .unwrap();
    assert!(failing.next_attempt_at.unwrap() > store.now_ms());
    let Outcome::Ok(now) = call("s1").await else {
        panic!("answer expected");
    };
    assert_eq!(
        serde_json::from_value::<PushNowAnswer>(now).unwrap().state,
        PushState::Failing
    );
    let due = store
        .git_push(SessionId::from("s1"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (due.attempts, due.next_attempt_at),
        (1, Some(store.now_ms()))
    );
}

#[test]
fn a_link_without_fleet_git_pauses_the_loops_until_it_ends() {
    let flag = Arc::new(AtomicBool::new(false));
    // A deck with `fleet_git.v1` pauses nothing.
    drop(PauseGuard::new(&flag, true));
    assert!(!flag.load(Ordering::Acquire));
    let guard = PauseGuard::new(&flag, false);
    assert!(flag.load(Ordering::Acquire));
    drop(guard);
    assert!(!flag.load(Ordering::Acquire));
}

// ---- home attempts and `station.info` (FG-5, FG-6, FG-11)

use fleetdeck_core::config::StationConfig;
use fleetdeck_core::paths::Env;
use fleetdeck_proto::fleet_git::{ConfigDigest, DigestItem, DriftItem, HomeAttemptState};

use crate::fleet_git_runtime::FleetGitRuntime;
use crate::store::home_attempts::{BeginHome, HomeOutcome, NewHomeAttempt};

impl Rig {
    async fn snapshot(&self, revision: u64, homes: &[(&str, u64)]) -> SnapshotAck {
        let homes = homes
            .iter()
            .map(|(id, generation)| StationHome {
                project_id: (*id).to_owned(),
                generation: *generation,
                path: format!("/h/{id}"),
                sync: false,
            })
            .collect();
        let snapshot = GitSnapshot {
            revision,
            projects: Vec::new(),
            homes,
        };
        let params = serde_json::to_value(snapshot).unwrap();
        match projects_set(&self.manager, params).await {
            Outcome::Ok(value) => serde_json::from_value(value).unwrap(),
            Outcome::Err(e) => panic!("{e:?}"),
        }
    }

    fn new_attempt(&self, id: &str, project: &str, generation: u64) -> NewHomeAttempt {
        NewHomeAttempt {
            attempt_id: id.to_owned(),
            project_id: project.to_owned(),
            generation,
            request_digest: "d".to_owned(),
            retry_of: None,
            dest_path: self.root.join(id).display().to_string(),
            url: "https://h/acme/app.git".to_owned(),
            remote_name: "origin".to_owned(),
        }
    }

    async fn attempt(&self, id: &str, project: &str, generation: u64, state: HomeAttemptState) {
        let store = self.manager.store();
        let new = self.new_attempt(id, project, generation);
        assert!(matches!(
            store.home_begin(new).await.unwrap(),
            BeginHome::Created(_)
        ));
        let outcome = HomeOutcome {
            state,
            path: Some("/p".to_owned()),
            dev: None,
            ino: None,
            code: None,
            message: None,
        };
        store.home_finish(id.to_owned(), outcome).await.unwrap();
    }

    async fn superseded(&self, project: &str, id: &str) -> bool {
        let row = self
            .manager
            .store()
            .home_attempt(project.to_owned(), id.to_owned())
            .await
            .unwrap()
            .unwrap();
        row.superseded
    }
}

#[tokio::test]
async fn a_snapshot_supersedes_the_attempts_of_a_removed_or_older_home() {
    let Some(rig) = Rig::new().await else { return };
    rig.attempt("a1", "p1", 1, HomeAttemptState::Ready).await;
    rig.attempt("a2", "p2", 1, HomeAttemptState::Failed).await;
    rig.attempt("a3", "p3", 1, HomeAttemptState::Adopted).await;
    // The same generation is current; a project that is absent is not.
    rig.snapshot(1, &[("p1", 1), ("p3", 1)]).await;
    assert!(!rig.superseded("p1", "a1").await);
    assert!(rig.superseded("p2", "a2").await);
    assert!(!rig.superseded("p3", "a3").await);
    // A higher generation supersedes the older attempt.
    rig.snapshot(2, &[("p1", 2), ("p3", 1)]).await;
    assert!(rig.superseded("p1", "a1").await);
    assert!(!rig.superseded("p3", "a3").await);
}

#[tokio::test]
async fn a_repeated_snapshot_supersedes_too() {
    let Some(rig) = Rig::new().await else { return };
    let first = rig.snapshot(5, &[("p1", 1)]).await;
    assert!(first.stored);
    rig.attempt("a1", "p9", 1, HomeAttemptState::Ready).await;
    // The same revision and digest is a retry (`Same`).
    let again = rig.snapshot(5, &[("p1", 1)]).await;
    assert!(again.stored);
    assert!(rig.superseded("p9", "a1").await);
}

#[tokio::test]
async fn a_cloning_attempt_is_never_superseded() {
    let Some(rig) = Rig::new().await else { return };
    let new = rig.new_attempt("c1", "p1", 1);
    rig.manager.store().home_begin(new).await.unwrap();
    rig.snapshot(1, &[]).await;
    assert!(!rig.superseded("p1", "c1").await);
}

fn config(text: &str) -> StationConfig {
    let env = Env {
        home: Some("/home/t".into()),
        ..Env::default()
    };
    StationConfig::parse(text, &env).unwrap()
}

#[tokio::test]
async fn station_info_carries_traits_config_and_the_fleet_git_part() {
    let Some(rig) = Rig::new().await else { return };
    let cfg = config(&format!(
        "allowed_roots = [\"{}\"]\ntraits = [\"gpu\"]\nallow_check_cmd = true\nallow_local_remotes = true\n",
        rig.root.display()
    ));
    let policy = Arc::new(Policy::from_config(&cfg));
    let runtime = Arc::new(FleetGitRuntime::new(Arc::new(
        crate::workspace::GitOptions::isolated,
    )));
    // The digest is `None` until the drift task wrote the first one.
    assert_eq!(runtime.config(), None);
    let digest = ConfigDigest {
        claude_version: Some("2.1.0".to_owned()),
        codex_version: None,
        items: vec![DigestItem {
            name: DriftItem::ClaudeMd,
            hash: "absent".to_owned(),
        }],
        computed_at: 7,
    };
    runtime.set_config(digest.clone());
    assert_eq!(runtime.config(), Some(digest));

    let traits = super::traits(&runtime, &policy).await.unwrap();
    assert_eq!(traits.os, std::env::consts::OS);
    assert_eq!(traits.arch, std::env::consts::ARCH);
    assert_eq!(traits.labels, ["gpu"]);

    let info = super::info(&runtime, &policy).await;
    assert_eq!(
        info.clone_root,
        rig.root.join("projects").display().to_string()
    );
    assert!(info.allow_check_cmd);
    assert!(info.allow_local_remotes, "from the station.toml key");
    assert!(
        info.git_version
            .is_some_and(|v| v.starts_with(|c: char| c.is_ascii_digit()))
    );

    // A policy with the key off and the bridge on reports true too (`serve`
    // makes the OR); with both off it reports false.
    let bridged = Arc::new(Policy::from_config(&config("")).with_allow_local_remotes(true));
    assert!(super::info(&runtime, &bridged).await.allow_local_remotes);
    let closed = Arc::new(Policy::from_config(&config("")));
    let info = super::info(&runtime, &closed).await;
    assert!(!info.allow_local_remotes && !info.allow_check_cmd);
}
