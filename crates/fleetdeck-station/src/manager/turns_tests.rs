//! The session checkpoint wiring (DL-084) against real Git and the store,
//! fed with the event sequences of the providers: the start ref exists
//! before the input write, a later dispatch runs the due completion capture
//! of an earlier turn first (delayed finalization with T2 writes),
//! background writers after the parent turn, native queued turns, process
//! end, provider-initiated turns and inputs without a turn.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use fleetdeck_proto::event::{
    BackgroundTask, CheckpointSide, DiffSummary, InputState, ProviderKind, ProviderRefs, TurnStatus,
};
use fleetdeck_proto::{Event, EventBody, InputId, TurnId};
use fleetdeck_testkit::Det;

use super::SessionTurns;
use crate::store::{
    NewSession, OfflineApprovalPolicy, ReadResult, RowState, Store, StoreConfig, TurnCheckpointRow,
};
use crate::workspace::{CheckpointKey, GitOptions, Limits, Phase, Worktree};

const S1: &str = "s1";

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

fn repo() -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("repo");
    std::fs::create_dir(&dir).unwrap();
    git(&dir, &["init", "-q", "-b", "main"]);
    std::fs::write(dir.join("a.txt"), "one\n").unwrap();
    std::fs::write(dir.join("b.txt"), "two\n").unwrap();
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "init"]);
    (tmp, dir)
}

async fn setup(det: &Det, cwd: &Path) -> (Store, Arc<SessionTurns>) {
    let s = Store::open_in_memory(det.clock(), det.ids(), StoreConfig::default())
        .await
        .unwrap();
    s.recover(Vec::new()).await.unwrap();
    s.create_session(NewSession {
        session_id: S1.into(),
        provider: ProviderKind::Claude,
        provider_instance: "claude".into(),
        cwd: cwd.display().to_string(),
        worktree: None,
        branch: None,
        mode: None,
        title: None,
        offline_approval_policy: OfflineApprovalPolicy::Wait,
    })
    .await
    .unwrap();
    let turns = SessionTurns::open(
        s.clone(),
        det.ids(),
        GitOptions::isolated(),
        Limits::default(),
        S1.into(),
        cwd,
    )
    .await;
    turns.set_process("proc-1".to_owned().into());
    (s, turns)
}

fn started(turn: &str) -> Event {
    Event::TurnStarted {
        run_id: "run-1".into(),
        turn_id: turn.into(),
        provider_refs: ProviderRefs::new(),
    }
}

fn completed(turn: &str) -> Event {
    Event::TurnCompleted {
        turn_id: turn.into(),
        status: TurnStatus::Done,
        usage: None,
    }
}

fn input(id: &str, turn: Option<&str>, state: InputState) -> Event {
    Event::InputUpdated {
        input_id: id.into(),
        turn_id: turn.map(TurnId::from),
        state,
    }
}

fn background(n: usize) -> Event {
    Event::BackgroundUpdated {
        tasks: (0..n)
            .map(|i| BackgroundTask {
                task_id: format!("b{i}"),
                task_kind: "local_bash".into(),
            })
            .collect(),
    }
}

fn ended() -> Event {
    Event::SessionEnded {
        reason: "process_exit".into(),
        exit_code: Some(0),
    }
}

/// The dispatch of a prompt: fence, start capture, then the write.
async fn dispatch(turns: &SessionTurns, id: &str) {
    let fence = turns.fence().await;
    turns.begin_input(&fence, &InputId::from(id)).await;
    turns.input_sent(&InputId::from(id));
}

async fn rows(s: &Store) -> Vec<TurnCheckpointRow> {
    s.turn_checkpoints(S1.into()).await.unwrap()
}

async fn row_of_input(s: &Store, id: &str) -> TurnCheckpointRow {
    rows(s)
        .await
        .into_iter()
        .find(|r| r.input_id.as_ref().map(InputId::as_str) == Some(id))
        .unwrap()
}

async fn finalized(
    s: &Store,
) -> Vec<(TurnId, CheckpointSide, CheckpointSide, Option<DiffSummary>)> {
    let ReadResult::Events { events, .. } = s.read_after(0, 1000).await.unwrap() else {
        panic!("snapshot required");
    };
    events
        .into_iter()
        .filter_map(|e| match e.event {
            EventBody::Known(Event::TurnFinalized {
                turn_id,
                checkpoint: Some(c),
                diff_summary,
            }) => Some((turn_id, c.start, c.end, diff_summary)),
            _ => None,
        })
        .collect()
}

fn key(s: &Store, row: &TurnCheckpointRow, phase: Phase) -> CheckpointKey {
    CheckpointKey {
        incarnation: s.incarnation().as_str().to_owned(),
        session: S1.to_owned(),
        turn: row.app_turn.as_str().to_owned(),
        phase,
    }
}

fn unavailable(reason: &str) -> CheckpointSide {
    CheckpointSide::Unavailable {
        reason: reason.to_owned(),
    }
}

fn is_available(side: &CheckpointSide) -> bool {
    matches!(side, CheckpointSide::Available { .. })
}

#[tokio::test]
async fn a_quiet_turn_has_a_start_ref_before_the_write_and_finalizes_from_both_refs() {
    let det = Det::new();
    let (_tmp, dir) = repo();
    let (s, turns) = setup(&det, &dir).await;
    assert!(turns.has_worktree());
    dispatch(&turns, "in-1").await;
    // The start ref exists before the input is written.
    let row = row_of_input(&s, "in-1").await;
    let w = Worktree::open(&dir, GitOptions::isolated()).await.unwrap();
    let start = w.read(&key(&s, &row, Phase::Start)).await.unwrap().unwrap();
    assert_eq!(
        row.start,
        crate::store::RowSide::Available(start.commit.clone())
    );
    // The turn writes a tracked file.
    assert!(!turns.observe(&[started("turn-1")]).await);
    std::fs::write(dir.join("a.txt"), "one\nmore\n").unwrap();
    let due = turns
        .observe(&[
            input("in-1", Some("turn-1"), InputState::Started),
            completed("turn-1"),
        ])
        .await;
    assert!(due, "turn.completed makes the completion capture due");
    // `turn.completed` is observed after it is stored; nothing finalized yet.
    assert!(finalized(&s).await.is_empty());
    Arc::clone(&turns).capture_due().await;
    let done = finalized(&s).await;
    assert_eq!(done.len(), 1);
    let (turn, start_side, end_side, diff) = &done[0];
    assert_eq!(turn.as_str(), "turn-1");
    assert_eq!(
        start_side,
        &CheckpointSide::Available {
            commit: start.commit
        }
    );
    assert!(is_available(end_side));
    assert_eq!(
        *diff,
        Some(DiffSummary {
            files_changed: 1,
            insertions: 1,
            deletions: 0
        })
    );
    let row = row_of_input(&s, "in-1").await;
    assert_eq!(row.state, RowState::Finalized);
    assert!(
        w.read(&key(&s, &row, Phase::Completion))
            .await
            .unwrap()
            .is_some()
    );
    // A repeat capture finds nothing due and appends nothing.
    Arc::clone(&turns).capture_due().await;
    assert_eq!(finalized(&s).await.len(), 1);
}

#[tokio::test]
async fn a_later_dispatch_captures_the_earlier_completion_before_its_own_writes() {
    let det = Det::new();
    let (_tmp, dir) = repo();
    let (s, turns) = setup(&det, &dir).await;
    dispatch(&turns, "in-1").await;
    turns.observe(&[started("turn-1")]).await;
    std::fs::write(dir.join("a.txt"), "T1\n").unwrap();
    assert!(turns.observe(&[completed("turn-1")]).await);
    // The capture task has not run yet: the next prompt comes first. Its
    // fence runs the due capture before the T2 start capture and write.
    dispatch(&turns, "in-2").await;
    turns.observe(&[started("turn-2")]).await;
    std::fs::write(dir.join("b.txt"), "T2\n").unwrap();
    assert!(turns.observe(&[completed("turn-2")]).await);
    Arc::clone(&turns).capture_due().await;
    let done = finalized(&s).await;
    assert_eq!(done.len(), 2);
    let one = |files| {
        Some(DiffSummary {
            files_changed: files,
            insertions: 1,
            deletions: 1,
        })
    };
    // T1 sees only its own write; T2 sees only its own.
    assert_eq!(done[0].0.as_str(), "turn-1");
    assert_eq!(done[0].3, one(1));
    assert_eq!(done[1].0.as_str(), "turn-2");
    assert_eq!(done[1].3, one(1));
    assert!(is_available(&done[0].2) && is_available(&done[1].1));
}

#[tokio::test]
async fn background_writers_after_the_parent_turn_make_the_end_unavailable() {
    let det = Det::new();
    let (_tmp, dir) = repo();
    let (s, turns) = setup(&det, &dir).await;
    dispatch(&turns, "in-1").await;
    turns.observe(&[started("turn-1")]).await;
    assert!(turns.observe(&[completed("turn-1")]).await);
    // A background writer appears before the capture ran.
    assert!(!turns.observe(&[background(1)]).await);
    std::fs::write(dir.join("a.txt"), "background\n").unwrap();
    Arc::clone(&turns).capture_due().await;
    let done = finalized(&s).await;
    assert_eq!(done.len(), 1);
    assert!(is_available(&done[0].1));
    assert_eq!(done[0].2, unavailable("background_writers"));
    assert_eq!(done[0].3, None);
    let row = row_of_input(&s, "in-1").await;
    let w = Worktree::open(&dir, GitOptions::isolated()).await.unwrap();
    assert!(
        w.read(&key(&s, &row, Phase::Completion))
            .await
            .unwrap()
            .is_none()
    );
    // While the writer runs, a new input gets no start checkpoint.
    dispatch(&turns, "in-2").await;
    assert_eq!(
        row_of_input(&s, "in-2").await.start,
        crate::store::RowSide::Unavailable("background_writers".into())
    );
}

#[tokio::test]
async fn a_native_queued_turn_gives_explicit_unavailable_sides() {
    let det = Det::new();
    let (_tmp, dir) = repo();
    let (s, turns) = setup(&det, &dir).await;
    dispatch(&turns, "in-1").await;
    turns.observe(&[started("turn-1")]).await;
    // A follow-up while T1 runs: the provider queues it.
    dispatch(&turns, "in-2").await;
    turns
        .observe(&[input("in-2", None, InputState::Received)])
        .await;
    assert!(!turns.observe(&[completed("turn-1")]).await);
    turns.observe(&[started("turn-2")]).await;
    assert!(turns.observe(&[completed("turn-2")]).await);
    Arc::clone(&turns).capture_due().await;
    let done = finalized(&s).await;
    assert_eq!(done.len(), 2);
    assert!(is_available(&done[0].1));
    assert_eq!(done[0].2, unavailable("queued_input"));
    assert_eq!(done[1].1, unavailable("turn_running"));
    assert!(is_available(&done[1].2));
    assert_eq!(done[1].3, None);
}

#[tokio::test]
async fn process_end_finalizes_the_running_turn_and_abandons_waiting_inputs() {
    let det = Det::new();
    let (_tmp, dir) = repo();
    let (s, turns) = setup(&det, &dir).await;
    dispatch(&turns, "in-1").await;
    turns.observe(&[started("turn-1")]).await;
    dispatch(&turns, "in-2").await;
    assert!(!turns.observe(&[ended()]).await);
    let done = finalized(&s).await;
    assert_eq!(done.len(), 1);
    assert_eq!(done[0].2, unavailable("session_ended"));
    let waiting = row_of_input(&s, "in-2").await;
    assert_eq!(waiting.state, RowState::Abandoned);
    assert_eq!(waiting.turn_id, None);
}

#[tokio::test]
async fn an_input_without_a_turn_abandons_its_row_without_an_event() {
    let det = Det::new();
    let (_tmp, dir) = repo();
    let (s, turns) = setup(&det, &dir).await;
    // Refused before any I/O.
    let fence = turns.fence().await;
    turns.begin_input(&fence, &InputId::from("in-1")).await;
    turns.input_not_sent(&InputId::from("in-1")).await;
    drop(fence);
    assert_eq!(row_of_input(&s, "in-1").await.state, RowState::Abandoned);
    // Discarded by the provider.
    dispatch(&turns, "in-2").await;
    turns
        .observe(&[input("in-2", None, InputState::Discarded)])
        .await;
    assert_eq!(row_of_input(&s, "in-2").await.state, RowState::Abandoned);
    assert!(finalized(&s).await.is_empty());
    // The next prompt can capture again: nothing waits.
    dispatch(&turns, "in-3").await;
    assert!(matches!(
        row_of_input(&s, "in-3").await.start,
        crate::store::RowSide::Available(_)
    ));
}

#[tokio::test]
async fn a_provider_initiated_turn_gets_its_own_row() {
    let det = Det::new();
    let (_tmp, dir) = repo();
    let (s, turns) = setup(&det, &dir).await;
    turns.observe(&[started("turn-9")]).await;
    assert!(turns.observe(&[completed("turn-9")]).await);
    Arc::clone(&turns).capture_due().await;
    let done = finalized(&s).await;
    assert_eq!(done.len(), 1);
    assert_eq!(done[0].1, unavailable("provider_initiated"));
    assert!(is_available(&done[0].2));
    let all = rows(&s).await;
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].input_id, None);
    assert_eq!(all[0].turn_id.as_ref().map(TurnId::as_str), Some("turn-9"));
}

#[tokio::test]
async fn a_directory_outside_git_finalizes_every_turn_as_not_a_git_worktree() {
    let det = Det::new();
    let tmp = tempfile::tempdir().unwrap();
    let (s, turns) = setup(&det, tmp.path()).await;
    assert!(!turns.has_worktree());
    dispatch(&turns, "in-1").await;
    turns.observe(&[started("turn-1")]).await;
    assert!(turns.observe(&[completed("turn-1")]).await);
    Arc::clone(&turns).capture_due().await;
    let done = finalized(&s).await;
    assert_eq!(done.len(), 1);
    assert_eq!(done[0].1, unavailable("not_a_git_worktree"));
    assert_eq!(done[0].2, unavailable("not_a_git_worktree"));
    let row = row_of_input(&s, "in-1").await;
    assert_eq!(row.worktree_id, None);
}
