//! `session.resolve_revert` (DL-097) end to end against `fake-codex sim`:
//! the owner's two resolutions of a revert in `reconciliation_required`.
//! `retry_restore` restores the files again only after a confirmed
//! conversation rollback and only when the files still equal a checkpoint;
//! `reconciled` ends the revert, unblocks input and leaves no earlier turn
//! revertable.

use fleetdeck_proto::command::RevertResolution;

use super::*;

/// The revert states of `session`, in log order.
async fn states(st: &Station, session: &SessionId) -> Vec<(RevertState, Option<String>)> {
    st.events()
        .await
        .into_iter()
        .filter(|e| &e.session_id == session)
        .filter_map(|e| match e.event {
            EventBody::Known(Event::RevertUpdated { state, reason, .. }) => Some((state, reason)),
            _ => None,
        })
        .collect()
}

/// Waits until `session` has `n` revert states; returns them.
async fn wait_states(
    st: &Station,
    session: &SessionId,
    n: usize,
) -> Vec<(RevertState, Option<String>)> {
    let deadline = tokio::time::Instant::now() + WAIT;
    loop {
        let all = states(st, session).await;
        if all.len() >= n {
            return all;
        }
        assert!(tokio::time::Instant::now() < deadline, "{all:?}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn resolve(session: &SessionId, revert_id: &str, resolution: RevertResolution) -> SessionCommand {
    SessionCommand::ResolveRevert {
        session_id: session.clone(),
        revert_id: revert_id.to_owned(),
        resolution,
    }
}

fn recon(reason: &str) -> (RevertState, Option<String>) {
    (RevertState::ReconciliationRequired, Some(reason.to_owned()))
}

fn read(dir: &Path) -> String {
    std::fs::read_to_string(dir.join("a.txt")).unwrap()
}

/// A Codex session whose revert ends in `reconciliation_required`: the
/// sim's `mode` decides how. Returns the session, its workspace, the
/// reverted turn and the revert id.
async fn reconciling(st: &mut Station, mode: &str, reason: &str) -> (SessionId, PathBuf, String) {
    let (s, dir, turn) = codex_with_turn(st, mode).await;
    st.accepted(SessionCommand::Revert {
        session_id: s.clone(),
        turn_id: turn,
    })
    .await;
    let all = wait_states(st, &s, 2).await;
    let last = loop {
        let all = states(st, &s).await;
        if let Some(last) = all.last().filter(|(state, _)| terminal(*state)) {
            break last.clone();
        }
        assert!(all.len() < 5, "{all:?}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    assert_eq!(all[0].0, RevertState::Admitted);
    assert_eq!(last, recon(reason));
    let rows = st.manager.store().session_reverts(s.clone()).await.unwrap();
    (s, dir, rows[0].revert.revert_id.clone())
}

#[tokio::test]
async fn a_retry_restore_waits_until_the_files_equal_a_checkpoint_then_finishes_the_revert() {
    let mut st = Station::new(4).await;
    // The sim confirms the rollback, but a writer changes `a.txt` first.
    let (s, dir, revert_id) = reconciling(&mut st, "ok-write", "workspace_changed").await;
    assert_eq!(
        states(&st, &s).await,
        vec![
            (RevertState::Admitted, None),
            (RevertState::ConversationRolledBack, None),
            recon("workspace_changed"),
        ]
    );
    assert_eq!(read(&dir), "a writer\n");
    assert_eq!(
        st.rejected(SessionCommand::Prompt {
            session_id: s.clone(),
            input: text("blocked"),
        })
        .await,
        RejectCode::InvalidState
    );
    // A retry while the files still differ: nothing is written.
    st.accepted(resolve(&s, &revert_id, RevertResolution::RetryRestore))
        .await;
    let all = wait_states(&st, &s, 5).await;
    assert_eq!(
        all[3..],
        [
            (RevertState::ConversationRolledBack, None),
            recon("workspace_changed")
        ]
    );
    assert_eq!(read(&dir), "a writer\n");
    // The owner puts the files back to the completion checkpoint.
    std::fs::write(dir.join("a.txt"), "one\nedited\n").unwrap();
    st.accepted(resolve(&s, &revert_id, RevertResolution::RetryRestore))
        .await;
    let all = wait_states(&st, &s, 7).await;
    assert_eq!(
        all[5..],
        [
            (RevertState::ConversationRolledBack, None),
            (RevertState::Succeeded, None)
        ]
    );
    assert_eq!(read(&dir), "one\n");
    // The rollback was sent once only: a retry restores files, it never
    // repeats the conversation rollback. A restore that ends in
    // reconciliation reports an unknown outcome, like the first one.
    let effects = st.settled(&s).await;
    assert_eq!(
        effects
            .iter()
            .filter(|(kind, _)| *kind == "revert")
            .map(|(_, state)| *state)
            .collect::<Vec<_>>(),
        vec![
            EffectState::OutcomeUnknown,
            EffectState::OutcomeUnknown,
            EffectState::Confirmed,
        ]
    );
    // A resolved revert cannot be resolved again; input works.
    assert_eq!(
        st.rejected(resolve(&s, &revert_id, RevertResolution::Reconciled))
            .await,
        RejectCode::InvalidState
    );
    st.accepted(SessionCommand::Prompt {
        session_id: s.clone(),
        input: text("after"),
    })
    .await;
    st.manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn reconciled_unblocks_input_and_leaves_no_earlier_turn_revertable() {
    let mut st = Station::new(4).await;
    let (s, dir, revert_id) = reconciling(&mut st, "ok-write", "workspace_changed").await;
    let first = finalized(&st, 1).await.remove(0);
    let before = st.effect_states(&s).await;
    st.accepted(resolve(&s, &revert_id, RevertResolution::Reconciled))
        .await;
    let all = wait_states(&st, &s, 4).await;
    assert_eq!(
        all[3],
        (RevertState::Reconciled, Some("owner_reconciled".to_owned()))
    );
    // No file changed and no effect was added.
    assert_eq!(read(&dir), "a writer\n");
    assert_eq!(st.effect_states(&s).await.len(), before.len());
    let summary = st
        .manager
        .store()
        .summary(s.clone())
        .await
        .unwrap()
        .unwrap();
    assert!(!summary.revert.unwrap().blocks_input());
    assert!(
        st.manager
            .store()
            .revertable_turn_checkpoints(s.clone())
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        st.rejected(SessionCommand::Revert {
            session_id: s.clone(),
            turn_id: first,
        })
        .await,
        RejectCode::InvalidCommand
    );
    st.accepted(SessionCommand::Prompt {
        session_id: s.clone(),
        input: text("after"),
    })
    .await;
    st.manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn an_unconfirmed_rollback_cannot_be_retried_but_can_be_reconciled_after_the_session_ended() {
    let mut st = Station::new(4).await;
    let (s, dir, revert_id) = reconciling(&mut st, "exit", "rollback_uncertain").await;
    st.wait_for("the end", |e| {
        matches!(e, Event::SessionEnded { .. }).then_some(())
    })
    .await;
    let d = st
        .send(resolve(&s, &revert_id, RevertResolution::RetryRestore))
        .await;
    let reject = d.receipt.reject.unwrap();
    assert_eq!(reject.code, RejectCode::InvalidState);
    assert!(
        reject.message.contains("not confirmed"),
        "{}",
        reject.message
    );
    // Wrong ids are stored rejections and change nothing.
    assert_eq!(
        st.rejected(resolve(&s, "rv-nope", RevertResolution::Reconciled))
            .await,
        RejectCode::InvalidCommand
    );
    assert_eq!(
        st.rejected(resolve(
            &SessionId::from("nope"),
            &revert_id,
            RevertResolution::Reconciled
        ))
        .await,
        RejectCode::UnknownSession
    );
    assert_eq!(states(&st, &s).await.len(), 2);
    st.accepted(resolve(&s, &revert_id, RevertResolution::Reconciled))
        .await;
    assert_eq!(
        wait_states(&st, &s, 3).await[2],
        (RevertState::Reconciled, Some("owner_reconciled".to_owned()))
    );
    assert_eq!(read(&dir), "one\nedited\n");
    st.manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn a_reconciliation_while_the_rollback_has_no_answer_keeps_the_fence_until_the_session_stops()
{
    let mut st = Station::new(4).await;
    // The sim never answers the rollback; the adapter gives up after 500 ms.
    let (s, dir, revert_id) = reconciling(&mut st, "hang", "rollback_uncertain").await;
    let d = st
        .send(resolve(&s, &revert_id, RevertResolution::Reconciled))
        .await;
    let reject = d.receipt.reject.unwrap();
    assert_eq!(reject.code, RejectCode::InvalidState);
    assert!(
        reject.message.contains("stop the session"),
        "{}",
        reject.message
    );
    assert_eq!(states(&st, &s).await.len(), 2);
    // The fence still holds: the session takes no input, and no session
    // starts in the worktree.
    let ws = st.workspace();
    assert_eq!(
        st.rejected(SessionCommand::Prompt {
            session_id: s.clone(),
            input: text("hello"),
        })
        .await,
        RejectCode::InvalidState
    );
    assert_eq!(
        st.rejected(SessionCommand::Start {
            spec: spec(&ws, "claude"),
            prompt: Some(text("hello")),
            workspace: None,
            setup_cmd: None,
            batch: None,
        })
        .await,
        RejectCode::InvalidState
    );
    // The owner stops the session; then the reconciliation is accepted.
    st.accepted(SessionCommand::Stop {
        session_id: s.clone(),
        how: StopMode::Kill,
    })
    .await;
    st.wait_for("the end", |e| {
        matches!(e, Event::SessionEnded { .. }).then_some(())
    })
    .await;
    st.accepted(resolve(&s, &revert_id, RevertResolution::Reconciled))
        .await;
    assert_eq!(
        wait_states(&st, &s, 3).await[2],
        (RevertState::Reconciled, Some("owner_reconciled".to_owned()))
    );
    assert_eq!(read(&dir), "one\nedited\n");
    // The worktree is free again.
    st.start(&ws, Some("hello")).await;
    st.manager.shutdown(StopMode::Kill).await;
}
