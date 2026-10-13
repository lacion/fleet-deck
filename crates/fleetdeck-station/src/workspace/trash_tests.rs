//! The worktree trash (DL-150) with real `git`: the decisions, the effects
//! and the claim guard. The effects run by hand (no worker), so each test
//! controls what happens between the decision and the run.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use fleetdeck_proto::command::{
    CommandOrigin, RETRY_WINDOW_MS, ReceiptState, SessionCommand, SessionSpec,
};
use fleetdeck_proto::event::{Event, ProviderKind};
use fleetdeck_proto::link::{WorktreeInfo, WorktreeVerdict};
use fleetdeck_proto::{CommandEnvelope, CommandId, RejectCode};
use fleetdeck_testkit::Det;

use super::{SUPERSEDED, run};
use crate::manager::policy::Policy;
use crate::manager::trash::{self, HELD};
use crate::shell::{ShellConfig, Shells};
use crate::store::{
    Caller, Decided, Dispatch, NewSession, OfflineApprovalPolicy, Store, StoreConfig,
    WorktreeAction, WorktreeOp,
};
use crate::workspace::worktrees::{self, git_options};

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .current_dir(dir)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

struct Rig {
    det: Det,
    store: Store,
    policy: Policy,
    _root: tempfile::TempDir,
    base: PathBuf,
    main: PathBuf,
    wt: PathBuf,
    next: u32,
}

const SESSION: &str = "wt-session";

/// A station store with the ended session `wt-session` in the worktree
/// `repo--fd-x` (branch `fd/x`) of the repository `repo`.
async fn rig() -> Rig {
    let det = Det::new();
    let root = tempfile::tempdir().unwrap();
    let base = root.path().canonicalize().unwrap();
    let main = base.join("repo");
    std::fs::create_dir(&main).unwrap();
    git(&main, &["init", "-q", "-b", "main"]);
    std::fs::write(main.join("a.txt"), "one\n").unwrap();
    git(&main, &["add", "."]);
    git(&main, &["commit", "-q", "-m", "init"]);
    let wt = base.join("repo--fd-x");
    git(
        &main,
        &["worktree", "add", "-q", "-b", "fd/x", wt.to_str().unwrap()],
    );
    std::fs::write(wt.join("work.txt"), "unsaved work\n").unwrap();
    let store = Store::open_in_memory(det.clock(), det.ids(), StoreConfig::default())
        .await
        .unwrap();
    store.recover(Vec::new()).await.unwrap();
    let policy = Policy::new(
        vec![base.clone()],
        4,
        OfflineApprovalPolicy::Wait,
        BTreeMap::from([("claude".into(), ProviderKind::Claude)]),
    );
    let rig = Rig {
        det,
        store,
        policy,
        _root: root,
        base,
        main,
        wt,
        next: 0,
    };
    let wt = rig.wt.clone();
    rig.session(SESSION, &wt, true).await;
    rig
}

impl Rig {
    async fn session(&self, id: &str, cwd: &Path, ended: bool) {
        self.store
            .create_session(NewSession {
                session_id: id.into(),
                provider: ProviderKind::Claude,
                provider_instance: "claude".into(),
                cwd: cwd.display().to_string(),
                worktree: None,
                branch: None,
                mode: None,
                title: None,
                offline_approval_policy: OfflineApprovalPolicy::Wait,
            })
            .await
            .unwrap();
        let mut events: Vec<crate::store::NewEvent> = vec![
            Event::SessionNamed {
                callsign: id.to_owned(),
                worktree: Some(cwd.display().to_string()),
            }
            .into(),
        ];
        if ended {
            events.push(
                Event::SessionEnded {
                    reason: "process_exit".into(),
                    exit_code: Some(0),
                }
                .into(),
            );
        }
        self.store.append(id.into(), events).await.unwrap();
    }

    async fn command(&mut self, command: SessionCommand, origin: CommandOrigin) -> Decided {
        self.command_with(command, origin, None).await
    }

    async fn command_with(
        &mut self,
        command: SessionCommand,
        origin: CommandOrigin,
        shells: Option<Arc<Shells>>,
    ) -> Decided {
        self.next += 1;
        let envelope = CommandEnvelope {
            command_id: CommandId::from(format!("01JTRASH{:05}", self.next)),
            target_incarnation: None,
            retry_until: i64::try_from(self.det.now_ms()).unwrap() + RETRY_WINDOW_MS,
            origin,
            command: command.clone(),
        };
        let policy = self.policy.clone();
        self.store
            .decide(envelope, Caller::local(), move |d| {
                if matches!(
                    command,
                    SessionCommand::WorktreeTrash { .. }
                        | SessionCommand::WorktreeRestore { .. }
                        | SessionCommand::WorktreePurge { .. }
                        | SessionCommand::WorktreeRelease { .. }
                ) {
                    trash::decide(d, &policy, origin, shells.as_deref(), command)
                } else {
                    crate::manager::decide::decide(d, &policy, origin, command)
                }
            })
            .await
            .unwrap()
    }

    async fn ok(&mut self, command: SessionCommand) -> String {
        let d = self.command(command, CommandOrigin::Board).await;
        assert_eq!(d.receipt.state, ReceiptState::Accepted, "{:?}", d.receipt);
        d.receipt.command_id.as_str().to_owned()
    }

    async fn refused(&mut self, command: SessionCommand) -> (RejectCode, String) {
        let d = self.command(command, CommandOrigin::Board).await;
        assert_eq!(d.receipt.state, ReceiptState::Rejected, "{:?}", d.receipt);
        let r = d.receipt.reject.unwrap();
        (r.code, r.message)
    }

    fn trash_cmd() -> SessionCommand {
        SessionCommand::WorktreeTrash {
            session_id: SESSION.into(),
        }
    }

    fn restore_cmd() -> SessionCommand {
        SessionCommand::WorktreeRestore {
            session_id: SESSION.into(),
        }
    }

    fn purge_cmd(name: &str) -> SessionCommand {
        SessionCommand::WorktreePurge {
            session_id: SESSION.into(),
            confirm_name: name.to_owned(),
        }
    }

    fn release_cmd() -> SessionCommand {
        SessionCommand::WorktreeRelease {
            session_id: SESSION.into(),
        }
    }

    /// Runs the effect by hand, then settles the queued effect rows the
    /// way the worker does.
    async fn run(&self, op: &str, action: WorktreeAction) -> Dispatch {
        let outcome = run(&self.store, &self.policy, &git_options(), None, op, action).await;
        let fence = self.store.register_worker().await.unwrap();
        while let Some(effect) = self.store.claim_next(fence).await.unwrap() {
            self.store
                .settle(effect.effect_id, fence, Dispatch::Confirmed)
                .await
                .unwrap();
        }
        outcome
    }

    async fn row(&self, op: &str) -> WorktreeOp {
        self.store
            .worktree_op(op.to_owned())
            .await
            .unwrap()
            .unwrap()
    }

    fn listed(&self) -> String {
        git(&self.main, &["worktree", "list", "--porcelain"])
    }

    async fn inventory(&self) -> Vec<WorktreeInfo> {
        worktrees::inventory(&self.store, &self.policy, &git_options())
            .await
            .unwrap()
            .worktrees
    }

    /// Trash the worktree and run the effect.
    async fn trashed(&mut self) -> (String, WorktreeOp) {
        let op = self.ok(Self::trash_cmd()).await;
        assert_eq!(
            self.run(&op, WorktreeAction::Trash).await,
            Dispatch::Confirmed
        );
        let row = self.row(&op).await;
        assert_eq!(row.state, "trashed", "{row:?}");
        (op, row)
    }
}

#[tokio::test]
async fn a_trash_moves_the_folder_under_the_trash_folder_and_git_follows() {
    let mut r = rig().await;
    let op = r.ok(Rig::trash_cmd()).await;
    // The decision only stores the row; nothing moved yet.
    let row = r.row(&op).await;
    assert_eq!(row.state, "trashing");
    assert!(r.wt.is_dir());
    assert_eq!(
        Path::new(&row.trash_path),
        r.base
            .join(".fleetdeck-trash")
            .join("repo--fd-x--01jtrash0000")
    );
    assert_eq!(r.run(&op, WorktreeAction::Trash).await, Dispatch::Confirmed);
    let row = r.row(&op).await;
    assert_eq!(row.state, "trashed", "{row:?}");
    assert_eq!(row.branch.as_deref(), Some("fd/x"));
    assert_eq!(Path::new(&row.main), r.main);
    assert!(!r.wt.exists());
    let trash = PathBuf::from(&row.trash_path);
    assert_eq!(
        std::fs::read_to_string(trash.join("work.txt")).unwrap(),
        "unsaved work\n"
    );
    let listed = r.listed();
    assert!(
        listed.contains(&format!("worktree {}", trash.display())),
        "{listed}"
    );
    assert!(!listed.contains(&format!("worktree {}\n", r.wt.display())));
    // The inventory shows the trash and does not scan the folder.
    let inv = r.inventory().await;
    assert_eq!(inv.len(), 1);
    assert_eq!(inv[0].verdict, WorktreeVerdict::Trashed);
    let info = inv[0].trash.as_ref().unwrap();
    assert_eq!(info.state, "trashed");
    assert_eq!(info.trash_path, trash.display().to_string());
    assert_eq!(info.phase, None);
}

#[tokio::test]
async fn a_restore_moves_the_folder_back_and_git_lists_it_again() {
    let mut r = rig().await;
    let (_, row) = r.trashed().await;
    let op = r.ok(Rig::restore_cmd()).await;
    assert_eq!(r.row(&row.op_id).await.state, "restoring");
    assert_eq!(
        r.run(&row.op_id, WorktreeAction::Restore).await,
        Dispatch::Confirmed
    );
    let _ = op;
    assert_eq!(r.row(&row.op_id).await.state, "restored");
    assert_eq!(
        std::fs::read_to_string(r.wt.join("work.txt")).unwrap(),
        "unsaved work\n"
    );
    assert!(!Path::new(&row.trash_path).exists());
    assert!(r.listed().contains(&format!("worktree {}", r.wt.display())));
    // The hold ended: the inventory scans the folder again.
    let inv = r.inventory().await;
    assert_eq!(inv[0].trash, None);
    assert_ne!(inv[0].verdict, WorktreeVerdict::Trashed);
    // A new trash is possible.
    let (_, again) = r.trashed().await;
    assert!(Path::new(&again.trash_path).is_dir());
}

#[tokio::test]
async fn a_purge_needs_the_folder_name_and_deletes_the_folder_and_the_git_record() {
    let mut r = rig().await;
    let (_, row) = r.trashed().await;
    let name = Path::new(&row.trash_path)
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();
    let (code, message) = r.refused(Rig::purge_cmd("repo--fd-x")).await;
    assert_eq!(code, RejectCode::InvalidCommand);
    assert_eq!(message, "type the folder name to confirm");
    assert_eq!(r.row(&row.op_id).await.state, "trashed");
    assert!(Path::new(&row.trash_path).is_dir());

    r.ok(Rig::purge_cmd(&name)).await;
    assert_eq!(r.row(&row.op_id).await.state, "purging");
    assert_eq!(
        r.run(&row.op_id, WorktreeAction::Purge).await,
        Dispatch::Confirmed
    );
    assert_eq!(r.row(&row.op_id).await.state, "purged");
    assert!(!Path::new(&row.trash_path).exists());
    assert!(!Path::new(&row.admin_dir).exists());
    // The branch stays.
    assert!(git(&r.main, &["branch", "--list", "fd/x"]).contains("fd/x"));
    assert!(!r.listed().contains("repo--fd-x"));
    // The hold ended; the session shows a missing folder, as before.
    let inv = r.inventory().await;
    assert_eq!(inv[0].verdict, WorktreeVerdict::Missing);
    assert_eq!(inv[0].trash, None);
}

#[tokio::test]
async fn a_start_resume_shell_open_and_handover_are_refused_while_a_row_is_open() {
    let mut r = rig().await;
    let (_, row) = r.trashed().await;
    // Resume of the ended session.
    let (code, message) = r
        .refused(SessionCommand::Resume {
            session_id: SESSION.into(),
        })
        .await;
    assert_eq!((code, message.as_str()), (RejectCode::InvalidState, HELD));
    // A start into the original folder and into the trash folder.
    for cwd in [
        r.wt.display().to_string(),
        format!("{}/sub", row.trash_path),
    ] {
        let (code, message) = r
            .refused(SessionCommand::Start {
                spec: SessionSpec {
                    provider_instance: "claude".into(),
                    cwd,
                    mode: None,
                    model: None,
                    title: None,
                    coordinator: None,
                },
                prompt: None,
                workspace: None,
                setup_cmd: None,
            })
            .await;
        assert_eq!((code, message.as_str()), (RejectCode::InvalidState, HELD));
    }
    // A shell open and a handover.
    let shells = Arc::new(Shells::new(ShellConfig::default()));
    for change in [
        fleetdeck_proto::shell::ShellChange::Open {
            shell_id: "sh1".into(),
            cwd: r.wt.display().to_string(),
            cols: 80,
            rows: 24,
        },
        fleetdeck_proto::shell::ShellChange::Open {
            shell_id: "sh2".into(),
            cwd: row.trash_path.clone(),
            cols: 80,
            rows: 24,
        },
        fleetdeck_proto::shell::ShellChange::Handover {
            shell_id: "sh3".into(),
            session_id: SESSION.into(),
            cols: 80,
            rows: 24,
        },
    ] {
        let envelope = fleetdeck_proto::shell::ShellEnvelope {
            command_id: CommandId::from(format!("01JSHELL{}", change.shell_id())),
            target_incarnation: r.store.incarnation().clone(),
            retry_until: i64::try_from(r.det.now_ms()).unwrap() + RETRY_WINDOW_MS,
            change,
        };
        let policy = r.policy.clone();
        let (service, env) = (Arc::clone(&shells), envelope.clone());
        let decided = r
            .store
            .decide_mutation(
                fleetdeck_proto::mutation::Mutation::Shell(envelope),
                Caller::local(),
                move |d| crate::shell::decide(d, &service, &policy, &env),
            )
            .await
            .unwrap();
        assert_eq!(decided.receipt.state, ReceiptState::Rejected);
        let reject = decided.receipt.reject.unwrap();
        assert_eq!(reject.code, RejectCode::InvalidState);
        assert_eq!(reject.message, HELD);
    }
    // After the restore the hold ends.
    r.ok(Rig::restore_cmd()).await;
    assert_eq!(
        r.run(&row.op_id, WorktreeAction::Restore).await,
        Dispatch::Confirmed
    );
    assert!(
        r.store
            .decide(
                CommandEnvelope {
                    command_id: CommandId::from("01JTRASHFREE1"),
                    target_incarnation: None,
                    retry_until: i64::MAX,
                    origin: CommandOrigin::Board,
                    command: SessionCommand::Rename {
                        session_id: SESSION.into(),
                        title: None,
                    },
                },
                Caller::local(),
                |_| Ok(Default::default()),
            )
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn a_live_or_busy_session_and_a_shell_in_the_folder_refuse_a_trash() {
    let mut r = rig().await;
    // A live session whose directory is inside the worktree.
    let sub = r.wt.join("sub");
    std::fs::create_dir(&sub).unwrap();
    r.session("other", &sub, false).await;
    let (code, message) = r.refused(Rig::trash_cmd()).await;
    assert_eq!(code, RejectCode::InvalidState);
    assert!(message.contains("other"), "{message}");
    // The session itself must have ended.
    let (code, _) = r
        .refused(SessionCommand::WorktreeTrash {
            session_id: "other".into(),
        })
        .await;
    assert_eq!(code, RejectCode::InvalidState);
    // A session without a worktree.
    r.store
        .create_session(NewSession {
            session_id: "plain".into(),
            provider: ProviderKind::Claude,
            provider_instance: "claude".into(),
            cwd: r.main.display().to_string(),
            worktree: None,
            branch: None,
            mode: None,
            title: None,
            offline_approval_policy: OfflineApprovalPolicy::Wait,
        })
        .await
        .unwrap();
    let end: crate::store::NewEvent = Event::SessionEnded {
        reason: "process_exit".into(),
        exit_code: Some(0),
    }
    .into();
    r.store.append("plain".into(), vec![end]).await.unwrap();
    let (_, message) = r
        .refused(SessionCommand::WorktreeTrash {
            session_id: "plain".into(),
        })
        .await;
    assert_eq!(message, "the session has no worktree of its own");
    // Only the board may ask.
    let d = r
        .command(Rig::trash_cmd(), CommandOrigin::Coordinator)
        .await;
    assert_eq!(d.receipt.reject.unwrap().code, RejectCode::Forbidden);
    let d = r.command(Rig::trash_cmd(), CommandOrigin::Local).await;
    assert_eq!(d.receipt.reject.unwrap().code, RejectCode::Forbidden);
    // The main checkout is not a linked worktree.
    let main = r.main.clone();
    r.session("main-session", &main, true).await;
    let (_, message) = r
        .refused(SessionCommand::WorktreeTrash {
            session_id: "main-session".into(),
        })
        .await;
    assert_eq!(message, "this is not a linked worktree");
}

#[tokio::test]
async fn a_changed_folder_identity_fails_the_trash_and_moves_nothing() {
    let mut r = rig().await;
    let op = r.ok(Rig::trash_cmd()).await;
    // The owner replaces the folder between the decision and the effect.
    std::fs::rename(&r.wt, r.base.join("moved-away")).unwrap();
    std::fs::create_dir(&r.wt).unwrap();
    assert_eq!(r.run(&op, WorktreeAction::Trash).await, Dispatch::Confirmed);
    let row = r.row(&op).await;
    assert_eq!(row.state, "failed", "{row:?}");
    assert_eq!(row.phase.as_deref(), Some("trash"));
    assert!(
        row.message
            .as_deref()
            .unwrap()
            .contains("changed during the move"),
        "{row:?}"
    );
    assert!(r.wt.is_dir());
    assert!(r.base.join("moved-away/work.txt").is_file());
    assert!(!Path::new(&row.trash_path).exists());
    // The inventory keeps the normal scan and adds the trash facts.
    let inv = r.inventory().await;
    assert_eq!(inv[0].trash.as_ref().unwrap().state, "failed");
    assert_ne!(inv[0].verdict, WorktreeVerdict::Trashed);
}

#[tokio::test]
async fn a_release_is_accepted_only_for_a_failed_row() {
    let mut r = rig().await;
    let (code, _) = r.refused(Rig::release_cmd()).await;
    assert_eq!(code, RejectCode::InvalidState);
    let (op, _) = r.trashed().await;
    let (code, message) = r.refused(Rig::release_cmd()).await;
    assert_eq!(code, RejectCode::InvalidState);
    assert_eq!(message, "only a failed move can be released");
    // A purging row cannot be released either.
    let row = r.row(&op).await;
    let name = Path::new(&row.trash_path)
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();
    r.ok(Rig::purge_cmd(&name)).await;
    let (code, _) = r.refused(Rig::release_cmd()).await;
    assert_eq!(code, RejectCode::InvalidState);
    assert_eq!(r.row(&op).await.state, "purging");
}

/// The r3 blocker of the design review: a purge is admitted, the row is
/// forced to `failed` and released before the effect runs. The effect must
/// end `superseded` and delete nothing.
#[tokio::test]
async fn a_queued_purge_after_a_release_is_superseded_and_deletes_nothing() {
    let mut r = rig().await;
    let (op, row) = r.trashed().await;
    let name = Path::new(&row.trash_path)
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();
    r.ok(Rig::purge_cmd(&name)).await;
    assert_eq!(r.row(&op).await.state, "purging");
    // The effect is queued but has not run: force the row to `failed`.
    r.store
        .worktree_op_fail(op.clone(), "purging", "purge", "forced by the test".into())
        .await
        .unwrap();
    r.ok(Rig::release_cmd()).await;
    assert_eq!(r.row(&op).await.state, "released");
    // A shell now opens in the trash folder and writes a file.
    std::fs::write(Path::new(&row.trash_path).join("new-work.txt"), "new\n").unwrap();
    assert_eq!(
        r.run(&op, WorktreeAction::Purge).await,
        Dispatch::NotDispatched(SUPERSEDED.into())
    );
    assert_eq!(r.row(&op).await.state, "released");
    assert!(Path::new(&row.trash_path).join("new-work.txt").is_file());
    assert!(Path::new(&row.admin_dir).is_dir());
    // The same for the other two actions on a released row.
    assert_eq!(
        r.run(&op, WorktreeAction::Trash).await,
        Dispatch::NotDispatched(SUPERSEDED.into())
    );
    assert_eq!(
        r.run(&op, WorktreeAction::Restore).await,
        Dispatch::NotDispatched(SUPERSEDED.into())
    );
}

#[tokio::test]
async fn a_replaced_trash_folder_is_not_deleted() {
    let mut r = rig().await;
    let (op, row) = r.trashed().await;
    let name = Path::new(&row.trash_path)
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();
    r.ok(Rig::purge_cmd(&name)).await;
    std::fs::rename(&row.trash_path, r.base.join("kept")).unwrap();
    std::fs::create_dir(&row.trash_path).unwrap();
    std::fs::write(Path::new(&row.trash_path).join("other.txt"), "x").unwrap();
    assert_eq!(r.run(&op, WorktreeAction::Purge).await, Dispatch::Confirmed);
    let after = r.row(&op).await;
    assert_eq!(
        (after.state.as_str(), after.phase.as_deref()),
        ("failed", Some("purge"))
    );
    assert_eq!(
        after.message.as_deref(),
        Some("the trash folder changed; nothing was deleted")
    );
    assert!(Path::new(&row.trash_path).join("other.txt").is_file());
    assert!(r.base.join("kept/work.txt").is_file());
    // Restore is not offered for a failed purge; a release is.
    let (code, _) = r.refused(Rig::restore_cmd()).await;
    assert_eq!(code, RejectCode::InvalidState);
    r.ok(Rig::release_cmd()).await;
}

/// A crash between the rename and the repair: the folder is in the trash,
/// the row is still `trashing`, and a restart queues a new effect.
#[tokio::test]
async fn a_restart_finishes_a_trash_that_stopped_between_the_rename_and_the_repair() {
    let mut r = rig().await;
    let op = r.ok(Rig::trash_cmd()).await;
    let row = r.row(&op).await;
    std::fs::create_dir_all(Path::new(&row.trash_path).parent().unwrap()).unwrap();
    std::fs::rename(&r.wt, &row.trash_path).unwrap();
    assert_eq!(r.row(&op).await.state, "trashing");
    assert_eq!(r.store.requeue_worktree_ops().await.unwrap(), 1);
    let effects = r.store.session_effects(SESSION.into()).await.unwrap();
    assert!(effects.iter().any(|e| matches!(
        &e.payload,
        crate::store::EffectPayload::Worktree { op_id, action }
            if *op_id == op && *action == WorktreeAction::Trash
    )));
    assert_eq!(r.run(&op, WorktreeAction::Trash).await, Dispatch::Confirmed);
    let after = r.row(&op).await;
    assert_eq!(after.state, "trashed", "{after:?}");
    assert!(r.listed().contains(&format!("worktree {}", row.trash_path)));
    // A second run of the same effect is superseded.
    assert_eq!(
        r.run(&op, WorktreeAction::Trash).await,
        Dispatch::NotDispatched(SUPERSEDED.into())
    );
}

#[tokio::test]
async fn a_trash_that_failed_before_git_was_read_restores_without_a_repair() {
    let mut r = rig().await;
    let op = r.ok(Rig::trash_cmd()).await;
    r.store
        .worktree_op_fail(op.clone(), "trashing", "trash", "forced".into())
        .await
        .unwrap();
    // A failed row also refuses a new trash and a purge.
    let (code, _) = r.refused(Rig::trash_cmd()).await;
    assert_eq!(code, RejectCode::InvalidState);
    let (code, _) = r.refused(Rig::purge_cmd("x")).await;
    assert_eq!(code, RejectCode::InvalidState);
    r.ok(Rig::restore_cmd()).await;
    assert_eq!(
        r.run(&op, WorktreeAction::Restore).await,
        Dispatch::Confirmed
    );
    assert_eq!(r.row(&op).await.state, "restored");
    assert!(r.wt.join("work.txt").is_file());
}

#[tokio::test]
async fn an_open_shell_in_the_folder_refuses_a_trash_and_a_purge_and_fails_the_effect() {
    let mut r = rig().await;
    let shells = Arc::new(Shells::new(ShellConfig::default()));
    shells
        .execute(
            &fleetdeck_proto::shell::ShellChange::Open {
                shell_id: "sh1".into(),
                cwd: r.wt.display().to_string(),
                cols: 80,
                rows: 24,
            },
            &r.policy,
            0,
        )
        .unwrap();
    let d = r
        .command_with(
            Rig::trash_cmd(),
            CommandOrigin::Board,
            Some(Arc::clone(&shells)),
        )
        .await;
    let reject = d.receipt.reject.unwrap();
    assert_eq!(reject.code, RejectCode::InvalidState);
    assert!(
        reject.message.contains("a shell is open"),
        "{}",
        reject.message
    );
    // The effect checks again: a shell that opened after the decision.
    let op = r.ok(Rig::trash_cmd()).await;
    let outcome = run(
        &r.store,
        &r.policy,
        &git_options(),
        Some(&shells),
        &op,
        WorktreeAction::Trash,
    )
    .await;
    assert_eq!(outcome, Dispatch::Confirmed);
    let row = r.row(&op).await;
    assert_eq!(
        (row.state.as_str(), row.phase.as_deref()),
        ("failed", Some("trash"))
    );
    assert!(r.wt.is_dir(), "nothing moved");
    shells
        .execute(
            &fleetdeck_proto::shell::ShellChange::Close {
                shell_id: "sh1".into(),
            },
            &r.policy,
            0,
        )
        .unwrap();
}
