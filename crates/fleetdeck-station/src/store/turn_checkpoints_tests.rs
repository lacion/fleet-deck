//! Durable turn checkpoint rows (PLAN §6.6, L13; DL-082).

use fleetdeck_proto::event::{CheckpointSide, DiffSummary, ProviderKind};
use fleetdeck_testkit::Det;

use super::*;

const S1: &str = "s1";

async fn store(det: &Det) -> Store {
    let s = Store::open_in_memory(det.clock(), det.ids(), StoreConfig::default())
        .await
        .unwrap();
    s.recover(Vec::new()).await.unwrap();
    s.create_session(NewSession {
        session_id: S1.into(),
        provider: ProviderKind::Claude,
        provider_instance: "claude".into(),
        cwd: "/w".to_owned(),
        worktree: None,
        branch: None,
        mode: Some("default".to_owned()),
        title: None,
        offline_approval_policy: OfflineApprovalPolicy::Wait,
    })
    .await
    .unwrap();
    s
}

fn planned(turn: &str, input: Option<&str>) -> NewTurnCheckpoint {
    NewTurnCheckpoint {
        app_turn: turn.into(),
        session_id: S1.into(),
        process: Some("proc-1".to_owned().into()),
        input_id: input.map(Into::into),
        turn_id: None,
        worktree_id: Some("wt1".to_owned()),
        worktree_root: Some("/w".to_owned()),
        scope: "tracked".to_owned(),
        start: RowSide::Capturing,
    }
}

fn available(c: &str) -> RowSide {
    RowSide::Available(c.to_owned())
}

fn unavailable(r: &str) -> RowSide {
    RowSide::Unavailable(r.to_owned())
}

/// The events of `s1` of one kind.
async fn events_of(s: &Store, kind: &str) -> Vec<Event> {
    let ReadResult::Events { events, .. } = s.read_after(0, 1000).await.unwrap() else {
        panic!("snapshot required");
    };
    events
        .into_iter()
        .filter_map(|e| match e.event {
            EventBody::Known(ev) if ev.kind_name() == kind => Some(ev),
            _ => None,
        })
        .collect()
}

const SUMMARY: DiffSummary = DiffSummary {
    files_changed: 2,
    insertions: 5,
    deletions: 1,
};

/// A planned row for `input` with both sides available and turn `turn-1`
/// bound.
async fn ready(s: &Store, turn: &str, input: &str) -> AppTurnId {
    let t = s
        .plan_turn_checkpoint(planned(turn, Some(input)))
        .await
        .unwrap();
    assert!(
        s.record_checkpoint_side(t.clone(), Which::Start, available("c0"))
            .await
            .unwrap()
    );
    assert!(
        s.bind_checkpoint_turn(t.clone(), "turn-1".into())
            .await
            .unwrap()
    );
    assert!(
        s.record_checkpoint_side(t.clone(), Which::End, RowSide::Capturing)
            .await
            .unwrap()
    );
    assert!(
        s.record_checkpoint_side(t.clone(), Which::End, available("c1"))
            .await
            .unwrap()
    );
    t
}

#[tokio::test]
async fn turn_finalized_is_committed_once_with_both_sides_and_the_diff_summary() {
    let det = Det::new();
    let s = store(&det).await;
    let t = ready(&s, "T1", "in-1").await;
    let first = s.finalize_turn(t.clone(), Some(SUMMARY)).await.unwrap();
    assert!(first.appended);
    let again = s.finalize_turn(t.clone(), Some(SUMMARY)).await.unwrap();
    assert_eq!(
        again,
        Finalized {
            station_seq: first.station_seq,
            appended: false
        }
    );
    let events = events_of(&s, "turn.finalized").await;
    assert_eq!(
        events,
        vec![Event::TurnFinalized {
            turn_id: "turn-1".into(),
            checkpoint: Some(fleetdeck_proto::event::TurnCheckpoint {
                worktree_id: Some("wt1".to_owned()),
                scope: "tracked".to_owned(),
                start: CheckpointSide::Available {
                    commit: "c0".to_owned()
                },
                end: CheckpointSide::Available {
                    commit: "c1".to_owned()
                },
            }),
            diff_summary: Some(SUMMARY),
        }]
    );
    let row = s.turn_checkpoint(t).await.unwrap().unwrap();
    assert_eq!(row.state, RowState::Finalized);
    assert_eq!(row.finalized_seq, Some(first.station_seq));
}

#[tokio::test]
async fn turn_finalized_waits_for_both_sides_and_the_provider_turn() {
    let det = Det::new();
    let s = store(&det).await;
    let t = s
        .plan_turn_checkpoint(planned("T1", Some("in-1")))
        .await
        .unwrap();
    // Start still capturing: its ref may not exist yet.
    assert!(s.finalize_turn(t.clone(), None).await.is_err());
    s.record_checkpoint_side(t.clone(), Which::Start, available("c0"))
        .await
        .unwrap();
    s.record_checkpoint_side(t.clone(), Which::End, unavailable("queued_input"))
        .await
        .unwrap();
    // No provider turn bound yet.
    assert!(s.finalize_turn(t.clone(), None).await.is_err());
    s.bind_checkpoint_turn(t.clone(), "turn-1".into())
        .await
        .unwrap();
    let done = s.finalize_turn(t, Some(SUMMARY)).await.unwrap();
    assert!(done.appended);
    let events = events_of(&s, "turn.finalized").await;
    let [
        Event::TurnFinalized {
            diff_summary,
            checkpoint: Some(c),
            ..
        },
    ] = events.as_slice()
    else {
        panic!("one turn.finalized: {events:?}");
    };
    // No diff without two checkpoints.
    assert_eq!(*diff_summary, None);
    assert_eq!(
        c.end,
        CheckpointSide::Unavailable {
            reason: "queued_input".to_owned()
        }
    );
}

#[tokio::test]
async fn a_decided_side_never_changes_and_a_late_capture_result_is_dropped() {
    let det = Det::new();
    let s = store(&det).await;
    let t = s
        .plan_turn_checkpoint(planned("T1", Some("in-1")))
        .await
        .unwrap();
    // An invalidation decides the start first …
    assert!(
        s.record_checkpoint_side(t.clone(), Which::Start, unavailable("provider_initiated"))
            .await
            .unwrap()
    );
    // … and the capture result that arrives later is dropped.
    assert!(
        !s.record_checkpoint_side(t.clone(), Which::Start, available("c0"))
            .await
            .unwrap()
    );
    assert!(
        !s.record_checkpoint_side(t.clone(), Which::Start, RowSide::Capturing)
            .await
            .unwrap()
    );
    let row = s.turn_checkpoint(t.clone()).await.unwrap().unwrap();
    assert_eq!(row.start, unavailable("provider_initiated"));
    assert_eq!(row.end, RowSide::Open);
    // The same for the end side.
    s.record_checkpoint_side(t.clone(), Which::End, unavailable("background_writers"))
        .await
        .unwrap();
    assert!(
        !s.record_checkpoint_side(t.clone(), Which::End, available("c1"))
            .await
            .unwrap()
    );
    // A turn binds once.
    assert!(
        s.bind_checkpoint_turn(t.clone(), "turn-1".into())
            .await
            .unwrap()
    );
    assert!(
        !s.bind_checkpoint_turn(t.clone(), "turn-2".into())
            .await
            .unwrap()
    );
    let row = s.turn_checkpoint(t).await.unwrap().unwrap();
    assert_eq!(row.turn_id, Some("turn-1".into()));
}

#[tokio::test]
async fn a_plan_is_idempotent_per_input_and_a_start_is_never_open() {
    let det = Det::new();
    let s = store(&det).await;
    let t = s
        .plan_turn_checkpoint(planned("T1", Some("in-1")))
        .await
        .unwrap();
    // A repeat for the same input keeps the first app turn.
    let again = s
        .plan_turn_checkpoint(planned("T2", Some("in-1")))
        .await
        .unwrap();
    assert_eq!(again, t);
    assert_eq!(s.turn_checkpoints(S1.into()).await.unwrap().len(), 1);
    let mut open = planned("T3", Some("in-3"));
    open.start = RowSide::Open;
    assert!(s.plan_turn_checkpoint(open).await.is_err());
    // A provider-initiated turn has no input; each one is its own row.
    let mut provider = planned("T4", None);
    provider.turn_id = Some("turn-9".into());
    provider.start = unavailable("provider_initiated");
    s.plan_turn_checkpoint(provider).await.unwrap();
    assert_eq!(s.turn_checkpoints(S1.into()).await.unwrap().len(), 2);
}

#[tokio::test]
async fn a_row_without_a_provider_turn_is_abandoned_without_an_event() {
    let det = Det::new();
    let s = store(&det).await;
    let t = s
        .plan_turn_checkpoint(planned("T1", Some("in-1")))
        .await
        .unwrap();
    s.record_checkpoint_side(t.clone(), Which::Start, available("c0"))
        .await
        .unwrap();
    assert!(
        s.abandon_turn_checkpoint(t.clone(), "input_not_sent".to_owned())
            .await
            .unwrap()
    );
    let row = s.turn_checkpoint(t.clone()).await.unwrap().unwrap();
    assert_eq!(row.state, RowState::Abandoned);
    assert_eq!(row.end, unavailable("input_not_sent"));
    assert!(s.finalize_turn(t.clone(), None).await.is_err());
    assert!(
        !s.record_checkpoint_side(t, Which::End, available("c1"))
            .await
            .unwrap()
    );
    assert!(events_of(&s, "turn.finalized").await.is_empty());
    // A row with a bound turn is never abandoned: it gets turn.finalized.
    let b = ready(&s, "T9", "in-9").await;
    assert!(!s.abandon_turn_checkpoint(b, "x".to_owned()).await.unwrap());
}

#[tokio::test]
async fn unsettled_rows_survive_a_reopen_for_start_recovery() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("station.db");
    let det = Det::new();
    {
        let s = Store::open(&path, det.clock(), det.ids(), StoreConfig::default())
            .await
            .unwrap();
        s.recover(Vec::new()).await.unwrap();
        s.create_session(NewSession {
            session_id: S1.into(),
            provider: ProviderKind::Claude,
            provider_instance: "claude".into(),
            cwd: "/w".to_owned(),
            worktree: None,
            branch: None,
            mode: None,
            title: None,
            offline_approval_policy: OfflineApprovalPolicy::Wait,
        })
        .await
        .unwrap();
        let t = ready(&s, "T1", "in-1").await;
        s.finalize_turn(t, None).await.unwrap();
        s.plan_turn_checkpoint(planned("T2", Some("in-2")))
            .await
            .unwrap();
    }
    let s = Store::open(&path, det.clock(), det.ids(), StoreConfig::default())
        .await
        .unwrap();
    let open = s.unsettled_turn_checkpoints().await.unwrap();
    assert_eq!(open.len(), 1);
    assert_eq!(open[0].app_turn, AppTurnId::from("T2"));
    assert_eq!(open[0].start, RowSide::Capturing);
    assert_eq!(open[0].process, Some("proc-1".to_owned().into()));
}
