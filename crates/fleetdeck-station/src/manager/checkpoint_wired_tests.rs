//! Turn checkpoints wired into dispatch and the pump (DL-084) against
//! `fake-claude sim`: the station publishes the start ref before the
//! prompt write, `turn.completed` is in the log before `turn.finalized`,
//! and a native queued follow-up gets explicit unavailable sides.

use std::process::Command;

use fleetdeck_proto::TurnId;
use fleetdeck_proto::event::{CheckpointSide, DiffSummary};

use super::*;
use crate::workspace::{CheckpointKey, GitOptions, Phase, Worktree};

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

fn init_repo(dir: &Path) {
    git(dir, &["init", "-q", "-b", "main"]);
    std::fs::write(dir.join("a.txt"), "one\n").unwrap();
    git(dir, &["add", "."]);
    git(dir, &["commit", "-q", "-m", "init"]);
}

type Final = (
    u64,
    TurnId,
    CheckpointSide,
    CheckpointSide,
    Option<DiffSummary>,
);

async fn finalized(st: &Station) -> Vec<Final> {
    st.events()
        .await
        .into_iter()
        .filter_map(|e| match e.event {
            EventBody::Known(Event::TurnFinalized {
                turn_id,
                checkpoint: Some(c),
                diff_summary,
            }) => Some((e.station_seq, turn_id, c.start, c.end, diff_summary)),
            _ => None,
        })
        .collect()
}

async fn wait_finalized(st: &Station, n: usize) -> Vec<Final> {
    let deadline = tokio::time::Instant::now() + WAIT;
    loop {
        let done = finalized(st).await;
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

fn completed_seq(events: &[EventEnvelope], turn: &TurnId) -> u64 {
    events
        .iter()
        .find_map(|e| match &e.event {
            EventBody::Known(Event::TurnCompleted { turn_id, .. }) if turn_id == turn => {
                Some(e.station_seq)
            }
            _ => None,
        })
        .unwrap()
}

fn unavailable(reason: &str) -> CheckpointSide {
    CheckpointSide::Unavailable {
        reason: reason.to_owned(),
    }
}

async fn streaming(st: &Station, after: usize) {
    let deadline = tokio::time::Instant::now() + WAIT;
    loop {
        let n = st
            .events()
            .await
            .iter()
            .filter(|e| matches!(e.event, EventBody::Known(Event::MessageDelta { .. })))
            .count();
        if n > after {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "{:#?}",
            kinds(&st.events().await)
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn a_live_turn_publishes_both_refs_and_finalizes_after_turn_completed() {
    let mut st = Station::new(4).await;
    let ws = st.workspace();
    init_repo(Path::new(&ws));
    let s = st.start(&ws, Some("wait for the host")).await;
    streaming(&st, 0).await;
    // The turn writes a tracked file.
    std::fs::write(Path::new(&ws).join("a.txt"), "one\ntwo\n").unwrap();
    st.accepted(SessionCommand::Interrupt {
        session_id: s.clone(),
    })
    .await;
    let done = wait_finalized(&st, 1).await;
    let (seq, turn, start, end, diff) = done[0].clone();
    assert!(completed_seq(&st.events().await, &turn) < seq);
    assert!(
        matches!(start, CheckpointSide::Available { .. }),
        "{start:?}"
    );
    assert!(matches!(end, CheckpointSide::Available { .. }), "{end:?}");
    assert_eq!(
        diff,
        Some(DiffSummary {
            files_changed: 1,
            insertions: 1,
            deletions: 0
        })
    );
    // Both refs are in the station namespace of this app turn.
    let rows = st
        .manager
        .store()
        .turn_checkpoints(s.clone())
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    let w = Worktree::open(Path::new(&ws), GitOptions::isolated())
        .await
        .unwrap();
    for phase in [Phase::Start, Phase::Completion] {
        let key = CheckpointKey {
            incarnation: st.manager.store().incarnation().as_str().to_owned(),
            session: s.as_str().to_owned(),
            turn: rows[0].app_turn.as_str().to_owned(),
            phase,
        };
        assert!(w.read(&key).await.unwrap().is_some(), "{phase:?}");
    }
    // A quiet second turn changes nothing.
    st.accepted(SessionCommand::Prompt {
        session_id: s.clone(),
        input: text("hello"),
    })
    .await;
    let done = wait_finalized(&st, 2).await;
    assert_eq!(
        done[1].4,
        Some(DiffSummary {
            files_changed: 0,
            insertions: 0,
            deletions: 0
        })
    );
    st.manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn a_follow_up_queued_behind_a_running_turn_gets_explicit_unavailable_sides() {
    let mut st = Station::new(4).await;
    let ws = st.workspace();
    init_repo(Path::new(&ws));
    let s = st.start(&ws, Some("wait for the host")).await;
    streaming(&st, 0).await;
    st.accepted(SessionCommand::FollowUp {
        session_id: s.clone(),
        input: UserInput::new("echo after".to_owned()),
    })
    .await;
    st.wait_for("queued follow-up", |e| {
        matches!(e, Event::InputUpdated { turn_id: None, .. }).then_some(())
    })
    .await;
    st.accepted(SessionCommand::Interrupt {
        session_id: s.clone(),
    })
    .await;
    let done = wait_finalized(&st, 2).await;
    assert!(matches!(done[0].2, CheckpointSide::Available { .. }));
    assert_eq!(done[0].3, unavailable("queued_input"));
    assert_eq!(done[1].2, unavailable("turn_running"));
    assert!(matches!(done[1].3, CheckpointSide::Available { .. }));
    assert_eq!(done[1].4, None);
    st.manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn a_workspace_outside_git_finalizes_with_not_a_git_worktree() {
    let mut st = Station::new(4).await;
    let ws = st.workspace();
    let s = st.start(&ws, Some("hello")).await;
    let done = wait_finalized(&st, 1).await;
    assert_eq!(done[0].2, unavailable("not_a_git_worktree"));
    assert_eq!(done[0].3, unavailable("not_a_git_worktree"));
    assert_eq!(done[0].4, None);
    let rows = st.manager.store().turn_checkpoints(s).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].state, crate::store::RowState::Finalized);
    st.manager.shutdown(StopMode::Kill).await;
}

/// A kill ends the provider: the adapter completes the running turn first,
/// and the turn still gets exactly one `turn.finalized` with decided sides.
#[tokio::test]
async fn a_killed_session_finalizes_its_running_turn_once() {
    let mut st = Station::new(4).await;
    let ws = st.workspace();
    init_repo(Path::new(&ws));
    let s = st.start(&ws, Some("wait for the host")).await;
    streaming(&st, 0).await;
    st.accepted(SessionCommand::Stop {
        session_id: s.clone(),
        how: StopMode::Kill,
    })
    .await;
    let done = wait_finalized(&st, 1).await;
    st.wait_for("session ended", |e| {
        matches!(e, Event::SessionEnded { .. }).then_some(())
    })
    .await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(finalized(&st).await.len(), 1);
    assert!(matches!(done[0].2, CheckpointSide::Available { .. }));
    assert!(
        !matches!(done[0].3, CheckpointSide::Unavailable { ref reason } if reason == "capture_failed")
    );
    let rows = st.manager.store().turn_checkpoints(s).await.unwrap();
    assert_eq!(rows[0].state, crate::store::RowState::Finalized);
    st.manager.shutdown(StopMode::Kill).await;
}

/// The same wiring for a Codex session: the checkpoints come from the
/// station's refs, not from the provider's diff events.
#[tokio::test]
async fn a_codex_turn_finalizes_from_the_station_checkpoints() {
    let mut st = Station::new(4).await;
    let ws = st.workspace();
    init_repo(Path::new(&ws));
    let mut spec = spec(&ws, "codex");
    spec.mode = None;
    let s = st.start_with(spec, Some("hello")).await;
    let done = wait_finalized(&st, 1).await;
    let (seq, turn, start, end, diff) = done[0].clone();
    assert!(completed_seq(&st.events().await, &turn) < seq);
    assert!(
        matches!(start, CheckpointSide::Available { .. }),
        "{start:?}"
    );
    assert!(matches!(end, CheckpointSide::Available { .. }), "{end:?}");
    assert_eq!(
        diff,
        Some(DiffSummary {
            files_changed: 0,
            insertions: 0,
            deletions: 0
        })
    );
    let rows = st.manager.store().turn_checkpoints(s).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].turn_id.as_ref(), Some(&turn));
    st.manager.shutdown(StopMode::Kill).await;
}
