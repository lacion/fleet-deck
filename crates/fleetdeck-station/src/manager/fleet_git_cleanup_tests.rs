//! The cleanup loop (FG-3, design §6.3, §6.6) with real Git and a bare
//! remote on the same machine. The trash effect runs by hand, so each test
//! controls what happens between the decision and the move.

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use fleetdeck_proto::command::{
    CommandEnvelope, CommandOrigin, RETRY_WINDOW_MS, ReceiptState, SessionCommand,
};
use fleetdeck_proto::event::ProviderKind;
use fleetdeck_proto::fleet_git::{
    CleanupMode, CleanupState, GitSettings, GitSnapshot, HomeCheck, HomeCheckState, LocalRemotes,
    PushMode, StationHome, remote_identity,
};
use fleetdeck_proto::link::WorktreeVerdict;
use fleetdeck_proto::{CommandId, Event, EventBody};
use fleetdeck_testkit::Det;

use super::{CleanupCtx, CleanupOutcome, run_once};
use crate::manager::Policy;
use crate::manager::policy_hub::PolicyHub;
use crate::manager::trash;
use crate::shell::{ShellConfig, Shells};
use crate::store::git_state::tests::{new_git, store, view};
use crate::store::{
    Caller, Dispatch, NewEvent, NewSession, OfflineApprovalPolicy, ReadResult, Store,
    WorktreeAction, WorktreeOp,
};
use crate::workspace::GitOptions;
use crate::workspace::branches::parse_git_version;
use crate::workspace::worktrees::{self, git_options};

const HOUR: i64 = 3_600_000;

struct Rig {
    _dir: tempfile::TempDir,
    root: PathBuf,
    program: PathBuf,
    fetch_log: PathBuf,
    bare: PathBuf,
    home: PathBuf,
    det: Det,
    store: Store,
    hub: Arc<PolicyHub>,
    shells: Arc<Shells>,
    dest: String,
}

impl Rig {
    /// A bare remote, a home with `origin` set to it, and the project `p1`
    /// (cleanup `merged` after 24 hours, push mode `push`). `None` when Git is
    /// older than 2.45. With `publish_main`, `main` is on the remote.
    async fn new(push: PushMode, publish_main: bool) -> Option<Self> {
        Self::with_cleanup(push, publish_main, CleanupMode::Merged).await
    }

    async fn with_cleanup(
        push: PushMode,
        publish_main: bool,
        cleanup: CleanupMode,
    ) -> Option<Self> {
        let out = Command::new("git").arg("--version").output().unwrap();
        if parse_git_version(&String::from_utf8_lossy(&out.stdout)).is_none_or(|v| v < (2, 45)) {
            eprintln!("skipped: Fleet Git needs Git 2.45 or newer");
            return None;
        }
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
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
        let url = format!("file://{}", bare.display());
        let dest = remote_identity(&url, LocalRemotes::Allow).unwrap();
        let rig = Self {
            program: root.join("git-wrapper"),
            fetch_log: root.join("fetches"),
            _dir: dir,
            root,
            bare,
            home,
            store: store(&det).await,
            det,
            hub: Arc::new(PolicyHub::new(policy)),
            shells: Arc::new(Shells::new(ShellConfig::default())),
            dest: dest.dest.clone(),
        };
        rig.script("");
        rig.git(&rig.bare, &["init", "--bare", "-b", "main"]);
        rig.git(&rig.home, &["init", "-b", "main"]);
        rig.commit(&rig.home, "a.txt", "one");
        rig.git(&rig.home, &["remote", "add", "origin", &url]);
        if publish_main {
            rig.git(&rig.home, &["push", "origin", "main"]);
        }
        let mut project = view("p1");
        project.settings = GitSettings {
            sync: true,
            push,
            cleanup,
            cleanup_after_hours: 24,
            ..GitSettings::default()
        };
        project.clone_url = url;
        project.remote = dest;
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
        Some(rig)
    }

    /// The Git program: it logs each fetch, and `extra` runs first.
    fn script(&self, extra: &str) {
        let body = format!(
            "#!/bin/sh\nexport GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1\n\
             case \" $* \" in *\" fetch \"*) echo x >> {log}; {extra} ;; esac\n\
             exec git \"$@\"\n",
            log = self.fetch_log.display()
        );
        fs::write(&self.program, body).unwrap();
        fs::set_permissions(&self.program, fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn fetches(&self) -> usize {
        fs::read_to_string(&self.fetch_log).map_or(0, |t| t.lines().count())
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

    fn commit(&self, dir: &Path, file: &str, text: &str) -> String {
        fs::write(dir.join(file), format!("{text}\n")).unwrap();
        self.git(dir, &["add", "."]);
        self.git(dir, &["commit", "-m", text]);
        self.git(dir, &["rev-parse", "HEAD"])
    }

    fn options(&self) -> GitOptions {
        GitOptions {
            program: self.program.clone(),
            ..GitOptions::isolated()
        }
    }

    fn ctx(&self) -> CleanupCtx {
        let options = self.options();
        CleanupCtx {
            store: self.store.clone(),
            policy: Arc::clone(&self.hub),
            shells: Arc::clone(&self.shells),
            options: Arc::new(move || options.clone()),
        }
    }

    fn wt(id: &str, rig: &Self) -> PathBuf {
        rig.root.join(format!("work--{id}"))
    }

    /// A session `id` in its own worktree on `fd/alpha/<id>`, ended unless
    /// `live`. With `own_branch` false the station did not make the branch
    /// (no publication ref, no push row).
    async fn session(&self, id: &str, live: bool, own_branch: bool) -> PathBuf {
        let wt = Self::wt(id, self);
        let branch = format!("fd/alpha/{id}");
        self.git(
            &self.home,
            &["worktree", "add", "-q", "-b", &branch, wt.to_str().unwrap()],
        );
        let cwd = wt.display().to_string();
        self.store
            .create_session(NewSession {
                session_id: id.into(),
                provider: ProviderKind::Claude,
                provider_instance: "claude".into(),
                cwd: cwd.clone(),
                worktree: None,
                branch: None,
                mode: None,
                title: None,
                offline_approval_policy: OfflineApprovalPolicy::Wait,
            })
            .await
            .unwrap();
        let named: NewEvent = Event::SessionNamed {
            callsign: id.to_owned(),
            worktree: Some(cwd),
        }
        .into();
        self.store.append(id.into(), vec![named]).await.unwrap();
        let mut git = new_git(id, "p1");
        git.repo_root = self.home.display().to_string();
        git.remote_dest = self.dest.clone();
        let push = Some(("origin".to_owned(), self.dest.clone())).filter(|_| own_branch);
        if !own_branch {
            git.created_branch = false;
            git.publish_ref = None;
        }
        self.store.git_record_session(git, push).await.unwrap();
        if !live {
            self.store
                .append(id.into(), vec![ended_event()])
                .await
                .unwrap();
        }
        wt
    }

    /// The push row of `id` says that its current tip is pushed.
    async fn mark_pushed(&self, id: &str) {
        let tip = self.git(&Self::wt(id, self), &["rev-parse", "HEAD"]);
        let url = format!("file://{}", self.bare.display());
        self.store
            .git_push_trigger(id.into(), tip)
            .await
            .unwrap()
            .unwrap();
        self.store
            .git_push_open_attempt(id.into(), url)
            .await
            .unwrap();
        self.store.git_push_applied(id.into()).await.unwrap();
    }

    /// The branch of `id` has a commit, and `main` (home and remote) has it.
    fn merge(&self, id: &str) {
        self.commit(&Self::wt(id, self), &format!("{id}.txt"), id);
        self.git(&self.home, &["merge", "-q", &format!("fd/alpha/{id}")]);
        self.git(&self.home, &["push", "-q", "origin", "main"]);
    }

    /// `hours` after the latest end of the session that ended first.
    async fn at(&self, hours: i64) -> i64 {
        let rows = self.store.git_cleanup_rows().await.unwrap();
        rows.iter().filter_map(|r| r.ended_at).min().unwrap() + hours * HOUR
    }

    async fn pass(&self, hours: i64) -> Vec<CleanupOutcome> {
        run_once(&self.ctx(), self.at(hours).await).await
    }

    async fn cleanup_events(&self) -> Vec<(CleanupState, Option<String>)> {
        let ReadResult::Events { events, .. } = self.store.read_after(0, 1000).await.unwrap()
        else {
            panic!("snapshot required");
        };
        events
            .into_iter()
            .filter_map(|e| match e.event {
                EventBody::Known(Event::SessionCleanup { state, reason }) => Some((state, reason)),
                _ => None,
            })
            .collect()
    }

    async fn op(&self, id: &str, hours: i64) -> WorktreeOp {
        let op_id = format!("cleanup-{id}-{}", self.at(hours).await);
        self.store.worktree_op(op_id).await.unwrap().unwrap()
    }

    /// Runs the effect by hand, then settles the queued effect rows.
    async fn effect(&self, op: &str, action: WorktreeAction) -> Dispatch {
        let policy = self.hub.current();
        let out = crate::workspace::trash::run(
            &self.store,
            &policy,
            &self.options(),
            Some(&self.shells),
            op,
            action,
        )
        .await;
        let fence = self.store.register_worker().await.unwrap();
        while let Some(effect) = self.store.claim_next(fence).await.unwrap() {
            self.store
                .settle(effect.effect_id, fence, Dispatch::Confirmed)
                .await
                .unwrap();
        }
        out
    }

    async fn restore(&self, id: &str) {
        let policy = self.hub.current();
        let envelope = CommandEnvelope {
            authority: None,
            command_id: CommandId::from("01JRESTORE0001"),
            target_incarnation: None,
            retry_until: i64::try_from(self.det.now_ms()).unwrap() + RETRY_WINDOW_MS,
            origin: CommandOrigin::Board,
            command: SessionCommand::WorktreeRestore {
                session_id: id.into(),
            },
        };
        let decided = self
            .store
            .decide(envelope, Caller::local(), move |d| {
                trash::decide(
                    d,
                    &policy,
                    CommandOrigin::Board,
                    None,
                    SessionCommand::WorktreeRestore {
                        session_id: "s1".into(),
                    },
                )
            })
            .await
            .unwrap();
        assert_eq!(decided.receipt.state, ReceiptState::Accepted);
    }
}

fn ended_event() -> NewEvent {
    Event::SessionEnded {
        reason: "process_exit".to_owned(),
        exit_code: Some(0),
    }
    .into()
}

fn kept(id: &str, reason: &str) -> CleanupOutcome {
    CleanupOutcome {
        session_id: id.into(),
        state: CleanupState::Kept,
        reason: Some(reason.to_owned()),
    }
}

/// A merged, clean, pushed session `s1`: every condition holds after 25 hours.
async fn eligible(push: PushMode) -> Option<(Rig, PathBuf)> {
    let rig = Rig::new(push, true).await?;
    let wt = rig.session("s1", false, true).await;
    rig.merge("s1");
    rig.mark_pushed("s1").await;
    Some((rig, wt))
}

#[tokio::test]
async fn a_merged_clean_pushed_old_worktree_goes_to_the_trash_and_can_come_back() {
    let Some((rig, wt)) = eligible(PushMode::OnTurn).await else {
        return;
    };
    let ended = rig.at(0).await;
    let outcomes = rig.pass(25).await;
    assert_eq!(
        outcomes,
        vec![CleanupOutcome {
            session_id: "s1".into(),
            state: CleanupState::Trashed,
            reason: None
        }]
    );
    let op = rig.op("s1", 25).await;
    assert_eq!(
        (op.state.as_str(), op.expect_branch.as_deref()),
        ("trashing", Some("refs/heads/fd/alpha/s1"))
    );
    assert_eq!(
        op.expect_oid.as_deref(),
        Some(rig.git(&wt, &["rev-parse", "HEAD"]).as_str())
    );
    // The folder name ends with the last 12 characters of the session id.
    assert!(op.trash_path.ends_with("--s1"), "{}", op.trash_path);
    assert_eq!(
        rig.effect(&op.op_id, WorktreeAction::Trash).await,
        Dispatch::Confirmed
    );
    let row = rig
        .store
        .worktree_op(op.op_id.clone())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.state, "trashed", "{row:?}");
    assert!(!wt.exists());
    assert!(Path::new(&row.trash_path).join("s1.txt").is_file());
    // The branch stays; the inventory shows the trash; the state is final.
    assert_eq!(
        rig.git(
            &rig.home,
            &["rev-parse", "--verify", "refs/heads/fd/alpha/s1"]
        ),
        op.expect_oid.clone().unwrap()
    );
    let policy = rig.hub.current();
    let inventory = worktrees::inventory(&rig.store, &policy, &git_options())
        .await
        .unwrap();
    assert_eq!(inventory.worktrees[0].verdict, WorktreeVerdict::Trashed);
    assert_eq!(
        rig.cleanup_events().await,
        vec![(CleanupState::Trashed, None)]
    );
    assert!(rig.store.git_cleanup_rows().await.unwrap().is_empty());
    // A restore from the board works for a cleanup row; it is never trashed
    // by cleanup again.
    rig.restore("s1").await;
    assert_eq!(
        rig.effect(&op.op_id, WorktreeAction::Restore).await,
        Dispatch::Confirmed
    );
    assert!(wt.join("s1.txt").is_file());
    assert!(run_once(&rig.ctx(), ended + 60 * HOUR).await.is_empty());
}

#[tokio::test]
async fn a_session_that_is_too_new_waits_and_a_resume_counts_its_age_again() {
    let Some(rig) = Rig::new(PushMode::Off, true).await else {
        return;
    };
    rig.session("s1", false, true).await;
    let ended = rig.at(0).await;
    let due = (ended + 24 * HOUR).to_string();
    let waiting = |due: &str| CleanupOutcome {
        session_id: "s1".into(),
        state: CleanupState::Waiting,
        reason: Some(due.to_owned()),
    };
    assert_eq!(rig.pass(23).await, vec![waiting(&due)]);
    // Another pass says `waiting` again: no second event.
    assert_eq!(rig.pass(23).await, vec![waiting(&due)]);
    assert_eq!(
        rig.cleanup_events().await,
        vec![(CleanupState::Waiting, Some(due))]
    );
    // Resumed after 29 hours and ended again 1 hour later: the age restarts.
    rig.det.advance(Duration::from_secs(29 * 3600));
    let started: NewEvent = Event::SessionStarted {
        provider: ProviderKind::Claude,
        provider_instance: "claude".into(),
        provider_version: None,
        model: None,
        cwd: "/w".to_owned(),
        mode: None,
        capabilities: Box::new(fleetdeck_proto::capability::Capabilities::minimal()),
        resumed: None,
        provider_refs: fleetdeck_proto::event::ProviderRefs::new(),
        title: None,
        branch: None,
        repo: None,
    }
    .into();
    rig.store.append("s1".into(), vec![started]).await.unwrap();
    // A live session that was marked is `waiting` again, with no due time and
    // no new event.
    let live = CleanupOutcome {
        session_id: "s1".into(),
        state: CleanupState::Waiting,
        reason: None,
    };
    assert_eq!(run_once(&rig.ctx(), ended + 30 * HOUR).await, vec![live]);
    rig.det.advance(Duration::from_secs(3600));
    rig.store
        .append("s1".into(), vec![ended_event()])
        .await
        .unwrap();
    let second = rig.at(0).await;
    assert_eq!(second, ended + 30 * HOUR);
    let outcomes = run_once(&rig.ctx(), second + HOUR).await;
    assert_eq!(outcomes, vec![waiting(&(second + 24 * HOUR).to_string())]);
}

#[tokio::test]
async fn each_false_condition_keeps_the_worktree_with_its_reason() {
    // Not merged: an extra commit that no remote branch holds; and a squash merge.
    let Some(rig) = Rig::new(PushMode::Off, true).await else {
        return;
    };
    let wt = rig.session("s1", false, true).await;
    rig.commit(&wt, "s1.txt", "work");
    assert_eq!(rig.pass(25).await, vec![kept("s1", "not_merged")]);
    rig.git(&rig.home, &["merge", "--squash", "fd/alpha/s1"]);
    rig.git(&rig.home, &["commit", "-m", "squash"]);
    rig.git(&rig.home, &["push", "-q", "origin", "main"]);
    assert_eq!(rig.pass(25).await, vec![kept("s1", "not_merged")]);
    assert!(wt.is_dir());
    // Merged, then an untracked file: dirty.
    let Some((rig, wt)) = eligible(PushMode::OnTurn).await else {
        return;
    };
    fs::write(wt.join("new.txt"), "x\n").unwrap();
    assert_eq!(rig.pass(25).await, vec![kept("s1", "dirty")]);
    // A commit after the last push: not pushed.
    let Some((rig, wt)) = eligible(PushMode::OnTurn).await else {
        return;
    };
    rig.git(&wt, &["commit", "--allow-empty", "-m", "more"]);
    rig.git(&rig.home, &["merge", "-q", "fd/alpha/s1"]);
    rig.git(&rig.home, &["push", "-q", "origin", "main"]);
    assert_eq!(rig.pass(25).await, vec![kept("s1", "not_pushed")]);
    // Push off: the push row does not matter.
    let Some(rig) = Rig::new(PushMode::Off, true).await else {
        return;
    };
    rig.session("s1", false, true).await;
    assert_eq!(rig.pass(25).await[0].state, CleanupState::Trashed);
    // Push on and a branch that Fleet Git did not make: never pushed.
    let Some(rig) = Rig::new(PushMode::OnTurn, true).await else {
        return;
    };
    rig.session("s1", false, false).await;
    assert_eq!(rig.pass(25).await, vec![kept("s1", "not_pushed")]);
}

#[tokio::test]
async fn a_worktree_that_left_its_branch_is_kept_even_with_a_merged_branch() {
    let Some((rig, wt)) = eligible(PushMode::OnTurn).await else {
        return;
    };
    // Another branch with unpushed commits: the recorded branch is merged.
    rig.git(&wt, &["switch", "-q", "-c", "other"]);
    rig.commit(&wt, "other.txt", "other");
    assert_eq!(rig.pass(25).await, vec![kept("s1", "branch_changed")]);
    rig.git(&wt, &["switch", "-q", "--detach"]);
    assert_eq!(rig.pass(25).await, vec![kept("s1", "detached")]);
    assert!(wt.is_dir());
}

#[tokio::test]
async fn a_fetch_that_fails_keeps_it_and_each_repository_is_fetched_once() {
    let Some((rig, _)) = eligible(PushMode::OnTurn).await else {
        return;
    };
    // A second session of the same repository.
    rig.session("s2", false, true).await;
    rig.script("echo 'fatal: boom' >&2; exit 1");
    let outcomes = rig.pass(25).await;
    assert_eq!(
        outcomes,
        vec![kept("s1", "fetch_failed"), kept("s2", "fetch_failed")]
    );
    assert_eq!(rig.fetches(), 1);
}

#[tokio::test]
async fn a_remote_without_a_default_branch_and_a_refused_trash_keep_it() {
    // No branch on the remote at all.
    let Some(rig) = Rig::new(PushMode::Off, false).await else {
        return;
    };
    rig.session("s1", false, true).await;
    assert_eq!(rig.pass(25).await, vec![kept("s1", "no_default_branch")]);
    // The trash checks refuse: a shell is open in the worktree.
    let Some((rig, wt)) = eligible(PushMode::OnTurn).await else {
        return;
    };
    rig.shells
        .execute(
            &fleetdeck_proto::shell::ShellChange::Open {
                shell_id: "sh1".into(),
                cwd: wt.display().to_string(),
                cols: 80,
                rows: 24,
            },
            &rig.hub.current(),
            0,
        )
        .unwrap();
    let outcomes = rig.pass(25).await;
    assert_eq!(outcomes.len(), 1);
    let reason = outcomes[0].reason.clone().unwrap();
    assert!(
        reason.starts_with("trash_refused: a shell is open"),
        "{reason}"
    );
    assert!(wt.is_dir());
}

#[tokio::test]
async fn a_project_with_cleanup_off_is_never_checked() {
    let Some(rig) = Rig::with_cleanup(PushMode::Off, true, CleanupMode::Off).await else {
        return;
    };
    rig.session("s1", false, true).await;
    assert!(rig.pass(25).await.is_empty());
    assert_eq!(rig.fetches(), 0);
    assert!(rig.cleanup_events().await.is_empty());
}

#[tokio::test]
async fn a_change_after_the_checks_ends_the_trash_without_a_move() {
    for untracked in [false, true] {
        let Some((rig, wt)) = eligible(PushMode::OnTurn).await else {
            return;
        };
        assert_eq!(rig.pass(25).await[0].state, CleanupState::Trashed);
        // The decision is stored; the worktree changes before the effect.
        if untracked {
            fs::write(wt.join("late.txt"), "x\n").unwrap();
        } else {
            rig.git(&wt, &["commit", "--allow-empty", "-m", "late"]);
        }
        let op = rig.op("s1", 25).await;
        assert_eq!(
            rig.effect(&op.op_id, WorktreeAction::Trash).await,
            Dispatch::Confirmed
        );
        let row = rig.store.worktree_op(op.op_id).await.unwrap().unwrap();
        assert_eq!(
            (row.state.as_str(), row.message.as_deref()),
            ("released", Some("worktree_changed"))
        );
        assert!(wt.join("s1.txt").is_file());
        assert_eq!(
            rig.cleanup_events().await,
            vec![(CleanupState::Kept, Some("changed".to_owned()))]
        );
        let rows = rig.store.git_cleanup_rows().await.unwrap();
        assert_eq!(rows[0].cleanup_state.as_deref(), Some("kept:changed"));
    }
}

#[tokio::test]
async fn the_trash_folder_of_a_cleanup_ends_with_the_last_12_characters_of_the_session_id() {
    let Some(rig) = Rig::new(PushMode::Off, true).await else {
        return;
    };
    let id = "01JABCDEFGHJKMNPQRSTVWXYZ0";
    rig.session(id, false, true).await;
    assert_eq!(rig.pass(25).await[0].state, CleanupState::Trashed);
    let op = rig.op(id, 25).await;
    let suffix = &id.to_lowercase()[id.len() - 12..];
    assert!(
        op.trash_path.ends_with(&format!("--{suffix}")),
        "{}",
        op.trash_path
    );
}
