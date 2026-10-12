//! `session.revert` end to end (DL-087) against `fake-claude sim` and
//! `fake-codex sim` in a Git workspace: a provider without a conversation
//! rollback gets a stored rejection that changes nothing, Codex rolls back
//! its thread before the files, a refused rollback changes nothing, and an
//! uncertain one blocks input.

use std::process::Command;

use fleetdeck_proto::TurnId;
use fleetdeck_proto::event::RevertState;

use super::*;

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
    assert!(out.status.success(), "git {args:?}");
    String::from_utf8(out.stdout).unwrap()
}

fn init_repo(dir: &Path) {
    git(dir, &["init", "-q", "-b", "main"]);
    std::fs::write(dir.join("a.txt"), "one\n").unwrap();
    git(dir, &["add", "."]);
    git(dir, &["commit", "-q", "-m", "init"]);
}

fn refs(dir: &Path) -> String {
    git(dir, &["for-each-ref", "--format=%(refname) %(objectname)"])
}

/// The finalized turns, oldest first.
async fn finalized(st: &Station, n: usize) -> Vec<TurnId> {
    let deadline = tokio::time::Instant::now() + WAIT;
    loop {
        let done: Vec<TurnId> = st
            .events()
            .await
            .into_iter()
            .filter_map(|e| match e.event {
                EventBody::Known(Event::TurnFinalized { turn_id, .. }) => Some(turn_id),
                _ => None,
            })
            .collect();
        if done.len() >= n {
            return done;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "{:#?}",
            kinds(&st.events().await)
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn revert_state(
    st: &Station,
    done: impl Fn(RevertState) -> bool,
) -> (RevertState, Option<String>) {
    st.wait_for("revert.updated", |e| match e {
        Event::RevertUpdated { state, reason, .. } if done(*state) => {
            Some((*state, reason.clone()))
        }
        _ => None,
    })
    .await
}

fn terminal(state: RevertState) -> bool {
    matches!(
        state,
        RevertState::Succeeded | RevertState::Failed | RevertState::ReconciliationRequired
    )
}

/// A Codex session in a Git workspace with one finalized turn that wrote
/// `a.txt`; the sim's rollback mode is set by an earlier echo turn.
async fn codex_with_turn(st: &mut Station, rollback: &str) -> (SessionId, PathBuf, TurnId) {
    let ws = st.workspace();
    let dir = PathBuf::from(&ws);
    init_repo(&dir);
    let mut spec = spec(&ws, "codex");
    spec.mode = None;
    let s = st
        .start_with(spec, Some(&format!("set-rollback {rollback}")))
        .await;
    finalized(st, 1).await;
    st.accepted(SessionCommand::Prompt {
        session_id: s.clone(),
        input: text("wait edit"),
    })
    .await;
    st.wait_for("the waiting delta", |e| match e {
        Event::MessageDelta { text, .. } if text == "waiting" => Some(()),
        _ => None,
    })
    .await;
    std::fs::write(dir.join("a.txt"), "one\nedited\n").unwrap();
    st.accepted(SessionCommand::Interrupt {
        session_id: s.clone(),
    })
    .await;
    let turns = finalized(st, 2).await;
    st.settled(&s).await;
    (s, dir, turns[1].clone())
}

#[tokio::test]
async fn a_claude_revert_is_a_stored_rejection_that_changes_nothing() {
    let mut st = Station::new(4).await;
    let ws = st.workspace();
    let dir = PathBuf::from(&ws);
    init_repo(&dir);
    let s = st.start(&ws, Some("hello")).await;
    let turn = finalized(&st, 1).await.remove(0);
    st.settled(&s).await;
    std::fs::write(dir.join("a.txt"), "one\nlater\n").unwrap();
    let before_refs = refs(&dir);
    let before_events = st.events().await.len();
    let before_effects = st.effect_states(&s).await;
    let envelope = CommandEnvelope {
        command_id: CommandId::from("revert-1"),
        target_incarnation: None,
        retry_until: i64::try_from(st.det.now_ms()).unwrap() + RETRY_WINDOW_MS,
        origin: CommandOrigin::Board,
        command: SessionCommand::Revert {
            session_id: s.clone(),
            turn_id: turn,
        },
    };
    let first = st
        .manager
        .submit(envelope.clone(), Caller::local())
        .await
        .unwrap();
    assert_eq!(first.receipt.state, ReceiptState::Rejected);
    let reject = first.receipt.reject.clone().unwrap();
    assert_eq!(reject.code, RejectCode::Unsupported, "{reject:?}");
    assert_eq!(
        reject.unsupported.map(|u| u.op),
        Some(fleetdeck_proto::Operation::Revert),
        "{reject:?}"
    );
    // Nothing changed: files, refs, history and execution effects.
    assert_eq!(
        std::fs::read_to_string(dir.join("a.txt")).unwrap(),
        "one\nlater\n"
    );
    assert_eq!(refs(&dir), before_refs);
    assert_eq!(st.events().await.len(), before_events);
    assert_eq!(st.effect_states(&s).await, before_effects);
    assert!(
        st.manager
            .store()
            .session_reverts(s.clone())
            .await
            .unwrap()
            .is_empty()
    );
    // A repeat returns the stored receipt.
    let again = st.manager.submit(envelope, Caller::local()).await.unwrap();
    assert_eq!(again.receipt, first.receipt);
    st.manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn a_codex_revert_rolls_back_the_thread_then_restores_the_files() {
    let mut st = Station::new(4).await;
    let (s, dir, turn) = codex_with_turn(&mut st, "ok").await;
    let before_refs = refs(&dir);
    st.accepted(SessionCommand::Revert {
        session_id: s.clone(),
        turn_id: turn,
    })
    .await;
    assert_eq!(
        revert_state(&st, terminal).await,
        (RevertState::Succeeded, None)
    );
    assert_eq!(std::fs::read_to_string(dir.join("a.txt")).unwrap(), "one\n");
    assert_eq!(refs(&dir), before_refs, "the checkpoint refs stay");
    let states: Vec<RevertState> = st
        .events()
        .await
        .into_iter()
        .filter_map(|e| match e.event {
            EventBody::Known(Event::RevertUpdated { state, .. }) => Some(state),
            _ => None,
        })
        .collect();
    assert_eq!(
        states,
        vec![
            RevertState::Admitted,
            RevertState::ConversationRolledBack,
            RevertState::Succeeded
        ]
    );
    let effects = st.settled(&s).await;
    assert_eq!(effects.last(), Some(&("revert", EffectState::Confirmed)));
    // The reverted turn is gone from the revertable history; the session
    // takes input again.
    assert_eq!(
        st.rejected(SessionCommand::Revert {
            session_id: s.clone(),
            turn_id: finalized(&st, 2).await[1].clone(),
        })
        .await,
        RejectCode::InvalidCommand
    );
    st.accepted(SessionCommand::Prompt {
        session_id: s.clone(),
        input: text("after"),
    })
    .await;
    finalized(&st, 3).await;
    st.manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn a_refused_codex_rollback_fails_the_revert_and_changes_no_file() {
    let mut st = Station::new(4).await;
    let (s, dir, turn) = codex_with_turn(&mut st, "refuse").await;
    let before_refs = refs(&dir);
    st.accepted(SessionCommand::Revert {
        session_id: s.clone(),
        turn_id: turn,
    })
    .await;
    let (state, reason) = revert_state(&st, terminal).await;
    assert_eq!(state, RevertState::Failed);
    assert!(reason.unwrap().starts_with("rollback_refused"));
    assert_eq!(
        std::fs::read_to_string(dir.join("a.txt")).unwrap(),
        "one\nedited\n"
    );
    assert_eq!(refs(&dir), before_refs);
    let effects = st.settled(&s).await;
    assert_eq!(
        effects.last(),
        Some(&("revert", EffectState::RetiredNotDispatched))
    );
    // A failed revert does not block input.
    st.accepted(SessionCommand::Prompt {
        session_id: s.clone(),
        input: text("after"),
    })
    .await;
    st.manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn a_provider_exit_during_the_rollback_requires_reconciliation_and_blocks_input() {
    let mut st = Station::new(4).await;
    let (s, dir, turn) = codex_with_turn(&mut st, "exit").await;
    st.accepted(SessionCommand::Revert {
        session_id: s.clone(),
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
    assert_eq!(
        std::fs::read_to_string(dir.join("a.txt")).unwrap(),
        "one\nedited\n"
    );
    let effects = st.settled(&s).await;
    assert!(
        effects.contains(&("revert", EffectState::OutcomeUnknown)),
        "{effects:?}"
    );
    let summary = st
        .manager
        .store()
        .summary(s.clone())
        .await
        .unwrap()
        .unwrap();
    assert!(summary.revert.unwrap().blocks_input());
    st.manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn a_revert_of_a_busy_session_is_rejected_before_any_change() {
    let mut st = Station::new(4).await;
    let (s, dir, turn) = codex_with_turn(&mut st, "ok").await;
    st.accepted(SessionCommand::Prompt {
        session_id: s.clone(),
        input: text("wait again"),
    })
    .await;
    st.wait_for("a running turn", |e| match e {
        Event::TurnStarted { turn_id, .. } if turn_id != &turn => Some(()),
        _ => None,
    })
    .await;
    assert_eq!(
        st.rejected(SessionCommand::Revert {
            session_id: s.clone(),
            turn_id: turn,
        })
        .await,
        RejectCode::InvalidState
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("a.txt")).unwrap(),
        "one\nedited\n"
    );
    assert!(
        st.manager
            .store()
            .session_reverts(s)
            .await
            .unwrap()
            .is_empty()
    );
    st.manager.shutdown(StopMode::Kill).await;
}
