//! The revert executor (DL-087) against real Git and the store, with a
//! scripted provider session: the conversation rollback comes before any
//! file change, a refusal or a changed workspace changes nothing, an
//! uncertain rollback needs reconciliation, a writer during the rollback
//! makes the restore stop, and start recovery is safe at every step.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use async_trait::async_trait;
use fleetdeck_proto::command::{
    CommandEnvelope, CommandOrigin, CommandResult, SessionCommand, StopMode,
};
use fleetdeck_proto::event::{InputState, RevertState};
use fleetdeck_proto::{Capabilities, CommandId, Event, EventBody};
use fleetdeck_providers::adapter::Result as ProviderResult;
use fleetdeck_providers::{
    Answer, ControlId, HostInput, LiveSession, ModelId, NativeSessionId, PermissionMode,
    ProviderError,
};
use fleetdeck_testkit::Det;

use crate::manager::revert_run::{self, RevertEnv, reason};
use crate::manager::turns::SessionTurns;
use crate::manager::turns::turns_tests::{
    S1, completed, dispatch, git, input, key, repo, rows, setup, started,
};
use crate::store::{
    Caller, Dispatch, NewRevert, ReadResult, RevertStep, RowSide, Store, TurnCheckpointRow,
};
use crate::workspace::{GitOptions, Limits, Phase, Worktree};

#[derive(Clone)]
enum Rollback {
    Ok,
    Refuse,
    Uncertain,
    /// Confirms, but a writer changes a tracked file meanwhile.
    OkWhileWriting(std::path::PathBuf),
}

struct Scripted {
    caps: Capabilities,
    rollback: Rollback,
    calls: Arc<AtomicU32>,
}

impl Scripted {
    fn new(rollback: Rollback) -> (Self, Arc<AtomicU32>) {
        let calls = Arc::new(AtomicU32::new(0));
        let mut caps = Capabilities::minimal();
        caps.rollback = true;
        (
            Self {
                caps,
                rollback,
                calls: Arc::clone(&calls),
            },
            calls,
        )
    }
}

fn unused<T>() -> ProviderResult<T> {
    Err(ProviderError::NotReady("not used by the revert tests"))
}

#[async_trait]
impl LiveSession for Scripted {
    fn capabilities(&self) -> &Capabilities {
        &self.caps
    }
    async fn prompt(&mut self, _: HostInput) -> ProviderResult<()> {
        unused()
    }
    async fn steer(&mut self, _: HostInput) -> ProviderResult<()> {
        unused()
    }
    async fn follow_up(&mut self, _: HostInput) -> ProviderResult<()> {
        unused()
    }
    async fn interrupt(&mut self, _: Option<ControlId>) -> ProviderResult<()> {
        unused()
    }
    async fn answer(&mut self, _: Answer) -> ProviderResult<()> {
        unused()
    }
    async fn set_mode(&mut self, _: PermissionMode) -> ProviderResult<()> {
        unused()
    }
    async fn set_model(&mut self, _: ModelId) -> ProviderResult<()> {
        unused()
    }
    async fn fork(&mut self) -> ProviderResult<NativeSessionId> {
        unused()
    }
    async fn rollback(&mut self, turns: u32) -> ProviderResult<()> {
        assert!(turns > 0);
        self.calls.fetch_add(1, Ordering::SeqCst);
        match &self.rollback {
            Rollback::Ok => Ok(()),
            Rollback::Refuse => Err(ProviderError::Refused("no such turn".into())),
            Rollback::Uncertain => Err(ProviderError::Uncertain("no answer".into())),
            Rollback::OkWhileWriting(path) => {
                std::fs::write(path, "a writer\n").unwrap();
                Ok(())
            }
        }
    }
    async fn stop(&mut self, _: StopMode) -> ProviderResult<()> {
        Ok(())
    }
}

/// One session with two finalized turns: T1 writes `a.txt`, T2 writes
/// `b.txt`.
struct Rig {
    det: Det,
    _tmp: tempfile::TempDir,
    dir: std::path::PathBuf,
    store: Store,
    turns: Arc<SessionTurns>,
    git: GitOptions,
}

impl Rig {
    async fn new() -> Self {
        let det = Det::new();
        let (tmp, dir) = repo();
        let (store, turns) = setup(&det, &dir).await;
        let rig = Self {
            det,
            _tmp: tmp,
            dir,
            store,
            turns,
            git: GitOptions::isolated(),
        };
        rig.turn("in-1", "turn-1", "a.txt", "one\nT1\n").await;
        rig.turn("in-2", "turn-2", "b.txt", "two\nT2\n").await;
        rig
    }

    async fn turn(&self, input_id: &str, turn: &str, file: &str, text: &str) {
        dispatch(&self.turns, input_id).await;
        self.turns.observe(&[started(turn)]).await;
        std::fs::write(self.dir.join(file), text).unwrap();
        let due = self
            .turns
            .observe(&[
                input(input_id, Some(turn), InputState::Started),
                completed(turn),
            ])
            .await;
        assert!(due);
        Arc::clone(&self.turns).capture_due().await;
    }

    fn env(&self) -> RevertEnv<'_> {
        RevertEnv {
            store: &self.store,
            git: &self.git,
            limits: Limits::default(),
        }
    }

    async fn rows(&self) -> Vec<TurnCheckpointRow> {
        rows(&self.store).await
    }

    /// Admits a revert of `turn-1` (both turns) directly in the store.
    async fn admit(&self) -> String {
        let rows = self.rows().await;
        let commit = |side: &RowSide| match side {
            RowSide::Available(c) => c.clone(),
            other => panic!("{other:?}"),
        };
        let new = NewRevert {
            revert_id: "rv-1".into(),
            session_id: S1.into(),
            process: "proc-1".to_owned().into(),
            turn_id: "turn-1".into(),
            app_turn: rows[0].app_turn.clone(),
            current_app_turn: rows[1].app_turn.clone(),
            turns: 2,
            target_commit: commit(&rows[0].start),
            current_commit: commit(&rows[1].end),
            worktree_root: self.dir.display().to_string(),
        };
        let envelope = CommandEnvelope {
            command_id: CommandId::from("c-revert"),
            target_incarnation: None,
            retry_until: i64::try_from(self.det.now_ms()).unwrap() + 60_000,
            origin: CommandOrigin::Board,
            command: SessionCommand::Revert {
                session_id: S1.into(),
                turn_id: "turn-1".into(),
            },
        };
        self.store
            .decide(envelope, Caller::local(), move |d| {
                d.admit_revert(&new)?;
                Ok(CommandResult {
                    session_id: Some(S1.into()),
                    input_id: None,
                })
            })
            .await
            .unwrap();
        "rv-1".into()
    }

    async fn run(&self, session: &mut Scripted) -> Dispatch {
        revert_run::run(
            self.env(),
            &self.turns,
            session,
            &"proc-1".to_owned().into(),
            "rv-1",
        )
        .await
    }

    async fn step(&self) -> (RevertStep, Option<String>) {
        let row = self.store.revert("rv-1".into()).await.unwrap().unwrap();
        (row.step, row.reason)
    }

    fn file(&self, name: &str) -> String {
        std::fs::read_to_string(self.dir.join(name)).unwrap()
    }

    fn refs(&self) -> String {
        let out = std::process::Command::new("git")
            .current_dir(&self.dir)
            .args(["for-each-ref", "--format=%(refname) %(objectname)"])
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .output()
            .unwrap();
        String::from_utf8(out.stdout).unwrap()
    }

    async fn states(&self) -> Vec<(RevertState, Option<String>)> {
        let ReadResult::Events { events, .. } = self.store.read_after(0, 10_000).await.unwrap()
        else {
            panic!("snapshot");
        };
        events
            .into_iter()
            .filter_map(|e| match e.event {
                EventBody::Known(Event::RevertUpdated { state, reason, .. }) => {
                    Some((state, reason))
                }
                _ => None,
            })
            .collect()
    }

    async fn reverted(&self) -> Vec<Option<String>> {
        let mut out = Vec::new();
        for row in self.rows().await {
            out.push(self.store.reverted_by(row.app_turn).await.unwrap());
        }
        out
    }
}

#[tokio::test]
async fn a_confirmed_rollback_restores_the_start_checkpoint_of_the_target_turn() {
    let rig = Rig::new().await;
    let refs = rig.refs();
    rig.admit().await;
    let (mut session, calls) = Scripted::new(Rollback::Ok);
    assert_eq!(rig.run(&mut session).await, Dispatch::Confirmed);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(rig.file("a.txt"), "one\n");
    assert_eq!(rig.file("b.txt"), "two\n");
    assert_eq!(rig.step().await, (RevertStep::Succeeded, None));
    assert_eq!(
        rig.states().await,
        vec![
            (RevertState::Admitted, None),
            (RevertState::ConversationRolledBack, None),
            (RevertState::Succeeded, None),
        ]
    );
    // The checkpoint refs stay; the turns leave the revertable history.
    assert_eq!(rig.refs(), refs);
    assert_eq!(
        rig.reverted().await,
        vec![Some("rv-1".to_owned()), Some("rv-1".to_owned())]
    );
    // A second run of the same effect changes nothing.
    let (mut again, calls) = Scripted::new(Rollback::Ok);
    assert!(matches!(
        rig.run(&mut again).await,
        Dispatch::NotDispatched(_)
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn a_refused_rollback_changes_no_file_and_no_ref() {
    let rig = Rig::new().await;
    let refs = rig.refs();
    rig.admit().await;
    let (mut session, calls) = Scripted::new(Rollback::Refuse);
    let Dispatch::NotDispatched(why) = rig.run(&mut session).await else {
        panic!("a refusal is a definite non-dispatch");
    };
    assert!(why.starts_with(reason::ROLLBACK_REFUSED), "{why}");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(rig.file("a.txt"), "one\nT1\n");
    assert_eq!(rig.file("b.txt"), "two\nT2\n");
    assert_eq!(rig.refs(), refs);
    assert_eq!(rig.step().await.0, RevertStep::Failed);
    assert_eq!(rig.reverted().await, vec![None, None]);
}

#[tokio::test]
async fn an_uncertain_rollback_requires_reconciliation_and_restores_nothing() {
    let rig = Rig::new().await;
    rig.admit().await;
    let (mut session, _) = Scripted::new(Rollback::Uncertain);
    assert_eq!(
        rig.run(&mut session).await,
        Dispatch::Unknown(reason::ROLLBACK_UNCERTAIN.into())
    );
    assert_eq!(rig.file("a.txt"), "one\nT1\n");
    assert_eq!(
        rig.step().await,
        (
            RevertStep::ReconciliationRequired,
            Some(reason::ROLLBACK_UNCERTAIN.into())
        )
    );
    // The summary blocks input until the owner reconciles.
    let summary = rig.store.summary(S1.into()).await.unwrap().unwrap();
    assert!(summary.revert.unwrap().blocks_input());
}

#[tokio::test]
async fn a_workspace_change_after_the_last_turn_fails_the_revert_before_the_rollback() {
    let rig = Rig::new().await;
    rig.admit().await;
    std::fs::write(rig.dir.join("a.txt"), "owner edit\n").unwrap();
    let (mut session, calls) = Scripted::new(Rollback::Ok);
    assert_eq!(
        rig.run(&mut session).await,
        Dispatch::NotDispatched(reason::WORKSPACE_CHANGED.into())
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0, "no rollback was sent");
    assert_eq!(rig.file("a.txt"), "owner edit\n");
    assert_eq!(rig.step().await.0, RevertStep::Failed);
}

#[tokio::test]
async fn a_changed_checkpoint_ref_fails_the_revert_before_the_rollback() {
    let rig = Rig::new().await;
    rig.admit().await;
    let row = rig.rows().await.remove(0);
    let name = Worktree::open(&rig.dir, GitOptions::isolated())
        .await
        .unwrap()
        .read(&key(&rig.store, &row, Phase::Start))
        .await
        .unwrap()
        .unwrap()
        .ref_name;
    git(&rig.dir, &["update-ref", &name, "HEAD"]);
    let (mut session, calls) = Scripted::new(Rollback::Ok);
    assert_eq!(
        rig.run(&mut session).await,
        Dispatch::NotDispatched(reason::CHECKPOINT_CHANGED.into())
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(rig.file("a.txt"), "one\nT1\n");
}

#[tokio::test]
async fn a_writer_during_the_rollback_stops_the_restore_with_reconciliation() {
    let rig = Rig::new().await;
    rig.admit().await;
    let path = rig.dir.join("a.txt");
    let (mut session, _) = Scripted::new(Rollback::OkWhileWriting(path));
    assert_eq!(
        rig.run(&mut session).await,
        Dispatch::Unknown(reason::WORKSPACE_CHANGED.into())
    );
    // The writer's content stays; nothing was overwritten.
    assert_eq!(rig.file("a.txt"), "a writer\n");
    assert_eq!(rig.file("b.txt"), "two\nT2\n");
    assert_eq!(
        rig.states().await.last().cloned(),
        Some((
            RevertState::ReconciliationRequired,
            Some(reason::WORKSPACE_CHANGED.into())
        ))
    );
}

async fn recover_from(rig: &Rig, at: RevertStep) {
    rig.admit().await;
    let path: &'static [RevertStep] = match at {
        RevertStep::Admitted => &[],
        RevertStep::RollbackSent => &[RevertStep::RollbackSent],
        RevertStep::RolledBack => &[RevertStep::RollbackSent, RevertStep::RolledBack],
        RevertStep::Restoring => &[
            RevertStep::RollbackSent,
            RevertStep::RolledBack,
            RevertStep::Restoring,
        ],
        other => panic!("{other:?}"),
    };
    let mut from = RevertStep::Admitted;
    for to in path {
        let from_steps: &'static [RevertStep] = match from {
            RevertStep::Admitted => &[RevertStep::Admitted],
            RevertStep::RollbackSent => &[RevertStep::RollbackSent],
            RevertStep::RolledBack => &[RevertStep::RolledBack],
            _ => &[],
        };
        assert!(
            rig.store
                .advance_revert("rv-1".into(), from_steps, *to, None)
                .await
                .unwrap()
        );
        from = *to;
    }
    assert_eq!(revert_run::recover(rig.env()).await.unwrap(), 1);
}

#[tokio::test]
async fn recovery_of_an_admitted_revert_fails_it_without_any_change() {
    let rig = Rig::new().await;
    recover_from(&rig, RevertStep::Admitted).await;
    assert_eq!(
        rig.step().await,
        (RevertStep::Failed, Some(reason::STATION_RESTART.into()))
    );
    assert_eq!(rig.file("a.txt"), "one\nT1\n");
}

#[tokio::test]
async fn recovery_after_a_sent_rollback_never_repeats_it_and_requires_reconciliation() {
    let rig = Rig::new().await;
    recover_from(&rig, RevertStep::RollbackSent).await;
    assert_eq!(
        rig.step().await,
        (
            RevertStep::ReconciliationRequired,
            Some(reason::ROLLBACK_UNCERTAIN.into())
        )
    );
    assert_eq!(rig.file("a.txt"), "one\nT1\n");
    assert_eq!(rig.file("b.txt"), "two\nT2\n");
}

#[tokio::test]
async fn recovery_after_a_confirmed_rollback_finishes_the_restore() {
    let rig = Rig::new().await;
    recover_from(&rig, RevertStep::RolledBack).await;
    assert_eq!(rig.step().await, (RevertStep::Succeeded, None));
    assert_eq!(rig.file("a.txt"), "one\n");
    assert_eq!(rig.file("b.txt"), "two\n");
}

#[tokio::test]
async fn recovery_during_the_restore_finishes_a_complete_one_and_stops_a_partial_one() {
    // The crash came after the last write: the workspace is the target.
    let rig = Rig::new().await;
    std::fs::write(rig.dir.join("a.txt"), "one\n").unwrap();
    std::fs::write(rig.dir.join("b.txt"), "two\n").unwrap();
    recover_from(&rig, RevertStep::Restoring).await;
    assert_eq!(rig.step().await, (RevertStep::Succeeded, None));
    // The crash came between two writes: neither checkpoint matches, so
    // the station cannot tell it from a foreign writer and stops.
    let rig = Rig::new().await;
    std::fs::write(rig.dir.join("a.txt"), "one\n").unwrap();
    recover_from(&rig, RevertStep::Restoring).await;
    assert_eq!(
        rig.step().await,
        (
            RevertStep::ReconciliationRequired,
            Some(reason::WORKSPACE_CHANGED.into())
        )
    );
    assert_eq!(rig.file("b.txt"), "two\nT2\n");
}

#[tokio::test]
async fn a_second_open_revert_of_the_session_is_refused_by_the_store() {
    let rig = Rig::new().await;
    rig.admit().await;
    let rows = rig.rows().await;
    let envelope = CommandEnvelope {
        command_id: CommandId::from("c-other"),
        target_incarnation: None,
        retry_until: i64::try_from(rig.det.now_ms()).unwrap() + 60_000,
        origin: CommandOrigin::Board,
        command: SessionCommand::Revert {
            session_id: S1.into(),
            turn_id: "turn-2".into(),
        },
    };
    let row = rows[1].clone();
    let dir = rig.dir.display().to_string();
    let result = rig
        .store
        .decide(envelope, Caller::local(), move |d| {
            d.admit_revert(&NewRevert {
                revert_id: "rv-2".into(),
                session_id: S1.into(),
                process: "proc-1".to_owned().into(),
                turn_id: "turn-2".into(),
                app_turn: row.app_turn.clone(),
                current_app_turn: row.app_turn.clone(),
                turns: 1,
                target_commit: String::new(),
                current_commit: String::new(),
                worktree_root: dir,
            })?;
            Ok(CommandResult {
                session_id: Some(S1.into()),
                input_id: None,
            })
        })
        .await;
    assert!(
        result.is_err(),
        "the active-revert index is the durable fence"
    );
    assert!(rig.store.revert("rv-2".into()).await.unwrap().is_none());
}
