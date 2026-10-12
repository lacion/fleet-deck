//! A revert and the other sessions of its worktree (DL-093, findings 1 and
//! 2 of the review sa-3491e52e), end to end against `fake-claude sim` and
//! `fake-codex sim`: every session in the worktree must be quiet before a
//! revert is admitted (a running turn, a natively queued input, background
//! work), an open revert holds the worktree against the input and the
//! starts of other sessions, and a second revert after a successful one
//! sees only the turns that are not reverted.

use super::*;
use crate::store::RevertStep;

/// The rejection message of `command`.
async fn refusal(st: &mut Station, command: SessionCommand) -> (RejectCode, String) {
    let d = st.send(command).await;
    assert_eq!(d.receipt.state, ReceiptState::Rejected, "{:?}", d.receipt);
    let reject = d.receipt.reject.unwrap();
    (reject.code, reject.message)
}

/// The number of `session`'s events that `pick` accepts.
async fn count(st: &Station, session: &SessionId, pick: impl Fn(&Event) -> bool) -> usize {
    st.events()
        .await
        .into_iter()
        .filter(|e| &e.session_id == session)
        .filter(|e| matches!(&e.event, EventBody::Known(event) if pick(event)))
        .count()
}

/// Waits until `session` has `n` events that `pick` accepts.
async fn wait_count(
    st: &Station,
    what: &str,
    session: &SessionId,
    n: usize,
    pick: impl Fn(&Event) -> bool + Copy,
) {
    let deadline = tokio::time::Instant::now() + WAIT;
    while count(st, session, pick).await < n {
        assert!(tokio::time::Instant::now() < deadline, "no {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn is_finalized(e: &Event) -> bool {
    matches!(e, Event::TurnFinalized { .. })
}

fn is_started(e: &Event) -> bool {
    matches!(e, Event::TurnStarted { .. })
}

#[tokio::test]
async fn a_revert_waits_for_a_turn_and_a_queued_input_of_another_session_in_the_worktree() {
    let mut st = Station::new(4).await;
    let (a, dir, turn) = codex_with_turn(&mut st, "ok").await;
    let ws = st.workspace();
    // B: a running turn and a second input that the provider queues.
    let b = st.start(&ws, Some("wait one")).await;
    wait_count(&st, "B's first turn", &b, 1, is_started).await;
    st.accepted(SessionCommand::Prompt {
        session_id: b.clone(),
        input: text("wait two"),
    })
    .await;
    let revert = SessionCommand::Revert {
        session_id: a.clone(),
        turn_id: turn.clone(),
    };
    let (code, message) = refusal(&mut st, revert.clone()).await;
    assert_eq!(code, RejectCode::InvalidState);
    assert!(message.contains(b.as_str()), "{message}");
    // B's first turn ends; its queued input keeps the worktree busy.
    st.accepted(SessionCommand::Interrupt {
        session_id: b.clone(),
    })
    .await;
    wait_count(&st, "B's first finalized turn", &b, 1, is_finalized).await;
    let (code, message) = refusal(&mut st, revert.clone()).await;
    assert_eq!(code, RejectCode::InvalidState);
    assert!(message.contains(b.as_str()), "{message}");
    wait_count(&st, "B's second turn", &b, 2, is_started).await;
    st.accepted(SessionCommand::Interrupt {
        session_id: b.clone(),
    })
    .await;
    wait_count(&st, "B's second finalized turn", &b, 2, is_finalized).await;
    st.settled(&b).await;
    // B is quiet: the revert runs.
    st.accepted(revert).await;
    assert_eq!(
        revert_state(&st, terminal).await,
        (RevertState::Succeeded, None)
    );
    assert_eq!(std::fs::read_to_string(dir.join("a.txt")).unwrap(), "one\n");
    assert!(
        st.manager
            .store()
            .session_reverts(b)
            .await
            .unwrap()
            .is_empty()
    );
    st.manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn a_revert_waits_for_background_work_of_another_session_in_the_worktree() {
    let mut st = Station::new(4).await;
    let (a, dir, turn) = codex_with_turn(&mut st, "ok").await;
    let ws = st.workspace();
    let mut spec = spec(&ws, "codex");
    spec.mode = None;
    let b = st.start_with(spec, Some("bg sleep 1")).await;
    wait_count(
        &st,
        "B's background work",
        &b,
        1,
        |e| matches!(e, Event::BackgroundUpdated { tasks } if !tasks.is_empty()),
    )
    .await;
    let (code, message) = refusal(
        &mut st,
        SessionCommand::Revert {
            session_id: a.clone(),
            turn_id: turn,
        },
    )
    .await;
    assert_eq!(code, RejectCode::InvalidState);
    assert!(message.contains("background work"), "{message}");
    assert!(
        st.manager
            .store()
            .session_reverts(a)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("a.txt")).unwrap(),
        "one\nedited\n"
    );
    st.manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn an_open_revert_holds_the_worktree_against_other_sessions() {
    let mut st = Station::new(4).await;
    let (a, dir, turn) = codex_with_turn(&mut st, "exit").await;
    let ws = st.workspace();
    let b = st.start(&ws, Some("hello")).await;
    wait_count(&st, "B's finalized turn", &b, 1, is_finalized).await;
    st.settled(&b).await;
    st.accepted(SessionCommand::Revert {
        session_id: a.clone(),
        turn_id: turn,
    })
    .await;
    assert_eq!(
        revert_state(&st, terminal).await,
        (
            RevertState::ReconciliationRequired,
            Some("rollback_uncertain".to_owned())
        )
    );
    // B takes no input and no new session starts in the worktree.
    let (code, message) = refusal(
        &mut st,
        SessionCommand::Prompt {
            session_id: b.clone(),
            input: text("hello again"),
        },
    )
    .await;
    assert_eq!(code, RejectCode::InvalidState);
    assert!(message.contains(a.as_str()), "{message}");
    let (code, message) = refusal(
        &mut st,
        SessionCommand::Start {
            spec: spec(&ws, "claude"),
            prompt: Some(text("hello")),
            workspace: None,
        },
    )
    .await;
    assert_eq!(code, RejectCode::InvalidState);
    assert!(message.contains(a.as_str()), "{message}");
    assert_eq!(count(&st, &b, is_started).await, 1);
    assert_eq!(
        std::fs::read_to_string(dir.join("a.txt")).unwrap(),
        "one\nedited\n"
    );
    // B can still be stopped.
    st.accepted(SessionCommand::Stop {
        session_id: b.clone(),
        how: StopMode::Graceful,
    })
    .await;
    st.manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn a_second_revert_after_a_successful_one_reverts_the_earlier_turn() {
    let mut st = Station::new(4).await;
    let (a, dir, turn) = codex_with_turn(&mut st, "ok").await;
    st.accepted(SessionCommand::Revert {
        session_id: a.clone(),
        turn_id: turn,
    })
    .await;
    assert_eq!(
        revert_state(&st, terminal).await,
        (RevertState::Succeeded, None)
    );
    st.settled(&a).await;
    let first = finalized(&st, 2).await.remove(0);
    st.accepted(SessionCommand::Revert {
        session_id: a.clone(),
        turn_id: first,
    })
    .await;
    let deadline = tokio::time::Instant::now() + WAIT;
    let reverts = loop {
        let rows = st.manager.store().session_reverts(a.clone()).await.unwrap();
        if rows.len() == 2 && !rows[1].step.is_running() {
            break rows;
        }
        assert!(tokio::time::Instant::now() < deadline, "{rows:?}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    assert_eq!(
        reverts
            .iter()
            .map(|r| (r.step, r.reason.clone()))
            .collect::<Vec<_>>(),
        vec![(RevertStep::Succeeded, None), (RevertStep::Succeeded, None)]
    );
    assert_eq!(std::fs::read_to_string(dir.join("a.txt")).unwrap(), "one\n");
    st.manager.shutdown(StopMode::Kill).await;
}
