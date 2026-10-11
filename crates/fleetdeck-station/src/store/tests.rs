use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use fleetdeck_proto::command::{
    CommandOrigin, CommandResult, RETRY_WINDOW_MS, ReceiptState, SessionCommand, UserInput,
};
use fleetdeck_proto::event::{ProviderKind, RunOrigin, SessionState};
use fleetdeck_proto::session::Activity;
use fleetdeck_proto::{CommandId, InputId, Reject, RejectCode};
use fleetdeck_testkit::Det;

use super::*;

const DAY: Duration = Duration::from_secs(24 * 60 * 60);

async fn store(det: &Det) -> Store {
    let s = Store::open_in_memory(det.clock(), det.ids(), StoreConfig::default())
        .await
        .unwrap();
    s.recover(Vec::new()).await.unwrap();
    s
}

fn new_session(id: &str) -> NewSession {
    NewSession {
        session_id: id.into(),
        provider: ProviderKind::Claude,
        provider_instance: "claude".into(),
        cwd: "/w".to_owned(),
        worktree: None,
        branch: None,
        mode: Some("default".to_owned()),
        title: Some("t".to_owned()),
        offline_approval_policy: OfflineApprovalPolicy::Wait,
    }
}

fn run_started(n: u32) -> NewEvent {
    Event::RunStarted {
        run_id: format!("run-{n}").into(),
        input_summary: "hi".to_owned(),
        origin: RunOrigin::Board,
    }
    .into()
}

fn mode(m: &str) -> NewEvent {
    Event::ModeChanged { mode: m.to_owned() }.into()
}

fn seqs(events: &[EventEnvelope]) -> Vec<(u64, u64)> {
    events
        .iter()
        .map(|e| (e.station_seq, e.session_seq))
        .collect()
}

async fn read_all(store: &Store, cursor: u64) -> Vec<EventEnvelope> {
    match store.read_after(cursor, 1000).await.unwrap() {
        ReadResult::Events { events, .. } => events,
        ReadResult::SnapshotRequired { .. } => panic!("snapshot required at {cursor}"),
    }
}

#[test]
fn the_migrations_are_valid() {
    schema::migrations().validate().unwrap();
}

#[tokio::test]
async fn a_new_database_gets_a_new_incarnation_and_keeps_it_on_reopen() {
    let tmp = tempfile::tempdir().unwrap();
    let det = Det::new();
    let path = tmp.path().join("data/station.db");
    let first = Store::open(&path, det.clock(), det.ids(), StoreConfig::default())
        .await
        .unwrap();
    let inc = first.incarnation().clone();
    first.create_session(new_session("s1")).await.unwrap();
    first
        .append("s1".into(), vec![run_started(1)])
        .await
        .unwrap();
    drop(first);
    let again = Store::open(&path, det.clock(), det.ids(), StoreConfig::default())
        .await
        .unwrap();
    assert_eq!(again.incarnation(), &inc);
    assert_eq!(again.head(), 1, "the head survives a restart");
    let events = again.append("s1".into(), vec![mode("plan")]).await.unwrap();
    assert_eq!(seqs(&events), vec![(2, 2)]);
    // A reset database (a new file) is a new incarnation.
    let other = Store::open(
        &tmp.path().join("other/station.db"),
        det.clock(),
        det.ids(),
        StoreConfig::default(),
    )
    .await
    .unwrap();
    assert_ne!(other.incarnation(), &inc);
    assert_eq!(other.head(), 0);
}

#[tokio::test]
async fn station_seq_is_global_and_session_seq_is_per_session() {
    let det = Det::new();
    let s = store(&det).await;
    s.create_session(new_session("a")).await.unwrap();
    s.create_session(new_session("b")).await.unwrap();
    let a = s
        .append("a".into(), vec![run_started(1), mode("plan")])
        .await
        .unwrap();
    let b = s.append("b".into(), vec![run_started(1)]).await.unwrap();
    let a2 = s.append("a".into(), vec![mode("default")]).await.unwrap();
    assert_eq!(seqs(&a), vec![(1, 1), (2, 2)]);
    assert_eq!(seqs(&b), vec![(3, 1)]);
    assert_eq!(seqs(&a2), vec![(4, 3)]);
    let all = read_all(&s, 0).await;
    assert_eq!(all.len(), 4);
    assert_eq!(all[2].session_id.as_str(), "b");
    assert_eq!(all[3], a2[0], "a read returns the stored envelope");
    assert!(matches!(
        s.create_session(new_session("a")).await,
        Err(StoreError::SessionExists(_))
    ));
}

#[tokio::test]
async fn appends_fold_the_projection_in_the_same_transaction() {
    let det = Det::new();
    let s = store(&det).await;
    s.create_session(new_session("s1")).await.unwrap();
    let before = s.summary("s1".into()).await.unwrap().unwrap();
    assert_eq!(
        (before.state, before.activity),
        (SessionState::Starting, Activity::Idle)
    );
    assert_eq!(before.title.as_deref(), Some("t"));
    s.append("s1".into(), vec![run_started(1), mode("plan")])
        .await
        .unwrap();
    let after = s.summary("s1".into()).await.unwrap().unwrap();
    assert_eq!(after.activity, Activity::Working);
    assert_eq!(after.mode.as_deref(), Some("plan"));
    assert_eq!(after.session_seq, 2);
    let snap = s.snapshot().await.unwrap();
    assert_eq!(snap.watermark, 2);
    assert_eq!(snap.incarnation, *s.incarnation());
    assert_eq!(snap.sessions, vec![after]);
}

#[tokio::test]
async fn a_failed_append_stores_nothing_and_does_not_move_the_head() {
    let det = Det::new();
    let s = store(&det).await;
    s.create_session(new_session("s1")).await.unwrap();
    let mut head = s.subscribe();
    s.append("s1".into(), vec![run_started(1)]).await.unwrap();
    assert!(head.has_changed().unwrap());
    assert_eq!(*head.borrow_and_update(), 1);
    let err = s
        .append("nope".into(), vec![mode("plan")])
        .await
        .unwrap_err();
    assert!(matches!(err, StoreError::UnknownSession(_)));
    assert!(!head.has_changed().unwrap());
    assert_eq!(read_all(&s, 0).await.len(), 1);
    // The next event takes the next number: nothing was allocated.
    let next = s.append("s1".into(), vec![mode("plan")]).await.unwrap();
    assert_eq!(seqs(&next), vec![(2, 2)]);
}

#[tokio::test]
async fn reads_page_in_order_and_refuse_a_cursor_ahead_of_the_head() {
    let det = Det::new();
    let s = store(&det).await;
    s.create_session(new_session("s1")).await.unwrap();
    let events: Vec<NewEvent> = (1..=5).map(run_started).collect();
    s.append("s1".into(), events).await.unwrap();
    let ReadResult::Events { events, head } = s.read_after(1, 2).await.unwrap() else {
        panic!("events expected");
    };
    assert_eq!(head, 5);
    assert_eq!(
        events.iter().map(|e| e.station_seq).collect::<Vec<_>>(),
        vec![2, 3]
    );
    assert!(read_all(&s, 5).await.is_empty());
    assert!(matches!(
        s.read_after(6, 10).await,
        Err(StoreError::CursorAhead { cursor: 6, head: 5 })
    ));
}

#[tokio::test]
async fn retention_keeps_unacknowledged_events_and_then_needs_a_snapshot() {
    let det = Det::new();
    let s = store(&det).await;
    s.create_session(new_session("s1")).await.unwrap();
    s.pair_deck("deck-1".to_owned(), None).await.unwrap();
    s.append("s1".into(), (1..=3).map(run_started).collect())
        .await
        .unwrap();
    det.advance(8 * DAY);
    s.append("s1".into(), vec![mode("plan")]).await.unwrap();
    // Old, but nobody acknowledged them: they stay.
    assert_eq!(s.prune().await.unwrap().events, 0);
    s.ack("deck-1".to_owned(), 2).await.unwrap();
    let report = s.prune().await.unwrap();
    assert_eq!((report.events, report.pruned_through), (2, 2));
    // An ack never moves back; an ack ahead of the head is refused.
    s.ack("deck-1".to_owned(), 1).await.unwrap();
    s.ack("deck-1".to_owned(), 4).await.unwrap();
    assert!(matches!(
        s.ack("deck-1".to_owned(), 5).await,
        Err(StoreError::CursorAhead { .. })
    ));
    assert!(matches!(
        s.ack("deck-x".to_owned(), 1).await,
        Err(StoreError::UnknownDeck(_))
    ));
    // Event 4 is young: only event 3 goes.
    let report = s.prune().await.unwrap();
    assert_eq!((report.events, report.pruned_through), (1, 3));
    assert_eq!(
        s.read_after(1, 10).await.unwrap(),
        ReadResult::SnapshotRequired { head: 4 }
    );
    let rest = read_all(&s, 3).await;
    assert_eq!(
        rest.iter().map(|e| e.station_seq).collect::<Vec<_>>(),
        vec![4]
    );
}

#[tokio::test]
async fn without_a_paired_deck_only_the_window_counts() {
    let det = Det::new();
    let s = store(&det).await;
    s.create_session(new_session("s1")).await.unwrap();
    s.append("s1".into(), vec![run_started(1)]).await.unwrap();
    assert_eq!(s.prune().await.unwrap().events, 0, "inside the window");
    det.advance(8 * DAY);
    assert_eq!(s.prune().await.unwrap().events, 1);
    // A second deck that never acknowledged holds everything back.
    s.append("s1".into(), vec![run_started(2)]).await.unwrap();
    s.pair_deck("a".to_owned(), None).await.unwrap();
    s.pair_deck("b".to_owned(), None).await.unwrap();
    s.ack("a".to_owned(), 2).await.unwrap();
    det.advance(8 * DAY);
    assert_eq!(s.prune().await.unwrap().events, 0);
}

// --- receipts ---------------------------------------------------------------

fn envelope(det: &Det, id: &str, text: &str) -> CommandEnvelope {
    CommandEnvelope {
        command_id: CommandId::from(id),
        target_incarnation: None,
        retry_until: i64::try_from(det.now_ms()).unwrap() + RETRY_WINDOW_MS,
        origin: CommandOrigin::Board,
        command: SessionCommand::Prompt {
            session_id: "s1".into(),
            input: UserInput {
                text: text.to_owned(),
            },
        },
    }
}

/// A decision that appends one event and counts its runs.
fn prompt_decision(
    runs: &Arc<AtomicUsize>,
) -> impl FnOnce(&mut Decision<'_>) -> Result<CommandResult, DecideError> + Send + 'static {
    let runs = Arc::clone(runs);
    move |d| {
        runs.fetch_add(1, Ordering::SeqCst);
        d.append(&"s1".into(), run_started(1))?;
        Ok(CommandResult {
            session_id: Some("s1".into()),
            input_id: Some(InputId::from("in-1")),
        })
    }
}

async fn with_session(det: &Det) -> Store {
    let s = store(det).await;
    s.create_session(new_session("s1")).await.unwrap();
    s
}

#[tokio::test]
async fn an_accepted_command_commits_its_events_and_its_receipt_together() {
    let det = Det::new();
    let s = with_session(&det).await;
    let runs = Arc::new(AtomicUsize::new(0));
    let mut head = s.subscribe();
    let d = s
        .decide(
            envelope(&det, "c1", "hi"),
            Caller::deck("k"),
            prompt_decision(&runs),
        )
        .await
        .unwrap();
    assert!(!d.duplicate);
    assert_eq!(d.receipt.state, ReceiptState::Accepted);
    assert_eq!(d.receipt.station_seq, Some(1));
    assert_eq!(seqs(&d.events), vec![(1, 1)]);
    assert_eq!(*head.borrow_and_update(), 1);
    assert_eq!(
        s.receipt("c1".into()).await.unwrap(),
        Some(d.receipt.clone())
    );
    // A command without events is accepted at the current head.
    let none = s
        .decide(envelope(&det, "c2", "x"), Caller::deck("k"), |_| {
            Ok(CommandResult::default())
        })
        .await
        .unwrap();
    assert_eq!(none.receipt.station_seq, Some(1));
}

#[tokio::test]
async fn a_duplicate_returns_the_stored_receipt_and_does_nothing_else() {
    let det = Det::new();
    let s = with_session(&det).await;
    let runs = Arc::new(AtomicUsize::new(0));
    let first = s
        .decide(
            envelope(&det, "c1", "hi"),
            Caller::deck("k"),
            prompt_decision(&runs),
        )
        .await
        .unwrap();
    det.advance(Duration::from_secs(60));
    let again = s
        .decide(
            envelope(&det, "c1", "hi"),
            Caller::deck("k"),
            prompt_decision(&runs),
        )
        .await
        .unwrap();
    assert!(again.duplicate);
    assert_eq!(
        again.receipt, first.receipt,
        "the original receipt, unchanged"
    );
    assert!(again.events.is_empty());
    assert_eq!(runs.load(Ordering::SeqCst), 1, "the decision ran once");
    assert_eq!(s.head(), 1);
}

#[tokio::test]
async fn a_reused_id_with_another_payload_method_or_caller_is_rejected() {
    let det = Det::new();
    let s = with_session(&det).await;
    let runs = Arc::new(AtomicUsize::new(0));
    let first = s
        .decide(
            envelope(&det, "c1", "hi"),
            Caller::deck("k"),
            prompt_decision(&runs),
        )
        .await
        .unwrap();
    let mut other_method = envelope(&det, "c1", "hi");
    other_method.command = SessionCommand::Interrupt {
        session_id: "s1".into(),
    };
    for (env, caller) in [
        (envelope(&det, "c1", "other text"), Caller::deck("k")),
        (other_method, Caller::deck("k")),
        (envelope(&det, "c1", "hi"), Caller::local()),
        (envelope(&det, "c1", "hi"), Caller::deck("other")),
    ] {
        let d = s.decide(env, caller, prompt_decision(&runs)).await.unwrap();
        assert!(!d.duplicate);
        assert_eq!(d.receipt.state, ReceiptState::Rejected);
        assert_eq!(
            d.receipt.reject.as_ref().map(|r| r.code),
            Some(RejectCode::CommandIdReused)
        );
    }
    assert_eq!(runs.load(Ordering::SeqCst), 1);
    assert_eq!(s.receipt("c1".into()).await.unwrap(), Some(first.receipt));
    assert_eq!(s.head(), 1);
}

#[tokio::test]
async fn a_rejected_decision_rolls_back_its_writes_and_stores_the_rejection() {
    let det = Det::new();
    let s = with_session(&det).await;
    let d = s
        .decide(envelope(&det, "c1", "hi"), Caller::local(), |d| {
            d.append(&"s1".into(), run_started(1))?;
            Err(Reject::new(RejectCode::InvalidState, "the session is busy").into())
        })
        .await
        .unwrap();
    assert_eq!(d.receipt.state, ReceiptState::Rejected);
    assert!(d.events.is_empty());
    assert_eq!(s.head(), 0);
    assert!(
        read_all(&s, 0).await.is_empty(),
        "the event was rolled back"
    );
    let summary = s.summary("s1".into()).await.unwrap().unwrap();
    assert_eq!(summary.session_seq, 0, "the projection was rolled back");
    let again = s
        .decide(envelope(&det, "c1", "hi"), Caller::local(), |_| {
            panic!("a duplicate must not decide again")
        })
        .await
        .unwrap();
    assert!(again.duplicate);
    assert_eq!(again.receipt, d.receipt);
}

#[tokio::test]
async fn a_store_error_in_a_decision_stores_nothing() {
    let det = Det::new();
    let s = with_session(&det).await;
    let err = s
        .decide(envelope(&det, "c1", "hi"), Caller::local(), |d| {
            d.append(&"s1".into(), run_started(1))?;
            d.append(&"missing".into(), run_started(2))?;
            Ok(CommandResult::default())
        })
        .await
        .unwrap_err();
    assert!(matches!(err, StoreError::UnknownSession(_)));
    assert_eq!(s.receipt("c1".into()).await.unwrap(), None);
    assert!(read_all(&s, 0).await.is_empty());
}

#[tokio::test]
async fn admission_rejections_follow_g3() {
    let det = Det::new();
    let s = with_session(&det).await;
    let never = |_: &mut Decision<'_>| -> Result<CommandResult, DecideError> {
        panic!("an inadmissible command is not decided")
    };
    // Another incarnation: rejected, no receipt.
    let mut env = envelope(&det, "c1", "hi");
    env.target_incarnation = Some("other".into());
    let d = s.decide(env, Caller::local(), never).await.unwrap();
    assert_eq!(
        d.receipt.reject.map(|r| r.code),
        Some(RejectCode::IncarnationMismatch)
    );
    assert_eq!(s.receipt("c1".into()).await.unwrap(), None);
    // The right incarnation is admitted.
    let mut env = envelope(&det, "c0", "hi");
    env.target_incarnation = Some(s.incarnation().clone());
    let d = s
        .decide(env, Caller::local(), |_| Ok(CommandResult::default()))
        .await
        .unwrap();
    assert_eq!(d.receipt.state, ReceiptState::Accepted);
    // retry_until passed: rejected, no receipt.
    let mut env = envelope(&det, "c2", "hi");
    env.retry_until -= RETRY_WINDOW_MS + 3 * 60 * 1000;
    let d = s.decide(env, Caller::local(), never).await.unwrap();
    assert_eq!(d.receipt.reject.map(|r| r.code), Some(RejectCode::Expired));
    assert_eq!(s.receipt("c2".into()).await.unwrap(), None);
    // retry_until too far away: rejected with a stored receipt.
    let mut env = envelope(&det, "c3", "hi");
    env.retry_until += RETRY_WINDOW_MS;
    let d = s.decide(env, Caller::local(), never).await.unwrap();
    assert_eq!(
        d.receipt.reject.map(|r| r.code),
        Some(RejectCode::InvalidCommand)
    );
    assert!(s.receipt("c3".into()).await.unwrap().is_some());
}

#[tokio::test]
async fn a_resend_after_receipt_expiry_is_rejected_and_not_executed() {
    let det = Det::new();
    let s = with_session(&det).await;
    let runs = Arc::new(AtomicUsize::new(0));
    let env = envelope(&det, "c1", "hi");
    s.decide(env.clone(), Caller::deck("k"), prompt_decision(&runs))
        .await
        .unwrap();
    det.advance(DAY + Duration::from_secs(1));
    let report = s.prune().await.unwrap();
    assert_eq!(report.receipts, 1);
    assert_eq!(s.receipt("c1".into()).await.unwrap(), None);
    // G3: the receipt is gone, but the envelope's retry_until passed long
    // ago, so the same command cannot run twice.
    let d = s
        .decide(env, Caller::deck("k"), prompt_decision(&runs))
        .await
        .unwrap();
    assert_eq!(d.receipt.reject.map(|r| r.code), Some(RejectCode::Expired));
    assert_eq!(runs.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn receipts_and_events_survive_a_restart() {
    let tmp = tempfile::tempdir().unwrap();
    let det = Det::new();
    let path = tmp.path().join("station.db");
    let runs = Arc::new(AtomicUsize::new(0));
    let first = {
        let s = Store::open(&path, det.clock(), det.ids(), StoreConfig::default())
            .await
            .unwrap();
        s.recover(Vec::new()).await.unwrap();
        s.create_session(new_session("s1")).await.unwrap();
        s.decide(
            envelope(&det, "c1", "hi"),
            Caller::deck("k"),
            prompt_decision(&runs),
        )
        .await
        .unwrap()
    };
    let s = Store::open(&path, det.clock(), det.ids(), StoreConfig::default())
        .await
        .unwrap();
    s.recover(Vec::new()).await.unwrap();
    let again = s
        .decide(
            envelope(&det, "c1", "hi"),
            Caller::deck("k"),
            prompt_decision(&runs),
        )
        .await
        .unwrap();
    assert!(again.duplicate);
    assert_eq!(again.receipt, first.receipt);
    assert_eq!(runs.load(Ordering::SeqCst), 1);
    // Recovery appended the lost-session events after the stored ones.
    assert!(read_all(&s, 0).await.starts_with(&first.events));
}

#[tokio::test]
async fn a_snapshot_keeps_every_running_session_and_the_newest_exited_ones_up_to_the_limit() {
    let det = Det::new();
    let config = StoreConfig {
        snapshot_sessions: 3,
        ..StoreConfig::default()
    };
    let s = Store::open_in_memory(det.clock(), det.ids(), config)
        .await
        .unwrap();
    s.recover(Vec::new()).await.unwrap();
    // live-old is the oldest and still running; e1..e4 exited later.
    s.create_session(new_session("live-old")).await.unwrap();
    for id in ["e1", "e2", "e3", "e4"] {
        det.advance(Duration::from_secs(1));
        s.create_session(new_session(id)).await.unwrap();
        let ended: NewEvent = Event::SessionEnded {
            reason: "done".to_owned(),
            exit_code: Some(0),
        }
        .into();
        s.append(id.into(), vec![ended]).await.unwrap();
    }
    let snap = s.snapshot().await.unwrap();
    let ids: Vec<&str> = snap
        .sessions
        .iter()
        .map(|x| x.session_id.as_str())
        .collect();
    assert_eq!(ids, ["live-old", "e3", "e4"]);
    assert_eq!(
        StoreConfig::default().snapshot_sessions,
        fleetdeck_proto::link::MAX_SNAPSHOT_SESSIONS
    );
}
