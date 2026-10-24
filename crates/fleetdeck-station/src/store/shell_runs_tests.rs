//! The shell rows, their settlement and advance API, the retirement hook and
//! the V27 migration (B8).

use fleetdeck_proto::command::{
    CommandOrigin, CommandResult, RETRY_WINDOW_MS, ReceiptState, SessionCommand,
};
use fleetdeck_proto::event::ProviderKind;
use fleetdeck_proto::{CommandEnvelope, CommandId};
use fleetdeck_testkit::Det;
use rusqlite::Connection;

use super::*;
use crate::store::{
    Caller, EffectPayload, EffectState, NewSession, OfflineApprovalPolicy, StoreConfig, schema,
};

const S1: &str = "s1";
const ROOT: &str = "/w";

async fn store(det: &Det) -> Store {
    let s = Store::open_in_memory(det.clock(), det.ids(), StoreConfig::default())
        .await
        .unwrap();
    s.recover(Vec::new()).await.unwrap();
    s.create_session(NewSession {
        session_id: S1.into(),
        provider: ProviderKind::Claude,
        provider_instance: "claude".into(),
        cwd: ROOT.to_owned(),
        worktree: None,
        branch: None,
        mode: None,
        title: None,
        offline_approval_policy: OfflineApprovalPolicy::Wait,
    })
    .await
    .unwrap();
    s.process_started(S1.into(), "proc-1".into(), Some(1))
        .await
        .unwrap();
    s
}

/// Decides a command that queues a shell effect with its row; returns the
/// effect id.
async fn queue(s: &Store, det: &Det, id: &str, root: &'static str) -> String {
    let envelope = CommandEnvelope {
        authority: None,
        command_id: CommandId::from(id),
        target_incarnation: None,
        retry_until: i64::try_from(det.now_ms()).unwrap() + RETRY_WINDOW_MS,
        origin: CommandOrigin::Board,
        command: SessionCommand::Interrupt {
            session_id: S1.into(),
        },
    };
    let d = s
        .decide(envelope, Caller::local(), move |d| {
            let effect_id = d
                .enqueue(
                    &S1.into(),
                    EffectPayload::ShellCommand {
                        command: "ls".to_owned(),
                        provider: ProviderKind::Claude,
                        worktree_root: root.to_owned(),
                        timeout_ms: 600_000,
                    },
                )?
                .effect_id
                .as_str()
                .to_owned();
            d.shell_insert(&effect_id, &S1.into(), ProviderKind::Claude, root, "ls")?;
            Ok(CommandResult {
                session_id: Some(S1.into()),
                input_id: None,
                seq: None,
            })
        })
        .await
        .unwrap();
    assert_eq!(d.receipt.state, ReceiptState::Accepted);
    d.effects[0].effect_id.as_str().to_owned()
}

async fn state(s: &Store, id: &str) -> ShellState {
    s.shell_run(id.to_owned()).await.unwrap().unwrap().state
}

async fn settle(s: &Store, id: &str, state: ShellState) -> bool {
    s.shell_settle(id.to_owned(), state, Some(0), Some("out".into()), None)
        .await
        .unwrap()
}

#[tokio::test]
async fn the_states_know_which_are_terminal() {
    use ShellState::*;
    for open in [Queued, Running, AwaitingProof] {
        assert!(!open.is_terminal(), "{open:?}");
    }
    for done in [Done, Failed, Cancelled, Timeout, Unknown] {
        assert!(done.is_terminal(), "{done:?}");
    }
    assert_eq!(AwaitingProof.as_str(), "awaiting_proof");
}

#[tokio::test]
async fn a_row_is_unsettled_until_its_advance_ran() {
    let det = Det::new();
    let s = store(&det).await;
    assert!(!s.shell_root_unsettled(ROOT.to_owned()).await.unwrap());
    let id = queue(&s, &det, "c1", ROOT).await;
    let row = s.shell_run(id.clone()).await.unwrap().unwrap();
    assert_eq!(row.state, ShellState::Queued);
    assert_eq!(row.provider, "claude");
    assert_eq!(row.worktree_root, ROOT);
    assert!(s.shell_root_unsettled(ROOT.to_owned()).await.unwrap());
    // Another root is not busy; the roots compare exactly.
    assert!(!s.shell_root_unsettled("/w/sub".to_owned()).await.unwrap());
    assert!(!s.shell_root_unsettled("/".to_owned()).await.unwrap());

    assert!(settle(&s, &id, ShellState::Done).await);
    let row = s.shell_run(id.clone()).await.unwrap().unwrap();
    assert!(row.advance_pending && row.ended_at.is_some());
    assert_eq!(row.exit_code, Some(0));
    assert_eq!(row.stdout_tail.as_deref(), Some("out"));
    assert!(s.shell_root_unsettled(ROOT.to_owned()).await.unwrap());
    assert_eq!(s.shell_runs_unsettled().await.unwrap().len(), 1);
    assert_eq!(
        s.shell_pending_advances().await.unwrap(),
        vec![(id.clone(), ROOT.to_owned())]
    );

    s.shell_advance(id.clone(), ROOT.to_owned(), false)
        .await
        .unwrap();
    assert!(!s.shell_root_unsettled(ROOT.to_owned()).await.unwrap());
    assert!(s.shell_runs_unsettled().await.unwrap().is_empty());
    assert!(s.shell_pending_advances().await.unwrap().is_empty());
}

#[tokio::test]
async fn a_terminal_row_is_never_settled_again() {
    let det = Det::new();
    let s = store(&det).await;
    let id = queue(&s, &det, "c1", ROOT).await;
    assert!(settle(&s, &id, ShellState::Done).await);
    assert!(!settle(&s, &id, ShellState::Cancelled).await);
    assert!(!settle(&s, &id, ShellState::Failed).await);
    assert_eq!(state(&s, &id).await, ShellState::Done);
    // A state that is not terminal is no settlement.
    let other = queue(&s, &det, "c2", ROOT).await;
    assert!(
        s.shell_settle(other.clone(), ShellState::Running, None, None, None)
            .await
            .is_err()
    );
    assert_eq!(state(&s, &other).await, ShellState::Queued);
}

#[tokio::test]
async fn a_row_with_a_stored_cancel_does_not_start_running() {
    let det = Det::new();
    let s = store(&det).await;
    let id = queue(&s, &det, "c1", ROOT).await;
    assert!(s.shell_request_cancel(id.clone()).await.unwrap());
    assert!(!s.shell_set_running(id.clone()).await.unwrap());
    assert_eq!(state(&s, &id).await, ShellState::Queued);

    let free = queue(&s, &det, "c2", ROOT).await;
    assert!(s.shell_set_running(free.clone()).await.unwrap());
    assert_eq!(state(&s, &free).await, ShellState::Running);
    // A second move does nothing.
    assert!(!s.shell_set_running(free.clone()).await.unwrap());
    // A terminal row takes no cancel.
    assert!(settle(&s, &free, ShellState::Cancelled).await);
    assert!(!s.shell_request_cancel(free).await.unwrap());
}

#[tokio::test]
async fn the_proof_wait_moves_only_from_the_state_that_was_read() {
    let det = Det::new();
    let s = store(&det).await;
    let id = queue(&s, &det, "c1", ROOT).await;
    assert!(
        !s.shell_set_awaiting_proof(id.clone(), ShellState::Running)
            .await
            .unwrap()
    );
    assert!(
        s.shell_set_awaiting_proof(id.clone(), ShellState::Queued)
            .await
            .unwrap()
    );
    assert_eq!(state(&s, &id).await, ShellState::AwaitingProof);
    // It still blocks the folder, and a proof settles it.
    assert!(s.shell_root_unsettled(ROOT.to_owned()).await.unwrap());
    assert!(settle(&s, &id, ShellState::Unknown).await);
    assert!(
        !s.shell_set_awaiting_proof(id, ShellState::Queued)
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn the_admission_is_closed_until_opened_and_the_sweep_cancels_every_open_row() {
    let det = Det::new();
    let s = store(&det).await;
    // A missing flag means closed.
    assert!(!s.shell_admission_open().await.unwrap());
    s.shell_open_admission().await.unwrap();
    assert!(s.shell_admission_open().await.unwrap());

    let a = queue(&s, &det, "c1", ROOT).await;
    let b = queue(&s, &det, "c2", ROOT).await;
    assert!(s.shell_set_running(b.clone()).await.unwrap());
    let c = queue(&s, &det, "c3", ROOT).await;
    assert!(settle(&s, &c, ShellState::Done).await);

    let mut ids = s.shell_close_admission_and_cancel_all().await.unwrap();
    ids.sort();
    let mut want = vec![a.clone(), b.clone()];
    want.sort();
    assert_eq!(ids, want);
    assert!(!s.shell_admission_open().await.unwrap());
    for id in [&a, &b] {
        let row = s.shell_run(id.clone()).await.unwrap().unwrap();
        assert!(row.cancel_requested, "{id}");
    }
    let done = s.shell_run(c).await.unwrap().unwrap();
    assert!(!done.cancel_requested);
    // The stored cancel keeps `a` from running.
    assert!(!s.shell_set_running(a).await.unwrap());
}

#[tokio::test]
async fn the_advance_counts_the_folder_generation_at_the_start_and_at_the_settlement() {
    let det = Det::new();
    let s = store(&det).await;
    assert_eq!(s.root_generation(ROOT.to_owned()).await.unwrap(), 0);
    let id = queue(&s, &det, "c1", ROOT).await;
    let start = s
        .shell_advance(id.clone(), ROOT.to_owned(), true)
        .await
        .unwrap();
    assert_eq!(start, 1);
    let row = s.shell_run(id.clone()).await.unwrap().unwrap();
    assert_eq!(row.generation, Some(1));
    assert!(!row.advance_pending);

    assert!(settle(&s, &id, ShellState::Done).await);
    let end = s
        .shell_advance(id.clone(), ROOT.to_owned(), false)
        .await
        .unwrap();
    assert_eq!(end, 2);
    assert!(
        !s.shell_run(id.clone())
            .await
            .unwrap()
            .unwrap()
            .advance_pending
    );
    // A repeated settlement advance changes nothing.
    let again = s
        .shell_advance(id.clone(), ROOT.to_owned(), false)
        .await
        .unwrap();
    assert_eq!(again, 2);
    assert_eq!(s.root_generation(ROOT.to_owned()).await.unwrap(), 2);
    // Another root has its own counter.
    assert_eq!(s.root_generation("/other".to_owned()).await.unwrap(), 0);
}

#[tokio::test]
async fn only_finished_claude_rows_without_an_input_are_attachable() {
    let det = Det::new();
    let s = store(&det).await;
    let open = queue(&s, &det, "c1", ROOT).await;
    let done = queue(&s, &det, "c2", ROOT).await;
    let cancelled = queue(&s, &det, "c3", ROOT).await;
    assert!(settle(&s, &done, ShellState::Done).await);
    assert!(settle(&s, &cancelled, ShellState::Cancelled).await);

    let ids = |rows: Vec<ShellRow>| rows.into_iter().map(|r| r.effect_id).collect::<Vec<_>>();
    let attachable = ids(s.shell_attachable(S1.into()).await.unwrap());
    assert_eq!(attachable, vec![done.clone(), cancelled.clone()]);
    assert!(!attachable.contains(&open));

    let n = s
        .shell_include(vec![done.clone()], "input-1".to_owned())
        .await
        .unwrap();
    assert_eq!(n, 1);
    assert_eq!(
        ids(s.shell_attachable(S1.into()).await.unwrap()),
        vec![cancelled]
    );
    // A row that an input carried keeps its first input.
    let n = s
        .shell_include(vec![done.clone()], "input-2".to_owned())
        .await
        .unwrap();
    assert_eq!(n, 0);
    let row = s.shell_run(done).await.unwrap().unwrap();
    assert_eq!(row.included_in.as_deref(), Some("input-1"));
}

#[tokio::test]
async fn retiring_a_shell_effect_settles_its_row_cancelled_with_a_pending_advance() {
    let det = Det::new();
    let s = store(&det).await;
    let id = queue(&s, &det, "c1", ROOT).await;
    // The process ends: the queued effect is retired by the generic path.
    s.process_ended("proc-1".into(), "process_lost".to_owned())
        .await
        .unwrap();
    let effect = s.effect(id.clone().into()).await.unwrap().unwrap();
    assert_eq!(effect.state, EffectState::RetiredNotDispatched);
    let row = s.shell_run(id.clone()).await.unwrap().unwrap();
    assert_eq!(row.state, ShellState::Cancelled);
    assert!(row.advance_pending);
    assert!(s.shell_root_unsettled(ROOT.to_owned()).await.unwrap());
}

#[tokio::test]
async fn a_retirement_never_overwrites_a_settled_row() {
    let det = Det::new();
    let s = store(&det).await;
    let id = queue(&s, &det, "c1", ROOT).await;
    assert!(settle(&s, &id, ShellState::Done).await);
    s.process_ended("proc-1".into(), "process_lost".to_owned())
        .await
        .unwrap();
    assert_eq!(state(&s, &id).await, ShellState::Done);
}

#[tokio::test]
async fn the_start_up_settles_the_rows_of_retired_effects_and_leaves_the_others() {
    let det = Det::new();
    let s = store(&det).await;
    let kept = queue(&s, &det, "c1", ROOT).await;
    let retired = queue(&s, &det, "c2", ROOT).await;
    // Retire one effect by its own row, as a recovery of a dead process
    // does, without the hook, to model a row that the hook missed.
    let target = retired.clone();
    s.write(move |conn, _, _| {
        conn.execute(
            "UPDATE effects SET state = 'retired_not_dispatched' WHERE effect_id = ?1",
            [&target],
        )?;
        Ok(())
    })
    .await
    .unwrap();
    assert_eq!(s.shell_cancel_unclaimed().await.unwrap(), 1);
    assert_eq!(state(&s, &retired).await, ShellState::Cancelled);
    assert_eq!(state(&s, &kept).await, ShellState::Queued);
    assert!(s.shell_run(retired).await.unwrap().unwrap().advance_pending);
}

#[test]
fn v27_adds_the_shell_tables_and_the_checkpoint_generation_to_a_v26_database() {
    let mut conn = Connection::open_in_memory().unwrap();
    let migrations = schema::migrations();
    migrations.to_version(&mut conn, 26).unwrap();
    let tables = |conn: &Connection| -> Vec<String> {
        conn.prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    };
    for table in ["shell_runs", "worktree_generations", "station_flags"] {
        assert!(!tables(&conn).contains(&table.to_owned()), "{table}");
    }
    let columns = |conn: &Connection| -> Vec<String> {
        conn.prepare("SELECT name FROM pragma_table_info('turn_checkpoints')")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    };
    assert!(!columns(&conn).contains(&"generation".to_owned()));

    migrations.to_latest(&mut conn).unwrap();
    for table in ["shell_runs", "worktree_generations", "station_flags"] {
        assert!(tables(&conn).contains(&table.to_owned()), "{table}");
    }
    assert!(columns(&conn).contains(&"generation".to_owned()));
    // The new tables take rows; a missing admission flag reads as closed.
    assert!(!admission_open(&conn).unwrap());
    conn.execute(
        "INSERT INTO shell_runs (effect_id, session_id, provider, worktree_root, command,
                                 state, created_at)
         VALUES ('e', 's', 'claude', '/w', 'ls', 'queued', 1)",
        [],
    )
    .unwrap();
    assert!(root_unsettled(&conn, "/w").unwrap());
}
