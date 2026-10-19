//! The stop plan and `session.stop_execution` over real process trees (BG-4,
//! design `docs/v2/design/board-gaps.md` §5). Every tree is started by the
//! test in its own session; nothing else is signalled.

#![cfg(target_os = "linux")]
#![allow(clippy::unwrap_used)]

use std::time::Duration;

use fleetdeck_proto::command::{CommandOrigin, ReceiptState, SessionCommand};
use fleetdeck_proto::execution_stop::{
    ExecutionStopPlan, ExecutionStopPlanQuery, StopOutcome, code,
};
use fleetdeck_proto::{RejectCode, SessionId};

use crate::manager::shim_resume_rig::*;
use crate::manager::stop_state::PLAN_TTL;
use crate::manager::stop_tree::{Tree, fifo, kill_process_of};
use crate::store::ShimState;
use crate::store::execution_stops::{self, StopRow};

fn query(session: &SessionId, shim: &str) -> ExecutionStopPlanQuery {
    ExecutionStopPlanQuery {
        session_id: session.clone(),
        shim_id: shim.to_owned(),
    }
}

fn stop(session: &SessionId, shim: &str, plan: &str) -> SessionCommand {
    SessionCommand::StopExecution {
        session_id: session.clone(),
        shim_id: shim.to_owned(),
        plan_id: plan.to_owned(),
    }
}

/// A session whose run `shim` was lost with the leader of `tree`.
async fn lost_run(rig: &mut Rig, tree: &Tree, shim: &str) -> SessionId {
    let session = rig.ended("claude").await;
    rig.row(
        &session,
        shim,
        ShimState::Lost,
        "execution_unknown",
        0,
        Some(tree.identity()),
    )
    .await;
    session
}

async fn plan(rig: &Rig, session: &SessionId, shim: &str) -> ExecutionStopPlan {
    rig.manager
        .execution_stop_plan(&query(session, shim))
        .await
        .unwrap()
}

/// The stop row of `command`, once it is settled.
async fn settled(rig: &Rig, command: &str) -> StopRow {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(40);
    loop {
        let row = rig
            .store()
            .execution_stop(command.to_owned())
            .await
            .unwrap();
        if let Some(row) = row.filter(|r| r.outcome().is_some()) {
            return row;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the stop did not settle"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn board_stop(rig: &mut Rig, command: SessionCommand) -> String {
    let d = rig.submit_as(CommandOrigin::Board, command).await;
    assert_eq!(d.receipt.state, ReceiptState::Accepted, "{:?}", d.receipt);
    d.receipt.command_id.as_str().to_owned()
}

#[tokio::test]
async fn the_plan_lists_the_tree_and_a_board_stop_signals_exactly_it() {
    let mut rig = Rig::direct(None).await;
    let mut tree = Tree::simple();
    let session = lost_run(&mut rig, &tree, "run").await;
    let plan = plan(&rig, &session, "run").await;
    assert!(plan.eligible && plan.code.is_none(), "{plan:?}");
    assert_eq!(plan.targets.len(), 3);
    assert!(
        plan.targets
            .iter()
            .any(|t| t.pid == tree.pid() && t.command == "sh")
    );
    assert!(!plan.plan_id.is_empty() && plan.expires_at > 0);
    assert_eq!(plan.check.shim_id.as_deref(), Some("run"));
    // Rule (a): the members are recorded as witnesses.
    let witnesses = rig
        .store()
        .execution_witnesses("run".to_owned())
        .await
        .unwrap();
    assert_eq!(witnesses.len(), 3);

    let command = board_stop(&mut rig, stop(&session, "run", &plan.plan_id)).await;
    let row = settled(&rig, &command).await;
    assert_eq!(row.state, execution_stops::DONE, "{row:?}");
    assert_eq!(row.code, None);
    assert_eq!(
        (row.term_sent, row.kill_sent, row.gone, row.skipped),
        (3, 0, 3, 0)
    );
    assert!(tree.members().is_empty());
    // The leader is a zombie until the test reaps it, and a zombie of the
    // group makes the census of the check incomplete.
    tree.end();
    // The check shows the outcome only for a deck with the feature.
    let old = rig.manager.execution_check(&session).await.unwrap();
    assert!(old.last_stop.is_none());
    let new = rig
        .manager
        .execution_check_with_stop(&session)
        .await
        .unwrap();
    let last = new.last_stop.clone().unwrap();
    assert_eq!(last.outcome, StopOutcome::Done);
    assert_eq!(last.command_id, command);
    assert!(!serde_json::to_string(&old).unwrap().contains("last_stop"));
    assert!(serde_json::to_string(&new).unwrap().contains("last_stop"));
    rig.manager
        .shutdown(fleetdeck_proto::command::StopMode::Kill)
        .await;
}

#[tokio::test]
async fn only_the_board_may_stop_and_a_refusal_stores_nothing() {
    let mut rig = Rig::direct(None).await;
    let mut tree = Tree::simple();
    let session = lost_run(&mut rig, &tree, "run").await;
    let plan = plan(&rig, &session, "run").await;
    for origin in [
        CommandOrigin::Local,
        CommandOrigin::Coordinator,
        CommandOrigin::Mail,
    ] {
        let d = rig
            .submit_as(origin, stop(&session, "run", &plan.plan_id))
            .await;
        assert_eq!(d.receipt.state, ReceiptState::Rejected, "{origin:?}");
        assert_eq!(d.receipt.reject.unwrap().code, RejectCode::Forbidden);
    }
    // The plan is still there: the board may use it.
    assert_eq!(tree.members().len(), 3);
    let command = board_stop(&mut rig, stop(&session, "run", &plan.plan_id)).await;
    settled(&rig, &command).await;
    tree.end();
}

#[tokio::test]
async fn a_plan_of_another_session_or_shim_or_an_old_plan_is_plan_changed() {
    let mut rig = Rig::direct(None).await;
    let mut tree = Tree::simple();
    let mut other = Tree::simple();
    let a = lost_run(&mut rig, &tree, "run-a").await;
    let b = lost_run(&mut rig, &other, "run-b").await;
    let plan_a = plan(&rig, &a, "run-a").await;
    let refused = |d: crate::store::Decided, want: &str| {
        assert_eq!(d.receipt.state, ReceiptState::Rejected, "{:?}", d.receipt);
        let reject = d.receipt.reject.unwrap();
        assert_eq!(reject.code, RejectCode::InvalidState);
        assert!(reject.message.starts_with(want), "{}", reject.message);
    };
    // The plan of A, used for B.
    let d = rig
        .submit_as(CommandOrigin::Board, stop(&b, "run-b", &plan_a.plan_id))
        .await;
    refused(d, "plan_changed: The process list changed.");
    // A plan id that does not exist.
    let d = rig
        .submit_as(CommandOrigin::Board, stop(&a, "run-a", "nope"))
        .await;
    refused(d, "plan_changed: ");
    // The shim of B with the session of A is an unknown shim.
    let d = rig
        .submit_as(CommandOrigin::Board, stop(&a, "run-b", &plan_a.plan_id))
        .await;
    assert_eq!(d.receipt.reject.unwrap().code, RejectCode::InvalidCommand);
    // A new plan of the session replaces the old one.
    let newer = plan(&rig, &a, "run-a").await;
    assert_ne!(newer.plan_id, plan_a.plan_id);
    let d = rig
        .submit_as(CommandOrigin::Board, stop(&a, "run-a", &plan_a.plan_id))
        .await;
    refused(d, "plan_changed: ");
    // An expired plan is plan_changed too.
    tokio::time::sleep(Duration::from_millis(10)).await;
    assert!(
        rig.manager
            .target
            .stops()
            .plans
            .take(&a, &newer.plan_id, std::time::Instant::now() + PLAN_TTL)
            .is_none()
    );
    let d = rig
        .submit_as(CommandOrigin::Board, stop(&a, "run-a", &newer.plan_id))
        .await;
    refused(d, "plan_changed: ");
    assert_eq!(tree.members().len(), 3);
    assert_eq!(other.members().len(), 3);
    tree.end();
    other.end();
}

#[tokio::test]
async fn a_duplicate_command_id_never_signals_again() {
    let mut rig = Rig::direct(None).await;
    let mut tree = Tree::simple();
    let session = lost_run(&mut rig, &tree, "run").await;
    let plan = plan(&rig, &session, "run").await;
    let envelope = rig.envelope(CommandOrigin::Board, stop(&session, "run", &plan.plan_id));
    let first = rig
        .manager
        .submit(envelope.clone(), crate::store::Caller::local())
        .await
        .unwrap();
    assert!(!first.duplicate && first.effects.len() == 1);
    let command = envelope.command_id.as_str().to_owned();
    let row = settled(&rig, &command).await;
    assert_eq!(row.term_sent, 3);
    // A new tree in the same shim row would show a second signal: the
    // duplicate must not reach it.
    let again = rig
        .manager
        .submit(envelope, crate::store::Caller::local())
        .await
        .unwrap();
    assert!(again.duplicate && again.effects.is_empty());
    assert_eq!(again.receipt.state, ReceiptState::Accepted);
    rig.settled(&session).await;
    let row = rig.store().execution_stop(command).await.unwrap().unwrap();
    assert_eq!((row.term_sent, row.kill_sent), (3, 0));
    tree.end();
}

#[tokio::test]
async fn a_child_that_ignores_sigterm_is_killed_through_the_same_pidfds() {
    let mut rig = Rig::direct(None).await;
    let mut tree = Tree::spawn("(trap '' TERM; exec sleep 600) & sleep 600 & wait");
    tree.wait_members(3);
    let session = lost_run(&mut rig, &tree, "run").await;
    let plan = plan(&rig, &session, "run").await;
    let command = board_stop(&mut rig, stop(&session, "run", &plan.plan_id)).await;
    let row = settled(&rig, &command).await;
    assert_eq!(row.state, execution_stops::DONE, "{row:?}");
    assert_eq!(
        (row.term_sent, row.kill_sent, row.gone, row.skipped),
        (3, 1, 3, 0),
        "{row:?}"
    );
    assert_eq!(row.code, None);
    tree.end();
}

#[tokio::test]
async fn a_member_that_appears_after_the_plan_gives_processes_remain_and_no_signal() {
    let mut rig = Rig::direct(None).await;
    let dir = tempfile::tempdir().unwrap();
    let go = fifo(dir.path());
    let script = format!(
        "sleep 600 & read x < {go}; sleep 600 & wait",
        go = go.display()
    );
    let mut tree = Tree::spawn(&script);
    tree.wait_members(2);
    let session = lost_run(&mut rig, &tree, "run").await;
    let plan = plan(&rig, &session, "run").await;
    assert_eq!(plan.targets.len(), 2);
    Tree::release(&go);
    tree.wait_members(3);
    let command = board_stop(&mut rig, stop(&session, "run", &plan.plan_id)).await;
    let row = settled(&rig, &command).await;
    assert_eq!(row.state, execution_stops::FAILED, "{row:?}");
    assert_eq!(row.code.as_deref(), Some(code::PROCESSES_REMAIN));
    assert_eq!((row.term_sent, row.kill_sent), (0, 0));
    assert_eq!(tree.members().len(), 3, "nothing was signalled");
    tree.end();
}

#[tokio::test]
async fn a_pid_reused_between_the_plan_and_the_dispatch_is_not_signalled() {
    let mut rig = Rig::direct(None).await;
    let mut tree = Tree::simple();
    let session = lost_run(&mut rig, &tree, "run").await;
    let plan = plan(&rig, &session, "run").await;
    let victim = plan
        .targets
        .iter()
        .find(|t| t.command == "sleep")
        .unwrap()
        .pid;
    let real = tree
        .members()
        .iter()
        .find(|m| m.pid == victim)
        .unwrap()
        .start_time;
    // The stored target now describes another process with this PID.
    rig.manager
        .target
        .stops()
        .plans
        .tamper_start_time(&session, victim, real + 1);
    let command = board_stop(&mut rig, stop(&session, "run", &plan.plan_id)).await;
    let row = settled(&rig, &command).await;
    // The recorded process is taken as ended (another has its PID): it gets
    // no signal and counts as gone, and nothing is skipped.
    assert_eq!((row.term_sent, row.skipped, row.gone), (2, 0, 3), "{row:?}");
    assert!(Tree::alive(victim), "the process of the reused PID lives");
    assert_eq!(row.code.as_deref(), Some(code::PROCESSES_REMAIN));
    kill_process_of(victim, real);
    tree.end();
}

#[tokio::test]
async fn with_the_leader_gone_only_the_recorded_witnesses_are_targets() {
    let mut rig = Rig::direct(None).await;
    let mut tree = Tree::simple();
    let session = lost_run(&mut rig, &tree, "run").await;
    // The check records the witnesses while the leader lives.
    let check = rig.manager.execution_check(&session).await.unwrap();
    assert!(check.identity_alive, "{check:?}");
    assert_eq!(
        rig.store()
            .execution_witnesses("run".to_owned())
            .await
            .unwrap()
            .len(),
        3
    );
    tree.kill_leader();
    let plan = plan(&rig, &session, "run").await;
    assert!(plan.eligible, "{plan:?}");
    assert_eq!(plan.targets.len(), 2, "{plan:?}");
    assert!(plan.targets.iter().all(|t| t.command == "sleep"));
    let command = board_stop(&mut rig, stop(&session, "run", &plan.plan_id)).await;
    let row = settled(&rig, &command).await;
    assert_eq!((row.term_sent, row.gone), (2, 2), "{row:?}");
    assert_eq!(row.code, None);
    tree.end();
}

#[tokio::test]
async fn without_a_witness_a_gone_leader_and_a_legacy_row_are_identity_unproven() {
    let mut rig = Rig::direct(None).await;
    let mut tree = Tree::simple();
    let session = lost_run(&mut rig, &tree, "run").await;
    // No check ran before the leader died: no witness exists.
    tree.kill_leader();
    let got = plan(&rig, &session, "run").await;
    assert!(!got.eligible && got.targets.is_empty(), "{got:?}");
    assert_eq!(got.code.as_deref(), Some(code::IDENTITY_UNPROVEN));
    assert!(got.plan_id.is_empty() && got.expires_at == 0);
    // A legacy row: no recorded identity.
    let legacy = rig.ended("claude").await;
    rig.row_for(
        &legacy,
        "legacy",
        ShimState::Lost,
        "execution_unknown",
        0,
        None,
        Some(&real_boot()),
        None,
    )
    .await;
    let got = plan(&rig, &legacy, "legacy").await;
    assert_eq!(
        got.code.as_deref(),
        Some(code::IDENTITY_UNPROVEN),
        "{got:?}"
    );
    let d = rig
        .submit_as(CommandOrigin::Board, stop(&legacy, "legacy", "x"))
        .await;
    let reject = d.receipt.reject.unwrap();
    assert_eq!(reject.code, RejectCode::InvalidState);
    assert!(
        reject.message.starts_with("identity_unproven: "),
        "{}",
        reject.message
    );
    tree.end();
}

#[tokio::test]
async fn an_empty_domain_is_an_answer_without_a_code_and_unknown_inputs_are_errors() {
    use crate::manager::StopPlanError;
    let mut rig = Rig::direct(None).await;
    let mut tree = Tree::simple();
    let session = lost_run(&mut rig, &tree, "run").await;
    tree.end();
    let got = plan(&rig, &session, "run").await;
    assert!(
        !got.eligible && got.code.is_none() && got.targets.is_empty(),
        "{got:?}"
    );
    let unknown = SessionId::from("nope".to_owned());
    assert!(matches!(
        rig.manager
            .execution_stop_plan(&query(&unknown, "run"))
            .await,
        Err(StopPlanError::UnknownSession)
    ));
    assert!(matches!(
        rig.manager
            .execution_stop_plan(&query(&session, "other"))
            .await,
        Err(StopPlanError::UnknownShim)
    ));
}

#[tokio::test]
async fn a_restart_settles_the_open_rows_and_signals_nothing() {
    let mut rig = Rig::direct(None).await;
    let mut tree = Tree::simple();
    let session = lost_run(&mut rig, &tree, "run").await;
    let store = rig.store();
    store
        .execution_stop_seed(
            ("auth", session.as_str(), "run"),
            execution_stops::AUTHORIZED,
            1,
        )
        .await;
    store
        .execution_stop_seed(
            ("tried", session.as_str(), "run"),
            execution_stops::ATTEMPTED,
            2,
        )
        .await;
    rig.as_shim_station().await;
    let row = |id: &'static str| {
        let store = rig.store();
        async move { store.execution_stop(id.to_owned()).await.unwrap().unwrap() }
    };
    let tried = row("tried").await;
    assert_eq!(tried.state, execution_stops::UNKNOWN);
    assert_eq!(tried.code.as_deref(), Some(code::OUTCOME_UNKNOWN));
    let auth = row("auth").await;
    assert_eq!(auth.state, execution_stops::FAILED);
    assert_eq!(auth.code.as_deref(), Some(code::PLAN_CHANGED));
    assert_eq!(tree.members().len(), 3, "recovery signals nothing");
    tree.end();
}

#[tokio::test]
async fn the_check_answers_the_newest_settled_stop_only_for_the_feature() {
    let mut rig = Rig::direct(None).await;
    let mut tree = Tree::simple();
    let session = lost_run(&mut rig, &tree, "run").await;
    let store = rig.store();
    store
        .execution_stop_seed(
            ("open", session.as_str(), "run"),
            execution_stops::ATTEMPTED,
            5,
        )
        .await;
    let check = rig
        .manager
        .execution_check_with_stop(&session)
        .await
        .unwrap();
    assert!(check.last_stop.is_none(), "an open row is skipped");
    tree.end();
}
