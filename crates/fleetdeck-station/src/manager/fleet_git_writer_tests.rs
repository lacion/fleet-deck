//! The one writer, the push triggers and the push loop (FG-2, design §5.4,
//! §5.5, §5.10), against real bare repositories on the same machine.

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use fleetdeck_proto::event::TurnStatus;
use fleetdeck_proto::fleet_git::{
    GitSnapshot, HomeCheck, HomeCheckState, LocalRemotes, PushMode, PushState, StationHome,
    remote_identity,
};
use fleetdeck_proto::{Event, SessionId};
use fleetdeck_testkit::Det;
use tokio::sync::watch;

use super::{FleetGitWriter, WriteJob};
use crate::manager::Policy;
use crate::manager::fleet_git_push::{self, trigger};
use crate::manager::policy_hub::PolicyHub;
use crate::store::git_state::PushRow;
use crate::store::git_state::tests::{new_git, new_session, store, view};
use crate::store::{NewEvent, OfflineApprovalPolicy, Store};
use crate::workspace::GitOptions;
use crate::workspace::branches::parse_git_version;
use crate::workspace::fleet_git::OptionsProvider;

struct Rig {
    _dir: tempfile::TempDir,
    root: PathBuf,
    program: PathBuf,
    bare: PathBuf,
    home: PathBuf,
    store: Store,
    hub: Arc<PolicyHub>,
}

impl Rig {
    /// A bare remote, a home that has it as `origin`, and the sessions `s1`
    /// (and `s2`) with their branches `fd/alpha/<id>`. `None` when Git is
    /// older than 2.45.
    async fn new(mode: PushMode, sessions: &[&str]) -> Option<Self> {
        let out = Command::new("git").arg("--version").output().unwrap();
        if parse_git_version(&String::from_utf8_lossy(&out.stdout)).is_none_or(|v| v < (2, 45)) {
            eprintln!("skipped: Fleet Git needs Git 2.45 or newer");
            return None;
        }
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let program = root.join("git-wrapper");
        let (bare, home) = (root.join("remote.git"), root.join("work"));
        fs::create_dir(&bare).unwrap();
        fs::create_dir(&home).unwrap();
        let det = Det::new();
        let policy = Policy::new(
            vec![root.clone()],
            4,
            OfflineApprovalPolicy::Wait,
            BTreeMap::new(),
        )
        .with_allow_local_remotes(true);
        let rig = Self {
            _dir: dir,
            root,
            program,
            bare,
            home,
            store: store(&det).await,
            hub: Arc::new(PolicyHub::new(policy)),
        };
        rig.script("exec git \"$@\"");
        rig.git(&rig.bare, &["init", "--bare", "-b", "main"]);
        rig.git(&rig.home, &["init", "-b", "main"]);
        rig.commit("one");
        let url = format!("file://{}", rig.bare.display());
        rig.git(&rig.home, &["remote", "add", "origin", &url]);
        let identity = remote_identity(&url, LocalRemotes::Allow).unwrap();
        let mut project = view("p1");
        project.settings.push = mode;
        project.clone_url = url;
        project.remote = identity.clone();
        project.home_path = rig.home.display().to_string();
        let check = HomeCheck {
            project_id: "p1".to_owned(),
            generation: 1,
            state: HomeCheckState::Verified,
            canonical_path: Some(project.home_path.clone()),
            seen: None,
            code: None,
        };
        let snapshot = GitSnapshot {
            revision: 1,
            projects: vec![project.clone()],
            homes: vec![StationHome {
                project_id: "p1".to_owned(),
                generation: 1,
                path: project.home_path.clone(),
                sync: true,
            }],
        };
        rig.store
            .git_apply_snapshot(snapshot, "d".to_owned(), vec![check])
            .await
            .unwrap();
        for id in sessions {
            rig.git(&rig.home, &["branch", &format!("fd/alpha/{id}")]);
            rig.store.create_session(new_session(id)).await.unwrap();
            let mut git = new_git(id, "p1");
            git.repo_root = rig.home.display().to_string();
            git.remote_dest = identity.dest.clone();
            let push = Some(("origin".to_owned(), identity.dest.clone()));
            rig.store.git_record_session(git, push).await.unwrap();
        }
        Some(rig)
    }

    fn script(&self, body: &str) {
        fs::write(
            &self.program,
            format!(
                "#!/bin/sh\nexport GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1\n{body}\n"
            ),
        )
        .unwrap();
        fs::set_permissions(&self.program, fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn git(&self, dir: &Path, args: &[&str]) -> String {
        let out = Command::new(&self.program)
            .current_dir(dir)
            .args(args)
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

    /// A new commit on `main` of the home.
    fn commit(&self, text: &str) -> String {
        fs::write(self.home.join("a.txt"), format!("{text}\n")).unwrap();
        self.git(&self.home, &["add", "."]);
        self.git(&self.home, &["commit", "-m", text]);
        self.git(&self.home, &["rev-parse", "HEAD"])
    }

    /// Moves the branch `fd/alpha/<id>` to `oid`.
    fn move_branch(&self, id: &str, oid: &str) {
        self.git(
            &self.home,
            &["branch", "-f", &format!("fd/alpha/{id}"), oid],
        );
    }

    fn options(&self) -> OptionsProvider {
        let options = GitOptions {
            program: self.program.clone(),
            ..GitOptions::isolated()
        };
        Arc::new(move || options.clone())
    }

    fn writer(&self) -> FleetGitWriter {
        FleetGitWriter::start(self.store.clone(), Arc::clone(&self.hub), self.options()).0
    }

    fn remote_ref(&self, id: &str) -> Option<String> {
        let name = format!("refs/heads/fd/alpha/{id}");
        let out = self.git(
            &self.bare,
            &["for-each-ref", "--format=%(objectname)", &name],
        );
        (!out.is_empty()).then_some(out)
    }

    async fn row(&self, id: &str) -> PushRow {
        self.store
            .git_push(SessionId::from(id))
            .await
            .unwrap()
            .unwrap()
    }

    /// Triggers `id` and lets the writer run its due row.
    async fn push(&self, writer: &FleetGitWriter, id: &str) -> PushRow {
        trigger(&self.store, &SessionId::from(id), true).await;
        assert!(
            writer
                .run(WriteJob::BranchPush {
                    session_id: SessionId::from(id)
                })
                .await
        );
        self.row(id).await
    }
}

fn tip(rig: &Rig, id: &str) -> String {
    rig.git(&rig.home, &["rev-parse", &format!("fd/alpha/{id}")])
}

#[tokio::test]
async fn a_trigger_and_the_writer_push_the_branch_then_a_revert_pushes_with_the_lease() {
    let Some(rig) = Rig::new(PushMode::OnTurn, &["s1"]).await else {
        return;
    };
    let writer = rig.writer();
    let first = rig.commit("two");
    rig.move_branch("s1", &first);
    let row = rig.push(&writer, "s1").await;
    assert_eq!(
        (row.state, row.last_pushed_oid.as_deref()),
        (PushState::Pushed, Some(first.as_str()))
    );
    assert_eq!(rig.remote_ref("s1"), Some(first.clone()));
    // A newer tip, then a revert to the older commit: the lease is the last
    // pushed commit, so the push forces over it.
    let second = rig.commit("three");
    rig.move_branch("s1", &second);
    let row = rig.push(&writer, "s1").await;
    assert_eq!(row.last_pushed_oid.as_deref(), Some(second.as_str()));
    rig.move_branch("s1", &first);
    let row = rig.push(&writer, "s1").await;
    assert_eq!(
        (row.state, row.last_pushed_oid.as_deref()),
        (PushState::Pushed, Some(first.as_str()))
    );
    assert_eq!(rig.remote_ref("s1"), Some(first));
    // The remote has no other ref than the pushed branch.
    let refs = rig.git(&rig.bare, &["for-each-ref", "--format=%(refname)"]);
    assert_eq!(refs, "refs/heads/fd/alpha/s1");
}

#[tokio::test]
async fn a_moved_remote_ref_fails_with_a_backoff_and_no_open_attempt() {
    let Some(rig) = Rig::new(PushMode::OnTurn, &["s1"]).await else {
        return;
    };
    // Another machine pushed the same ref.
    let other = rig.root.join("other");
    fs::create_dir(&other).unwrap();
    let url = format!("file://{}", rig.bare.display());
    rig.git(&other, &["init", "-b", "main"]);
    fs::write(other.join("b.txt"), "x\n").unwrap();
    rig.git(&other, &["add", "."]);
    rig.git(&other, &["commit", "-m", "other"]);
    rig.git(&other, &["push", &url, "HEAD:refs/heads/fd/alpha/s1"]);
    let moved = rig.remote_ref("s1");
    let writer = rig.writer();
    let first = rig.commit("two");
    rig.move_branch("s1", &first);
    let row = rig.push(&writer, "s1").await;
    assert_eq!((row.state, row.attempts), (PushState::Failing, 1));
    assert_eq!(row.code.as_deref(), Some("push_rejected"));
    assert_eq!(row.attempt_id, None);
    assert!(row.next_attempt_at.unwrap() > rig.store.now_ms());
    assert_eq!(rig.remote_ref("s1"), moved);
}

#[tokio::test]
async fn a_destination_that_changed_fails_before_any_attempt_opens() {
    let Some(rig) = Rig::new(PushMode::OnTurn, &["s1"]).await else {
        return;
    };
    let other = rig.root.join("other.git");
    fs::create_dir(&other).unwrap();
    rig.git(&other, &["init", "--bare", "-b", "main"]);
    let url = format!("file://{}", other.display());
    rig.git(&rig.home, &["remote", "set-url", "origin", &url]);
    let writer = rig.writer();
    let row = rig.push(&writer, "s1").await;
    assert_eq!((row.state, row.attempts), (PushState::Failing, 1));
    assert_eq!(row.code.as_deref(), Some("remote_mismatch"));
    assert_eq!(row.attempt_id, None);
    assert_eq!(rig.remote_ref("s1"), None);
    assert_eq!(rig.git(&other, &["for-each-ref"]), "");
}

#[tokio::test]
async fn a_restart_in_pushing_reads_the_remote_before_any_new_push() {
    let Some(rig) = Rig::new(PushMode::OnTurn, &["s1", "s2"]).await else {
        return;
    };
    let url = format!("file://{}", rig.bare.display());
    let writer = rig.writer();
    // s1: the push took effect before the station stopped.
    let applied = rig.commit("two");
    rig.move_branch("s1", &applied);
    trigger(&rig.store, &SessionId::from("s1"), true).await;
    rig.store
        .git_push_open_attempt("s1".into(), url.clone())
        .await
        .unwrap();
    rig.git(
        &rig.home,
        &["push", &url, &format!("{applied}:refs/heads/fd/alpha/s1")],
    );
    // s2: the push never reached the remote.
    rig.move_branch("s2", &applied);
    trigger(&rig.store, &SessionId::from("s2"), true).await;
    rig.store
        .git_push_open_attempt("s2".into(), url)
        .await
        .unwrap();
    for id in ["s1", "s2"] {
        let job = WriteJob::BranchPush {
            session_id: SessionId::from(id),
        };
        assert!(writer.run(job).await);
    }
    let one = rig.row("s1").await;
    assert_eq!(
        (one.state, one.last_pushed_oid.as_deref(), one.attempt_id),
        (PushState::Pushed, Some(applied.as_str()), None)
    );
    // The recovery read confirmed that nothing was applied: no new push ran.
    let two = rig.row("s2").await;
    assert_eq!((two.state, two.attempts), (PushState::Failing, 1));
    assert_eq!(two.attempt_id, None);
    assert_eq!(rig.remote_ref("s2"), None);
}

#[tokio::test]
async fn two_jobs_run_one_after_the_other() {
    let Some(rig) = Rig::new(PushMode::OnTurn, &["s1", "s2"]).await else {
        return;
    };
    let log = rig.root.join("pushes.log");
    rig.script(&format!(
        "case \" $* \" in *\" push \"*) echo start >> {log}; sleep 0.4; git \"$@\"; rc=$?; echo end >> {log}; exit $rc;; esac\nexec git \"$@\"",
        log = log.display()
    ));
    let writer = rig.writer();
    for id in ["s1", "s2"] {
        let oid = rig.commit(id);
        rig.move_branch(id, &oid);
        trigger(&rig.store, &SessionId::from(id), true).await;
    }
    let job = |id: &str| WriteJob::BranchPush {
        session_id: SessionId::from(id),
    };
    let (a, b) = tokio::join!(writer.run(job("s1")), writer.run(job("s2")));
    assert!(a && b);
    assert_eq!(
        fs::read_to_string(&log)
            .unwrap()
            .lines()
            .collect::<Vec<_>>(),
        ["start", "end", "start", "end"]
    );
    for id in ["s1", "s2"] {
        assert_eq!(rig.row(id).await.state, PushState::Pushed);
        assert_eq!(rig.remote_ref(id), Some(tip(&rig, id)));
    }
}

#[tokio::test]
async fn a_project_with_push_off_is_never_pushed() {
    let Some(rig) = Rig::new(PushMode::Off, &["s1"]).await else {
        return;
    };
    let writer = rig.writer();
    let oid = rig.commit("two");
    rig.move_branch("s1", &oid);
    let row = rig.push(&writer, "s1").await;
    assert_eq!((row.state, row.desired_oid), (PushState::Idle, None));
    assert_eq!(rig.remote_ref("s1"), None);
}

/// Polls until `pick` holds for the row of `id`.
async fn wait_for(rig: &Rig, id: &str, what: &str, pick: impl Fn(&PushRow) -> bool) -> PushRow {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        let row = rig.row(id).await;
        if pick(&row) {
            return row;
        }
        assert!(tokio::time::Instant::now() < deadline, "no {what}: {row:?}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn append(rig: &Rig, id: &str, event: Event) {
    rig.store
        .append(SessionId::from(id), vec![NewEvent::from(event)])
        .await
        .unwrap();
}

fn turn_completed() -> Event {
    Event::TurnCompleted {
        turn_id: "t1".into(),
        status: TurnStatus::Done,
        usage: None,
    }
}

fn ended() -> Event {
    Event::SessionEnded {
        reason: "exit".to_owned(),
        exit_code: Some(0),
    }
}

fn running(rig: &Rig) -> (Vec<tokio::task::JoinHandle<()>>, watch::Sender<bool>) {
    let (stop, rx) = watch::channel(false);
    let tasks = fleet_git_push::start(rig.store.clone(), Arc::clone(&rig.hub), rig.options(), rx);
    (tasks, stop)
}

#[tokio::test]
async fn a_completed_turn_makes_the_loop_push_with_on_turn() {
    let Some(rig) = Rig::new(PushMode::OnTurn, &["s1"]).await else {
        return;
    };
    let oid = rig.commit("two");
    rig.move_branch("s1", &oid);
    rig.store
        .git_set_push_cursor(rig.store.head())
        .await
        .unwrap();
    let (tasks, stop) = running(&rig);
    append(&rig, "s1", turn_completed()).await;
    let row = wait_for(&rig, "s1", "the push", |r| r.state == PushState::Pushed).await;
    assert_eq!(row.last_pushed_oid.as_deref(), Some(oid.as_str()));
    assert_eq!(rig.remote_ref("s1"), Some(oid));
    stop.send(true).unwrap();
    for task in tasks {
        task.await.unwrap();
    }
}

#[tokio::test]
async fn on_exit_pushes_at_the_end_of_the_session_only() {
    let Some(rig) = Rig::new(PushMode::OnExit, &["s1"]).await else {
        return;
    };
    let oid = rig.commit("two");
    rig.move_branch("s1", &oid);
    rig.store
        .git_set_push_cursor(rig.store.head())
        .await
        .unwrap();
    let (tasks, stop) = running(&rig);
    append(&rig, "s1", turn_completed()).await;
    tokio::time::sleep(Duration::from_millis(2500)).await;
    assert_eq!(rig.row("s1").await.state, PushState::Idle);
    assert_eq!(rig.remote_ref("s1"), None);
    append(&rig, "s1", ended()).await;
    wait_for(&rig, "s1", "the push", |r| r.state == PushState::Pushed).await;
    assert_eq!(rig.remote_ref("s1"), Some(oid));
    stop.send(true).unwrap();
    for task in tasks {
        task.await.unwrap();
    }
}

#[tokio::test]
async fn the_loops_pause_while_a_deck_without_fleet_git_is_linked() {
    let Some(rig) = Rig::new(PushMode::OnTurn, &["s1"]).await else {
        return;
    };
    let oid = rig.commit("two");
    rig.move_branch("s1", &oid);
    rig.store
        .git_set_push_cursor(rig.store.head())
        .await
        .unwrap();
    rig.store.fleet_git_paused().store(true, Ordering::Release);
    let (tasks, stop) = running(&rig);
    append(&rig, "s1", turn_completed()).await;
    tokio::time::sleep(Duration::from_millis(2500)).await;
    // The row is kept as it is: no trigger was read, nothing was pushed.
    assert_eq!(rig.row("s1").await.state, PushState::Idle);
    assert_eq!(rig.remote_ref("s1"), None);
    rig.store.fleet_git_paused().store(false, Ordering::Release);
    wait_for(&rig, "s1", "the push after the pause", |r| {
        r.state == PushState::Pushed
    })
    .await;
    assert_eq!(rig.remote_ref("s1"), Some(oid));
    stop.send(true).unwrap();
    for task in tasks {
        task.await.unwrap();
    }
}

#[tokio::test]
async fn a_position_ahead_of_the_log_rescans_every_row() {
    let Some(rig) = Rig::new(PushMode::OnTurn, &["s1"]).await else {
        return;
    };
    // A rescan after a gap sees the branch tip of a live row.
    let oid = rig.commit("two");
    rig.move_branch("s1", &oid);
    rig.store.git_set_push_cursor(u64::MAX / 2).await.unwrap();
    let (tasks, stop) = running(&rig);
    let row = wait_for(&rig, "s1", "the rescan", |r| r.desired_oid.is_some()).await;
    assert_eq!(row.desired_oid.as_deref(), Some(oid.as_str()));
    stop.send(true).unwrap();
    for task in tasks {
        task.await.unwrap();
    }
}
