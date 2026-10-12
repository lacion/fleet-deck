//! Start recovery of turn checkpoint rows from their refs (DL-082): a crash
//! between the row and the ref, or between the ref and the side record,
//! never loses a published checkpoint, never invents one, and gives each
//! started turn exactly one `turn.finalized`.

use std::path::{Path, PathBuf};
use std::process::Command;

use fleetdeck_proto::event::{CheckpointSide, DiffSummary, ProviderKind};
use fleetdeck_proto::{Event, EventBody};
use fleetdeck_testkit::Det;

use super::*;
use crate::store::{
    AppTurnId, NewSession, NewTurnCheckpoint, OfflineApprovalPolicy, ReadResult, RowSide, RowState,
    Store, StoreConfig, Which,
};

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
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "init"]);
    (tmp, dir)
}

async fn store(det: &Det, cwd: &Path) -> Store {
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
    s
}

fn key(s: &Store, turn: &str, phase: Phase) -> CheckpointKey {
    CheckpointKey {
        incarnation: s.incarnation().as_str().to_owned(),
        session: S1.to_owned(),
        turn: turn.to_owned(),
        phase,
    }
}

fn row(w: &Worktree, turn: &str, input: &str) -> NewTurnCheckpoint {
    NewTurnCheckpoint {
        app_turn: turn.into(),
        session_id: S1.into(),
        process: Some("proc-1".to_owned().into()),
        input_id: Some(input.into()),
        turn_id: None,
        worktree_id: Some(w.id().to_owned()),
        worktree_root: Some(w.root().display().to_string()),
        scope: SCOPE_TRACKED.to_owned(),
        start: RowSide::Capturing,
    }
}

async fn publish(w: &Worktree, k: &CheckpointKey) -> String {
    match w.capture(k, Limits::default()).await.unwrap() {
        Captured::Published(c) | Captured::Existing(c) => c.commit,
        Captured::Unavailable(why) => panic!("unavailable: {why:?}"),
    }
}

async fn finalized(s: &Store) -> Vec<Event> {
    let ReadResult::Events { events, .. } = s.read_after(0, 1000).await.unwrap() else {
        panic!("snapshot required");
    };
    events
        .into_iter()
        .filter_map(|e| match e.event {
            EventBody::Known(ev @ Event::TurnFinalized { .. }) => Some(ev),
            _ => None,
        })
        .collect()
}

fn sides(e: &Event) -> (CheckpointSide, CheckpointSide, Option<DiffSummary>) {
    let Event::TurnFinalized {
        checkpoint: Some(c),
        diff_summary,
        ..
    } = e
    else {
        panic!("{e:?}");
    };
    (c.start.clone(), c.end.clone(), *diff_summary)
}

#[tokio::test]
async fn a_crash_after_both_refs_were_published_keeps_them_and_finalizes_once() {
    let det = Det::new();
    let (_tmp, dir) = repo();
    let w = Worktree::open(&dir, GitOptions::isolated()).await.unwrap();
    let s = store(&det, &dir).await;
    let t = s.plan_turn_checkpoint(row(&w, "T1", "in-1")).await.unwrap();
    let c0 = publish(&w, &key(&s, "T1", Phase::Start)).await;
    s.bind_checkpoint_turn(t.clone(), "turn-1".into())
        .await
        .unwrap();
    std::fs::write(dir.join("a.txt"), "one\ntwo\n").unwrap();
    std::fs::write(dir.join("b.txt"), "new\n").unwrap();
    git(&dir, &["add", "b.txt"]);
    s.record_checkpoint_side(t.clone(), Which::End, RowSide::Capturing)
        .await
        .unwrap();
    let c1 = publish(&w, &key(&s, "T1", Phase::Completion)).await;
    // The crash: neither side result was recorded.
    let report = settle_unsettled(&s, GitOptions::isolated()).await.unwrap();
    assert_eq!(report.finalized, vec![AppTurnId::from("T1")]);
    let events = finalized(&s).await;
    assert_eq!(events.len(), 1);
    let (start, end, diff) = sides(&events[0]);
    assert_eq!(start, CheckpointSide::Available { commit: c0 });
    assert_eq!(end, CheckpointSide::Available { commit: c1 });
    assert_eq!(
        diff,
        Some(DiffSummary {
            files_changed: 2,
            insertions: 2,
            deletions: 0
        })
    );
    // A second recovery finds nothing and appends nothing.
    let again = settle_unsettled(&s, GitOptions::isolated()).await.unwrap();
    assert_eq!(again, SettleReport::default());
    assert_eq!(finalized(&s).await.len(), 1);
}

#[tokio::test]
async fn a_crash_before_a_ref_existed_makes_that_side_unavailable_and_never_captures_later() {
    let det = Det::new();
    let (_tmp, dir) = repo();
    let w = Worktree::open(&dir, GitOptions::isolated()).await.unwrap();
    let s = store(&det, &dir).await;
    let t = s.plan_turn_checkpoint(row(&w, "T1", "in-1")).await.unwrap();
    let c0 = publish(&w, &key(&s, "T1", Phase::Start)).await;
    s.record_checkpoint_side(t.clone(), Which::Start, RowSide::Available(c0.clone()))
        .await
        .unwrap();
    s.bind_checkpoint_turn(t.clone(), "turn-1".into())
        .await
        .unwrap();
    // The turn ran; the end side is still open (no completion yet) when the
    // station dies. A capture now would include later writes.
    std::fs::write(dir.join("a.txt"), "changed after the crash\n").unwrap();
    settle_unsettled(&s, GitOptions::isolated()).await.unwrap();
    let events = finalized(&s).await;
    let (start, end, diff) = sides(&events[0]);
    assert_eq!(start, CheckpointSide::Available { commit: c0 });
    assert_eq!(
        end,
        CheckpointSide::Unavailable {
            reason: "station_restart".to_owned()
        }
    );
    assert_eq!(diff, None);
    assert!(
        w.read(&key(&s, "T1", Phase::Completion))
            .await
            .unwrap()
            .is_none(),
        "recovery never captures"
    );
    // A capturing start without a ref is unavailable too.
    let u = s.plan_turn_checkpoint(row(&w, "T2", "in-2")).await.unwrap();
    s.bind_checkpoint_turn(u.clone(), "turn-2".into())
        .await
        .unwrap();
    settle_unsettled(&s, GitOptions::isolated()).await.unwrap();
    let events = finalized(&s).await;
    let (start, _, _) = sides(&events[1]);
    assert_eq!(
        start,
        CheckpointSide::Unavailable {
            reason: "station_restart".to_owned()
        }
    );
    assert!(
        w.read(&key(&s, "T2", Phase::Start))
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn a_row_without_a_started_turn_is_abandoned_and_a_vanished_worktree_is_unavailable() {
    let det = Det::new();
    let (tmp, dir) = repo();
    let w = Worktree::open(&dir, GitOptions::isolated()).await.unwrap();
    let s = store(&det, &dir).await;
    let t = s.plan_turn_checkpoint(row(&w, "T1", "in-1")).await.unwrap();
    publish(&w, &key(&s, "T1", Phase::Start)).await;
    let u = s.plan_turn_checkpoint(row(&w, "T2", "in-2")).await.unwrap();
    s.bind_checkpoint_turn(u.clone(), "turn-2".into())
        .await
        .unwrap();
    // The worktree is gone after the crash.
    drop(w);
    std::fs::remove_dir_all(tmp.path()).unwrap();
    let report = settle_unsettled(&s, GitOptions::isolated()).await.unwrap();
    assert_eq!(report.abandoned, vec![t.clone()]);
    assert_eq!(report.finalized, vec![u]);
    let abandoned = s.turn_checkpoint(t).await.unwrap().unwrap();
    assert_eq!(abandoned.state, RowState::Abandoned);
    let events = finalized(&s).await;
    assert_eq!(events.len(), 1);
    let (start, end, _) = sides(&events[0]);
    let restart = CheckpointSide::Unavailable {
        reason: "station_restart".to_owned(),
    };
    assert_eq!((start, end), (restart.clone(), restart));
}

fn unavailable(reason: &str) -> CheckpointSide {
    CheckpointSide::Unavailable {
        reason: reason.to_owned(),
    }
}

/// Records both sides `available` and binds the turn, as a live turn does
/// just before a crash that stops it ahead of `turn.finalized`.
async fn recorded_turn(s: &Store, w: &Worktree, dir: &Path, turn: &str) -> (String, String) {
    let t = s
        .plan_turn_checkpoint(row(w, turn, &format!("in-{turn}")))
        .await
        .unwrap();
    let c0 = publish(w, &key(s, turn, Phase::Start)).await;
    s.record_checkpoint_side(t.clone(), Which::Start, RowSide::Available(c0.clone()))
        .await
        .unwrap();
    s.bind_checkpoint_turn(t.clone(), format!("turn-{turn}").into())
        .await
        .unwrap();
    std::fs::write(dir.join("a.txt"), format!("{turn}\n")).unwrap();
    s.record_checkpoint_side(t.clone(), Which::End, RowSide::Capturing)
        .await
        .unwrap();
    let c1 = publish(w, &key(s, turn, Phase::Completion)).await;
    s.record_checkpoint_side(t, Which::End, RowSide::Available(c1.clone()))
        .await
        .unwrap();
    (c0, c1)
}

#[tokio::test]
async fn a_recorded_checkpoint_whose_ref_disappeared_is_finalized_unavailable() {
    let det = Det::new();
    let (_tmp, dir) = repo();
    let w = Worktree::open(&dir, GitOptions::isolated()).await.unwrap();
    let s = store(&det, &dir).await;
    let (_, c1) = recorded_turn(&s, &w, &dir, "T1").await;
    // Someone deletes the start ref before the station finalizes the turn.
    let start_ref = w.ref_name(&key(&s, "T1", Phase::Start)).unwrap();
    git(&dir, &["update-ref", "-d", &start_ref]);
    settle_unsettled(&s, GitOptions::isolated()).await.unwrap();
    let events = finalized(&s).await;
    assert_eq!(events.len(), 1);
    let (start, end, diff) = sides(&events[0]);
    assert_eq!(start, unavailable("checkpoint_missing"));
    assert_eq!(end, CheckpointSide::Available { commit: c1 });
    assert_eq!(diff, None, "no diff without both checkpoints");
}

#[tokio::test]
async fn a_recorded_checkpoint_whose_ref_changed_is_finalized_unavailable() {
    let det = Det::new();
    let (_tmp, dir) = repo();
    let w = Worktree::open(&dir, GitOptions::isolated()).await.unwrap();
    let s = store(&det, &dir).await;
    let (c0, c1) = recorded_turn(&s, &w, &dir, "T1").await;
    // The end ref now names another checkpoint commit of this worktree, and
    // the start ref names an ordinary commit (not a checkpoint).
    std::fs::write(dir.join("a.txt"), "other\n").unwrap();
    let other = publish(&w, &key(&s, "T9", Phase::Start)).await;
    assert_ne!(other, c1);
    let end_ref = w.ref_name(&key(&s, "T1", Phase::Completion)).unwrap();
    git(&dir, &["update-ref", &end_ref, &other]);
    let start_ref = w.ref_name(&key(&s, "T1", Phase::Start)).unwrap();
    git(&dir, &["update-ref", &start_ref, "HEAD"]);
    settle_unsettled(&s, GitOptions::isolated()).await.unwrap();
    let events = finalized(&s).await;
    assert_eq!(events.len(), 1);
    let (start, end, diff) = sides(&events[0]);
    assert_eq!(start, unavailable("checkpoint_changed"));
    assert_eq!(end, unavailable("checkpoint_changed"));
    assert_eq!(diff, None);
    // The recorded commits are not shown anywhere as checkpoints.
    let row = s.turn_checkpoint("T1".into()).await.unwrap().unwrap();
    assert_ne!(row.start, RowSide::Available(c0));
    assert_ne!(row.end, RowSide::Available(c1));
}

#[tokio::test]
async fn an_intact_recorded_checkpoint_is_kept_and_a_vanished_worktree_withdraws_it() {
    let det = Det::new();
    let (tmp, dir) = repo();
    let w = Worktree::open(&dir, GitOptions::isolated()).await.unwrap();
    let s = store(&det, &dir).await;
    let (c0, c1) = recorded_turn(&s, &w, &dir, "T1").await;
    settle_unsettled(&s, GitOptions::isolated()).await.unwrap();
    let (start, end, diff) = sides(&finalized(&s).await[0]);
    assert_eq!(start, CheckpointSide::Available { commit: c0 });
    assert_eq!(end, CheckpointSide::Available { commit: c1 });
    assert!(diff.is_some());
    // A second turn is recorded, then its worktree disappears.
    recorded_turn(&s, &w, &dir, "T2").await;
    drop(w);
    std::fs::remove_dir_all(tmp.path()).unwrap();
    settle_unsettled(&s, GitOptions::isolated()).await.unwrap();
    let events = finalized(&s).await;
    assert_eq!(events.len(), 2);
    let (start, end, _) = sides(&events[1]);
    let missing = unavailable("checkpoint_missing");
    assert_eq!((start, end), (missing.clone(), missing));
}
