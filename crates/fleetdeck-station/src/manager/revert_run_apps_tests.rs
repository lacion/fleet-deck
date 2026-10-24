//! The revert against the app call barrier (B15 T5b, design §4.6.1): a revert
//! meets `app_call_running` and `app_call_ran_after_turn` at the dispatch, in
//! a restore retry and a second time under the fence; the provider-timeout
//! and the detach cases keep the barrier while the server writes; the owner's
//! release and the list work after the caps and a restart. The rig of
//! `apps/calls_tests.rs` gives the session, the folder and the fake bridge;
//! the revert part copies the one of `revert_tests.rs`.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use fleetdeck_proto::apps::{AppCallState, AppUncertainCause};
use fleetdeck_proto::command::{CommandEnvelope, CommandOrigin, CommandResult, SessionCommand};
use fleetdeck_proto::event::InputState;
use fleetdeck_proto::{CommandId, SessionId};
use fleetdeck_providers::ProviderError;
use fleetdeck_providers::apps::AppError;
use tokio::sync::Notify;

use crate::apps::calls::rig::{Rig, S1, Script, proc, target_of, text, until};
use crate::apps::calls::{self, code};
use crate::manager::revert_run::{self, RevertEnv, Rollback, reason};
use crate::manager::shell_advance;
use crate::manager::target::HostEnd;
use crate::manager::turns::Fences;
use crate::manager::turns::turns_tests::{completed, dispatch, input, rows, started};
use crate::store::app_calls::{AppCallBarrier, CallState, NewAppCall};
use crate::store::apps::AppLookup;
use crate::store::{Caller, Dispatch, NewRevert, RevertStep, RowSide};
use crate::workspace::{GitOptions, Limits, PrivatePaths};

/// A provider that confirms every rollback.
struct Confirms;

#[async_trait]
impl Rollback for Confirms {
    async fn roll_back(&mut self, _turns: u32) -> Result<(), ProviderError> {
        Ok(())
    }
}

/// The app rig with two finalized turns: T1 writes `a.txt`, T2 writes `b.txt`.
struct RevRig {
    rig: Rig,
    git: GitOptions,
    private: PrivatePaths,
}

impl RevRig {
    async fn new() -> Self {
        let rev = Self {
            rig: Rig::new().await,
            git: GitOptions::isolated(),
            private: PrivatePaths::default(),
        };
        rev.turn("in-1", "turn-1", "a.txt", "one\nT1\n").await;
        rev.turn("in-2", "turn-2", "b.txt", "two\nT2\n").await;
        rev
    }

    async fn turn(&self, input_id: &str, turn: &str, file: &str, content: &str) {
        let turns = &self.rig.turns;
        dispatch(turns, input_id).await;
        turns.observe(&[started(turn)]).await;
        std::fs::write(self.rig.dir.join(file), content).unwrap();
        let due = turns
            .observe(&[
                input(input_id, Some(turn), InputState::Started),
                completed(turn),
            ])
            .await;
        assert!(due);
        Arc::clone(turns).capture_due().await;
    }

    fn env(&self) -> RevertEnv<'_> {
        RevertEnv {
            store: &self.rig.store,
            git: &self.git,
            limits: Limits::default(),
            private: &self.private,
        }
    }

    /// Admits revert `id` of the turn at `target` up to the turn at `current`.
    async fn admit_with(&self, id: &str, turn: &str, target: usize, current: usize) {
        let rows = rows(&self.rig.store).await;
        let commit = |side: &RowSide| match side {
            RowSide::Available(c) => c.clone(),
            other => panic!("{other:?}"),
        };
        let new = NewRevert {
            revert_id: id.into(),
            session_id: S1.into(),
            process: "proc-1".to_owned().into(),
            turn_id: turn.into(),
            app_turn: rows[target].app_turn.clone(),
            current_app_turn: rows[current].app_turn.clone(),
            turns: u32::try_from(current - target + 1).unwrap(),
            target_commit: commit(&rows[target].start),
            current_commit: commit(&rows[current].end),
            worktree_root: self.rig.turns.worktree_root().unwrap(),
        };
        let envelope = CommandEnvelope {
            authority: None,
            command_id: CommandId::from(format!("c-{id}").as_str()),
            target_incarnation: None,
            retry_until: i64::try_from(self.rig.det.now_ms()).unwrap() + 60_000,
            origin: CommandOrigin::Board,
            command: SessionCommand::Revert {
                session_id: S1.into(),
                turn_id: turn.into(),
            },
        };
        self.rig
            .store
            .decide(envelope, Caller::local(), move |d| {
                d.admit_revert(&new)?;
                Ok(CommandResult {
                    session_id: Some(S1.into()),
                    input_id: None,
                    seq: None,
                })
            })
            .await
            .unwrap();
    }

    /// Admits a revert of both turns and runs it.
    async fn revert(&self, id: &str) -> Dispatch {
        self.admit_with(id, "turn-1", 0, 1).await;
        self.run(id).await
    }

    async fn run(&self, id: &str) -> Dispatch {
        let mut session = Confirms;
        revert_run::run(
            self.env(),
            &self.rig.turns,
            &mut session,
            &proc("proc-1"),
            id,
        )
        .await
    }

    async fn step(&self, id: &str) -> (RevertStep, Option<String>) {
        let row = self.rig.store.revert(id.into()).await.unwrap().unwrap();
        (row.step, row.reason)
    }

    fn file(&self, name: &str) -> String {
        std::fs::read_to_string(self.rig.dir.join(name)).unwrap()
    }

    /// The `app_turn` of the first turn row.
    async fn first_turn(&self) -> String {
        rows(&self.rig.store).await[0].app_turn.as_str().to_owned()
    }

    fn files_unchanged(&self) {
        assert_eq!(self.file("a.txt"), "one\nT1\n");
        assert_eq!(self.file("b.txt"), "two\nT2\n");
    }

    async fn advance(&self) {
        shell_advance::advance_pending(&self.rig.store, &Fences::default())
            .await
            .unwrap();
    }

    async fn refused(&self, id: &str, why: &str) {
        assert_eq!(
            self.revert(id).await,
            Dispatch::NotDispatched(why.to_owned())
        );
        assert_eq!(
            self.step(id).await,
            (RevertStep::Failed, Some(why.to_owned()))
        );
        self.files_unchanged();
    }

    /// Runs one call of `tool` to its end through the gate.
    async fn finished_call(&self, tool: &str, confirmed: bool) -> String {
        let app = self.rig.app().await;
        let gate = Arc::new(Notify::new());
        self.rig.bridge.set(Script::Gated(
            Arc::clone(&gate),
            Box::new(Script::Ok(text("x"))),
        ));
        let handle = self.rig.spawn_call(&app, tool, confirmed);
        let id = self.rig.open_id().await;
        gate.notify_waiters();
        handle.await.unwrap().unwrap();
        self.rig.wait_settled(&id).await;
        id
    }
}

#[tokio::test]
async fn revert_refused_while_an_app_call_runs() {
    let rev = RevRig::new().await;
    let app = rev.rig.app().await;
    let gate = Arc::new(Notify::new());
    rev.rig.bridge.set(Script::Gated(
        Arc::clone(&gate),
        Box::new(Script::Ok(text("x"))),
    ));
    let handle = rev.rig.spawn_call(&app, "bump", true);
    let id = rev.rig.open_id().await;
    rev.refused("rv-1", reason::APP_CALL_RUNNING).await;
    gate.notify_waiters();
    handle.await.unwrap().unwrap();
    rev.rig.wait_settled(&id).await;
}

#[tokio::test]
async fn revert_refused_across_a_finished_non_read_only_call() {
    let rev = RevRig::new().await;
    rev.finished_call("bump", true).await;
    rev.refused("rv-1", reason::APP_CALL_RAN_AFTER_TURN).await;
}

#[tokio::test]
async fn revert_allowed_after_a_read_only_call_ended() {
    let rev = RevRig::new().await;
    rev.finished_call("show_counter", false).await;
    assert_eq!(rev.revert("rv-1").await, Dispatch::Confirmed);
    assert_eq!(rev.step("rv-1").await, (RevertStep::Succeeded, None));
    assert_eq!(rev.file("a.txt"), "one\n");
}

#[tokio::test]
async fn revert_rechecks_the_barrier_under_the_fence() {
    let rev = RevRig::new().await;
    rev.admit_with("rv-1", "turn-1", 0, 1).await;
    // The fence is held; the revert passes its first check (no row yet) and
    // waits for the fence; an app call inserts its row under the fence.
    let fence = rev.rig.turns.fence().await;
    let revert = rev.run("rv-1");
    let call = async {
        tokio::time::sleep(Duration::from_millis(200)).await;
        rev.rig.plant("racer", "proc-1", None).await;
        drop(fence);
    };
    let (dispatch, ()) = tokio::join!(revert, call);
    assert_eq!(
        dispatch,
        Dispatch::NotDispatched(reason::APP_CALL_RUNNING.to_owned())
    );
    assert_eq!(
        rev.step("rv-1").await,
        (
            RevertStep::Failed,
            Some(reason::APP_CALL_RUNNING.to_owned())
        )
    );
    rev.files_unchanged();
}

#[tokio::test]
async fn retry_restore_meets_the_app_barrier() {
    let rev = RevRig::new().await;
    rev.admit_with("rv-1", "turn-1", 0, 1).await;
    let store = &rev.rig.store;
    for (from, to) in [
        (&[RevertStep::Admitted], RevertStep::RollbackSent),
        (&[RevertStep::RollbackSent], RevertStep::RolledBack),
    ] {
        assert!(
            store
                .advance_revert("rv-1".into(), from, to, None)
                .await
                .unwrap()
        );
    }
    // The owner's retry of the restore, while an app call of the folder runs:
    // the revert goes back to `reconciliation_required`, nothing is written.
    rev.rig.plant("running", "proc-1", None).await;
    assert_eq!(
        rev.run("rv-1").await,
        Dispatch::NotDispatched(reason::APP_CALL_RUNNING.to_owned())
    );
    assert_eq!(
        rev.step("rv-1").await,
        (
            RevertStep::ReconciliationRequired,
            Some(reason::APP_CALL_RUNNING.to_owned())
        )
    );
    rev.files_unchanged();
}

/// A provider answer error of a non-read-only `slow_bump` call of `proc-1`.
fn timed_out() -> Script {
    Script::Err(AppError::Answered(
        "MCP server demo tool slow_bump timed out".to_owned(),
    ))
}

/// The fake server: writes `slow_bump.txt` into the folder after the provider
/// answered its timeout, and goes on writing.
fn server_writes(dir: &std::path::Path, times: u32) {
    let path = dir.join("slow_bump.txt");
    std::thread::spawn(move || {
        for n in 0..times {
            std::thread::sleep(Duration::from_millis(100));
            let _ = std::fs::write(&path, format!("write {n}\n"));
        }
    });
}

async fn file_exists(rev: &RevRig) {
    let path = rev.rig.dir.join("slow_bump.txt");
    until("the server write", || async { path.exists().then_some(()) }).await;
}

#[tokio::test]
async fn app_call_provider_timeout_keeps_the_barrier_while_the_server_writes() {
    let rev = RevRig::new().await;
    let app = rev.rig.app().await;
    rev.rig.bridge.set(timed_out());
    server_writes(&rev.rig.dir, 1);
    let err = rev.rig.call(&app, "slow_bump", true).await.unwrap_err();
    assert_eq!(err.code, code::PROVIDER_ERROR);
    assert!(
        err.message.contains("The tool may still run."),
        "{}",
        err.message
    );
    let id = rev.rig.open_id().await;
    let row = rev.rig.wait_state(&id, CallState::Uncertain).await;
    assert_eq!(row.cause, Some(AppUncertainCause::ProviderError));
    file_exists(&rev).await;
    // A revert of the turn before the call, dispatched after the file exists,
    // ends `failed` with `app_call_running`; the file is still there.
    rev.admit_with("rv-1", "turn-1", 0, 1).await;
    assert_eq!(
        rev.run("rv-1").await,
        Dispatch::NotDispatched(reason::APP_CALL_RUNNING.to_owned())
    );
    assert_eq!(
        rev.step("rv-1").await,
        (
            RevertStep::Failed,
            Some(reason::APP_CALL_RUNNING.to_owned())
        )
    );
    assert!(rev.rig.dir.join("slow_bump.txt").exists());
    // The end of the process settles the row; the advance raises the
    // generation; the same revert is then refused across the call.
    let session: SessionId = S1.into();
    assert_eq!(
        calls::process_ended(&rev.rig.store, &session, "proc-1")
            .await
            .unwrap(),
        1
    );
    rev.rig.wait_state(&id, CallState::Ended).await;
    rev.advance().await;
    rev.refused("rv-2", reason::APP_CALL_RAN_AFTER_TURN).await;
}

/// A non-read-only `slow_bump` call whose provider-timeout settlement is
/// paused, a detach of the host, then the resume: the row is `uncertain`.
async fn detached_uncertain_call(rev: &RevRig) -> String {
    let rig = &rev.rig;
    let target = target_of(rig);
    let session: SessionId = S1.into();
    target.register_app_host(&session, rig.host.clone());
    let (reached, resume) = (Arc::new(Notify::new()), Arc::new(Notify::new()));
    let mut env = target.app_env();
    env.hooks.before_record = Some((Arc::clone(&reached), Arc::clone(&resume)));
    let app = rig.app().await;
    rig.bridge.set(timed_out());
    let params = rig.tools_call(&app, "slow_bump", true);
    let host = target.app_host(&session).unwrap();
    let handle = tokio::spawn(async move { calls::app_call(&env, Some(host), params).await });
    reached.notified().await;
    let id = rig.open_id().await;
    assert!(target.drop_app_host(&session, &proc("proc-1"), HostEnd::Detached));
    resume.notify_one();
    assert_eq!(
        handle.await.unwrap().unwrap_err().code,
        code::PROVIDER_ERROR
    );
    let row = rig.wait_state(&id, CallState::Uncertain).await;
    assert_eq!(row.cause, Some(AppUncertainCause::ProviderError));
    let turn = rev.first_turn().await;
    let root = rig.turns.worktree_root().unwrap();
    assert_eq!(
        rig.store.app_call_barrier(root, turn).await.unwrap(),
        Some(AppCallBarrier::Running)
    );
    id
}

/// The restart of the station on the same store: a new `ProcessExits`, the
/// start-up of the advance task, and the shim proof check (none here).
async fn restart(rev: &RevRig) {
    shell_advance::start_up(&rev.rig.store, &Fences::default())
        .await
        .unwrap();
    calls::settle_proven(&rev.rig.store).await.unwrap();
}

#[tokio::test]
async fn a_late_answer_after_detach_keeps_the_call_uncertain() {
    let rev = RevRig::new().await;
    let id = detached_uncertain_call(&rev).await;
    restart(&rev).await;
    let row = rev.rig.row(&id).await;
    assert_eq!(row.state, CallState::Uncertain);
    assert_eq!(row.cause, Some(AppUncertainCause::ProviderError));
    let root = rev.rig.turns.worktree_root().unwrap();
    let turn = rev.first_turn().await;
    assert_eq!(
        rev.rig.store.app_call_barrier(root, turn).await.unwrap(),
        Some(AppCallBarrier::Running)
    );
    // The fake server writes into the folder; the revert is refused and the
    // file stays.
    server_writes(&rev.rig.dir, 20);
    file_exists(&rev).await;
    rev.refused("rv-1", reason::APP_CALL_RUNNING).await;
    assert!(rev.rig.dir.join("slow_bump.txt").exists());
    // The exit of another process changes nothing.
    let session: SessionId = S1.into();
    assert_eq!(
        calls::process_ended(&rev.rig.store, &session, "proc-2")
            .await
            .unwrap(),
        0
    );
    assert_eq!(rev.rig.row(&id).await.state, CallState::Uncertain);
    // The exit of that process in the restarted station settles the row; the
    // advance raises the generation; the same revert is refused across it.
    let restarted = target_of(&rev.rig);
    restarted.drop_app_host(&session, &proc("proc-1"), HostEnd::Exited);
    rev.rig.wait_state(&id, CallState::Ended).await;
    rev.advance().await;
    rev.refused("rv-2", reason::APP_CALL_RAN_AFTER_TURN).await;
}

#[tokio::test]
async fn a_late_answer_after_detach_ends_with_the_release() {
    let rev = RevRig::new().await;
    let id = detached_uncertain_call(&rev).await;
    restart(&rev).await;
    let session: SessionId = S1.into();
    let reply = calls::app_release(&rev.rig.env, session, id.clone())
        .await
        .unwrap();
    assert!(reply.released);
    rev.rig.wait_state(&id, CallState::Released).await;
    rev.advance().await;
    rev.refused("rv-1", reason::APP_CALL_RAN_AFTER_TURN).await;
}

#[tokio::test]
async fn app_calls_list_and_release_after_eviction_and_restart() {
    let rev = RevRig::new().await;
    let rig = &rev.rig;
    let session: SessionId = S1.into();
    let app = rig.app().await;
    let root = rig.turns.worktree_root().unwrap();
    // A non-read-only call `running` under an older incarnation.
    rig.store
        .app_call_begin(NewAppCall {
            call_id: "c1".to_owned(),
            session_id: S1.into(),
            app_id: app.clone(),
            worktree_root: root.clone(),
            server: "demo".to_owned(),
            tool: "bump".to_owned(),
            read_only: false,
            process: "proc-1".to_owned(),
            station_run: "older".to_owned(),
        })
        .await
        .unwrap();
    shell_advance::start_up(&rig.store, &Fences::default())
        .await
        .unwrap();
    let row = rig.row("c1").await;
    assert_eq!(row.state, CallState::Uncertain);
    assert_eq!(row.cause, Some(AppUncertainCause::StationRestart));
    // 50 newer apps make the caps delete the row of the first app.
    for _ in 0..crate::store::apps::MAX_APPS_PER_SESSION {
        rig.app().await;
    }
    let lookup = rig.store.app_lookup(S1.into(), app.clone()).await.unwrap();
    assert_eq!(lookup, AppLookup::Gone);
    // The list of the session shows the call with its app id; a second
    // session in the same folder shows it too, and a revert in the folder is
    // refused.
    let listed = calls::app_calls(&rig.env, Some(rig.host.clone()), session.clone())
        .await
        .unwrap();
    assert_eq!(listed.calls.len(), 1);
    assert_eq!(listed.calls[0].call_id, "c1");
    assert_eq!(listed.calls[0].app_id, app);
    assert_eq!(listed.calls[0].state, AppCallState::Uncertain);
    assert_eq!(
        listed.calls[0].cause,
        Some(AppUncertainCause::StationRestart)
    );
    let other = calls::app_calls(&rig.env, Some(rig.host.clone()), "s2".into())
        .await
        .unwrap();
    assert_eq!(other.calls.len(), 1);
    assert_eq!(other.calls[0].session_id.as_str(), S1);
    rev.refused("rv-1", reason::APP_CALL_RUNNING).await;
    // The release reads only the call row.
    let released = calls::app_release(&rig.env, session.clone(), "c1".to_owned())
        .await
        .unwrap();
    assert!(released.released);
    rev.advance().await;
    // A turn after the release stores the new generation: no barrier.
    rev.turn("in-3", "turn-3", "b.txt", "three\n").await;
    let third = rows(&rig.store).await[2].app_turn.as_str().to_owned();
    assert_eq!(rig.store.app_call_barrier(root, third).await.unwrap(), None);
    let again = calls::app_release(&rig.env, session, "c1".to_owned())
        .await
        .unwrap();
    assert!(!again.released);
}
