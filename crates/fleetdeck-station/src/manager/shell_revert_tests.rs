//! The revert barrier of the `!` shell commands (B8): a revert across a shell
//! command is refused, the same generation is allowed, an unsettled row
//! refuses, and a start checkpoint stores the folder generation.

use std::process::Command;

use fleetdeck_proto::TurnId;

use super::*;

fn git(dir: &Path, args: &[&str]) {
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
}

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
        assert!(tokio::time::Instant::now() < deadline, "no finalized turn");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// A Codex session in a Git workspace with two finalized turns; the second
/// wrote `a.txt`. Returns the session and the ids of both turns.
async fn codex_with_turns(st: &mut Station) -> (SessionId, Vec<TurnId>) {
    let ws = st.workspace();
    let dir = PathBuf::from(&ws);
    git(&dir, &["init", "-q", "-b", "main"]);
    std::fs::write(dir.join("a.txt"), "one\n").unwrap();
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "init"]);
    let mut spec = spec(&ws, "codex");
    spec.mode = None;
    let s = st.start_with(spec, Some("set-rollback ok")).await;
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
    (s, turns)
}

fn revert(s: &SessionId, turn: &TurnId) -> SessionCommand {
    SessionCommand::Revert {
        session_id: s.clone(),
        turn_id: turn.clone(),
    }
}

#[tokio::test]
async fn a_start_checkpoint_stores_the_folder_generation() {
    let mut st = Station::new(4).await;
    let (s, _) = codex_with_turns(&mut st).await;
    let generations = |st: &Station| {
        let store = st.manager.store().clone();
        let s = s.clone();
        async move {
            store
                .turn_checkpoints(s)
                .await
                .unwrap()
                .into_iter()
                .map(|r| r.generation)
                .collect::<Vec<_>>()
        }
    };
    // A Git folder that never saw a shell stores generation 0.
    assert_eq!(generations(&st).await, vec![Some(0), Some(0)]);
    let root = st.ws_root();
    st.plant("row-1", &s, &root, ShellState::Running).await;
    let store = st.manager.store().clone();
    assert!(
        store
            .shell_settle("row-1".to_owned(), ShellState::Done, Some(0), None, None)
            .await
            .unwrap()
    );
    st.advanced().await;
    st.accepted(SessionCommand::Prompt {
        session_id: s.clone(),
        input: text("hello"),
    })
    .await;
    finalized(&st, 3).await;
    assert_eq!(generations(&st).await, vec![Some(0), Some(0), Some(1)]);
    st.manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn a_revert_across_a_shell_command_is_refused_and_the_same_generation_is_allowed() {
    let mut st = Station::new(4).await;
    let (s, turns) = codex_with_turns(&mut st).await;
    let root = st.ws_root();
    st.plant("row-1", &s, &root, ShellState::Running).await;

    // An unsettled row refuses the revert (root busy at the admission).
    let d = st.send(revert(&s, &turns[1])).await;
    assert_eq!(
        refused(&d),
        pairs(
            RejectCode::InvalidState,
            "A shell command is running in this folder."
        )
    );
    let store = st.manager.store().clone();
    assert!(
        store
            .shell_settle("row-1".to_owned(), ShellState::Done, Some(0), None, None)
            .await
            .unwrap()
    );
    st.advanced().await;

    // The turns before the shell ran at a lower generation: refused.
    let d = st.send(revert(&s, &turns[1])).await;
    assert_eq!(
        refused(&d),
        pairs(
            RejectCode::InvalidState,
            "A shell command ran in this folder after this turn; the files cannot be reverted across it."
        )
    );
    // A turn after the shell stores the new generation and can be reverted.
    st.accepted(SessionCommand::Prompt {
        session_id: s.clone(),
        input: text("hello"),
    })
    .await;
    let turns = finalized(&st, 3).await;
    st.settled(&s).await;
    let d = st.send(revert(&s, &turns[2])).await;
    assert_eq!(d.receipt.state, ReceiptState::Accepted, "{:?}", d.receipt);
    st.manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn the_barrier_reads_a_missing_generation_as_zero() {
    let mut st = Station::new(4).await;
    let (s, turns) = codex_with_turns(&mut st).await;
    let root = st.ws_root();
    let store = st.manager.store().clone();
    // An older row without a stored generation.
    let rows = store.turn_checkpoints(s.clone()).await.unwrap();
    assert!(rows.iter().all(|r| r.generation == Some(0)));
    let app_turn = rows[1].app_turn.as_str().to_owned();
    assert_eq!(
        store
            .shell_barrier(root.clone(), app_turn.clone())
            .await
            .unwrap(),
        None
    );
    st.plant("row-1", &s, &root, ShellState::Running).await;
    assert_eq!(
        store
            .shell_barrier(root.clone(), app_turn.clone())
            .await
            .unwrap(),
        Some(crate::store::shell_runs::Barrier::Running)
    );
    assert!(
        store
            .shell_settle("row-1".to_owned(), ShellState::Done, None, None, None)
            .await
            .unwrap()
    );
    st.advanced().await;
    assert_eq!(
        store.shell_barrier(root, app_turn).await.unwrap(),
        Some(crate::store::shell_runs::Barrier::RanAfterTurn)
    );
    drop(turns);
    st.manager.shutdown(StopMode::Kill).await;
}
