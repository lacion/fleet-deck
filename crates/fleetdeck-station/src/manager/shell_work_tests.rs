//! The `!` shell commands and the station work (B8): drain, the restart
//! check and freeze, the start-up settlement and the advance loop.

use super::*;
use crate::manager::turns::Fences;
use crate::manager::{drain, restart_gate, shell_advance, shell_decide};

#[tokio::test]
async fn drain_and_the_restart_check_see_an_unsettled_row() {
    let mut st = Station::new(4).await;
    let root = st.ws_root();
    let s = st.idle_here().await;
    assert!(!st.manager.shell_work_open().await.unwrap());
    let report = st.manager.drain(Duration::from_millis(100)).await.unwrap();
    assert!(report.idle, "{report:?}");

    st.plant("row-1", &s, &root, ShellState::Running).await;
    assert!(st.manager.shell_work_open().await.unwrap());
    let report = st.manager.drain(Duration::from_millis(150)).await.unwrap();
    assert!(!report.idle);
    assert_eq!(report.busy, vec![s.to_string()]);

    // A settled row that waits for its advance still counts.
    let store = st.manager.store().clone();
    assert!(
        store
            .shell_settle("row-1".to_owned(), ShellState::Done, None, None, None)
            .await
            .unwrap()
    );
    assert!(st.manager.shell_work_open().await.unwrap());
    st.advanced().await;
    assert!(!st.manager.shell_work_open().await.unwrap());
    let report = st.manager.drain(WAIT).await.unwrap();
    assert!(report.idle, "{report:?}");
    st.manager.shutdown(StopMode::Kill).await;
}

#[test]
fn a_shell_command_counts_as_work_in_the_drain_and_the_restart_freeze() {
    let command = SessionCommand::ShellCommand {
        session_id: "s".into(),
        command: "ls".to_owned(),
    };
    assert!(drain::creates_work(&command));
    assert!(restart_gate::may_resume(&command));
    let release = SessionCommand::ReleaseShell {
        session_id: "s".into(),
        effect_id: "e".to_owned(),
    };
    assert!(!drain::creates_work(&release));
    assert!(shell_decide::needs_root(&command));
    assert!(!shell_decide::needs_root(&release));
}

#[tokio::test]
async fn a_draining_a_closed_admission_and_a_restart_refuse_a_new_shell_command() {
    let mut st = Station::new(4).await;
    let deck = st.paired_deck().await;
    let s = st.idle_here().await;

    st.manager.begin_restart();
    let d = st.as_board(&deck, shell(&s, "ls")).await;
    assert_eq!(
        refused(&d),
        pairs(RejectCode::InvalidState, "the station is restarting")
    );
    st.manager.end_restart();

    st.manager
        .store()
        .shell_close_admission_and_cancel_all()
        .await
        .unwrap();
    let d = st.as_board(&deck, shell(&s, "ls")).await;
    assert_eq!(
        refused(&d),
        pairs(RejectCode::InvalidState, "Fleet Deck is stopping.")
    );
    st.manager.store().shell_open_admission().await.unwrap();

    let report = st.manager.drain(WAIT).await.unwrap();
    assert!(report.idle);
    let d = st.as_board(&deck, shell(&s, "ls")).await;
    assert_eq!(d.receipt.state, ReceiptState::Rejected);
    assert_eq!(refused(&d).0, RejectCode::InvalidState);
    st.manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn the_start_up_settles_dead_rows_runs_the_pending_advances_and_opens_the_admission() {
    let mut st = Station::new(4).await;
    let root = st.ws_root();
    let s = st.idle_here().await;
    let store = st.manager.store().clone();
    st.plant("gone", &s, &root, ShellState::Queued).await;
    st.plant("settled", &s, &root, ShellState::Running).await;
    assert!(
        store
            .shell_settle(
                "settled".to_owned(),
                ShellState::Done,
                Some(0),
                Some("kept".into()),
                None
            )
            .await
            .unwrap()
    );
    store.shell_close_admission_and_cancel_all().await.unwrap();
    assert!(!store.shell_admission_open().await.unwrap());

    shell_advance::start_up(&store, &Fences::default())
        .await
        .unwrap();
    // `gone` has no effect row: it settles cancelled. `settled` keeps its
    // state and result and only gets its advance.
    assert_eq!(st.shell_row("gone").await.state, ShellState::Cancelled);
    let settled = st.shell_row("settled").await;
    assert_eq!(settled.state, ShellState::Done);
    assert_eq!(settled.stdout_tail.as_deref(), Some("kept"));
    assert!(store.shell_pending_advances().await.unwrap().is_empty());
    assert!(!store.shell_root_unsettled(root.clone()).await.unwrap());
    assert_eq!(store.root_generation(root).await.unwrap(), 2);
    assert!(store.shell_admission_open().await.unwrap());
    st.manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn the_advance_loop_advances_a_settled_row_once_and_clears_its_marker() {
    let mut st = Station::new(4).await;
    let root = st.ws_root();
    let s = st.idle_here().await;
    let store = st.manager.store().clone();
    st.plant("row-1", &s, &root, ShellState::Running).await;
    assert!(
        store
            .shell_settle("row-1".to_owned(), ShellState::Failed, Some(2), None, None)
            .await
            .unwrap()
    );
    assert!(st.shell_row("row-1").await.advance_pending);
    st.manager.shell_notify.notify_one();
    st.advanced().await;
    assert!(!st.shell_row("row-1").await.advance_pending);
    assert_eq!(store.root_generation(root.clone()).await.unwrap(), 1);
    // A repeated run changes nothing.
    assert_eq!(
        shell_advance::advance_pending(&store, &Fences::default())
            .await
            .unwrap(),
        0
    );
    assert_eq!(store.root_generation(root).await.unwrap(), 1);
    st.manager.shutdown(StopMode::Kill).await;
}
