//! The `shims` table rules (ADR-005 §3, §4.1, §4.3; slice 4, DL-104).

use std::path::PathBuf;

use fleetdeck_proto::event::{Event, ProviderKind};
use fleetdeck_testkit::Det;

use super::*;
use crate::store::shim_driver::FrameState;
use crate::store::{NewSession, OfflineApprovalPolicy, ReadResult, StoreConfig};

async fn store(det: &Det) -> Store {
    let s = Store::open_in_memory(det.clock(), det.ids(), StoreConfig::default())
        .await
        .unwrap();
    s.recover(Vec::new()).await.unwrap();
    s.create_session(NewSession {
        session_id: "s1".into(),
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
    s
}

fn shim(id: &str) -> NewShim {
    NewShim {
        shim_id: id.to_owned(),
        session_id: "s1".into(),
        socket_path: PathBuf::from(format!("/run/x/{id}.sock")),
    }
}

fn identity(pid: u32) -> ProviderIdentity {
    ProviderIdentity {
        pid,
        start_time: 77,
        boot_id: "boot-a".to_owned(),
    }
}

fn mode(m: &str) -> NewEvent {
    Event::ModeChanged { mode: m.to_owned() }.into()
}

async fn mode_events(s: &Store) -> usize {
    let ReadResult::Events { events, .. } = s.read_after(0, 1000).await.unwrap() else {
        panic!("no events");
    };
    events
        .iter()
        .filter(|e| {
            matches!(
                e.event,
                fleetdeck_proto::EventBody::Known(Event::ModeChanged { .. })
            )
        })
        .count()
}

#[tokio::test]
async fn a_starting_row_has_generation_zero_and_needs_a_known_session() {
    let det = Det::new();
    let s = store(&det).await;
    s.shim_starting(shim("a")).await.unwrap();
    let row = s.shim("a".to_owned()).await.unwrap().unwrap();
    assert_eq!(row.state, ShimState::Starting);
    assert_eq!((row.generation, row.cursor, row.next_write_seq), (0, 0, 1));
    assert_eq!(row.provider_identity, None);
    assert_eq!(row.incarnation, s.incarnation().as_str());
    let mut other = shim("b");
    other.session_id = "nope".into();
    assert!(matches!(
        s.shim_starting(other).await,
        Err(StoreError::UnknownSession(_))
    ));
    assert!(s.shim_starting(shim("a")).await.is_err(), "duplicate id");
}

#[tokio::test]
async fn the_provider_identity_commits_once_and_never_changes() {
    let det = Det::new();
    let s = store(&det).await;
    s.shim_starting(shim("a")).await.unwrap();
    assert!(s.shim_running("a".to_owned(), identity(10)).await.unwrap());
    assert!(s.shim_running("a".to_owned(), identity(10)).await.unwrap());
    assert!(!s.shim_running("a".to_owned(), identity(11)).await.unwrap());
    let row = s.shim("a".to_owned()).await.unwrap().unwrap();
    assert_eq!(row.state, ShimState::Running);
    assert_eq!(row.provider_identity, Some(identity(10)));
    assert!(!s.shim_running("zz".to_owned(), identity(10)).await.unwrap());
}

#[tokio::test]
async fn generations_rise_on_every_commit_and_stop_after_the_row_ends() {
    let det = Det::new();
    let s = store(&det).await;
    s.shim_starting(shim("a")).await.unwrap();
    // A crash after a commit and before the attach only skips a number.
    for want in 1..=3 {
        let got = s.shim_next_generation("a".to_owned()).await.unwrap();
        assert_eq!(got, Some(want));
    }
    assert!(
        s.shim_end(
            "a".to_owned(),
            ShimState::Lost,
            "execution_unknown".to_owned()
        )
        .await
        .unwrap()
    );
    assert_eq!(s.shim_next_generation("a".to_owned()).await.unwrap(), None);
    assert!(
        !s.shim_end("a".to_owned(), ShimState::Exited, "exited".to_owned())
            .await
            .unwrap(),
        "an ended row never ends again"
    );
    let row = s.shim("a".to_owned()).await.unwrap().unwrap();
    assert_eq!(
        (row.state, row.reason.as_deref()),
        (ShimState::Lost, Some("execution_unknown"))
    );
    assert!(
        s.shim_end("a".to_owned(), ShimState::Running, String::new())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn output_commits_events_and_cursor_together_only_for_the_current_generation() {
    let det = Det::new();
    let s = store(&det).await;
    s.shim_starting(shim("a")).await.unwrap();
    s.shim_running("a".to_owned(), identity(10)).await.unwrap();
    let g1 = s
        .shim_next_generation("a".to_owned())
        .await
        .unwrap()
        .unwrap();
    let OutputCommit::Committed(events) = s
        .shim_commit_output("a".to_owned(), g1, 40, vec![mode("plan")])
        .await
        .unwrap()
    else {
        panic!("not committed");
    };
    assert_eq!(events.len(), 1);
    let g2 = s
        .shim_next_generation("a".to_owned())
        .await
        .unwrap()
        .unwrap();
    assert!(g2 > g1);
    // The old attachment commits nothing: no event and no cursor.
    assert_eq!(
        s.shim_commit_output("a".to_owned(), g1, 90, vec![mode("stale")])
            .await
            .unwrap(),
        OutputCommit::StaleGeneration
    );
    assert_eq!(
        s.shim_commit_output("a".to_owned(), g2, 30, vec![mode("back")])
            .await
            .unwrap(),
        OutputCommit::CursorBehind { committed: 40 }
    );
    assert_eq!(mode_events(&s).await, 1);
    assert_eq!(s.shim("a".to_owned()).await.unwrap().unwrap().cursor, 40);
    // The same cursor again with no events is a no-op commit.
    assert!(matches!(
        s.shim_commit_output("a".to_owned(), g2, 40, Vec::new())
            .await
            .unwrap(),
        OutputCommit::Committed(e) if e.is_empty()
    ));
    s.shim_end("a".to_owned(), ShimState::Exited, "exited".to_owned())
        .await
        .unwrap();
    assert_eq!(
        s.shim_commit_output("a".to_owned(), g2, 50, vec![mode("late")])
            .await
            .unwrap(),
        OutputCommit::StaleGeneration
    );
    assert_eq!(mode_events(&s).await, 1);
}

#[tokio::test]
async fn write_sequence_numbers_are_unique_rising_and_refused_to_a_stale_generation() {
    let det = Det::new();
    let s = store(&det).await;
    s.shim_starting(shim("a")).await.unwrap();
    s.shim_running("a".to_owned(), identity(10)).await.unwrap();
    let g1 = s
        .shim_next_generation("a".to_owned())
        .await
        .unwrap()
        .unwrap();
    let a = s.shim_claim_write_seq("a".to_owned(), g1).await.unwrap();
    let b = s.shim_claim_write_seq("a".to_owned(), g1).await.unwrap();
    assert_eq!((a, b), (Some(1), Some(2)));
    let g2 = s
        .shim_next_generation("a".to_owned())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        s.shim_claim_write_seq("a".to_owned(), g1).await.unwrap(),
        None
    );
    // A new generation continues the sequence: a number is never reused.
    assert_eq!(
        s.shim_claim_write_seq("a".to_owned(), g2).await.unwrap(),
        Some(3)
    );
    let row = s.shim("a".to_owned()).await.unwrap().unwrap();
    assert_eq!(row.record().next_write_seq, 4);
    assert_eq!(row.record().generation, g2);
}

#[tokio::test]
async fn recovery_lists_open_rows_of_this_incarnation_starting_first() {
    let det = Det::new();
    let s = store(&det).await;
    s.shim_starting(shim("run")).await.unwrap();
    s.shim_running("run".to_owned(), identity(10))
        .await
        .unwrap();
    s.shim_starting(shim("start")).await.unwrap();
    s.shim_starting(shim("done")).await.unwrap();
    s.shim_end("done".to_owned(), ShimState::NeverStarted, "x".to_owned())
        .await
        .unwrap();
    let ids: Vec<String> = s
        .shims_to_recover()
        .await
        .unwrap()
        .into_iter()
        .map(|r| r.shim_id)
        .collect();
    assert_eq!(ids, ["start", "run"]);
}

/// DL-111: a delayed owner whose generation was taken over, or whose claim
/// was settled and claimed again, cannot move a frame; a completed frame
/// never returns to `pending`.
#[tokio::test]
async fn frame_state_changes_are_fenced_by_the_owner_generation_and_the_current_claim() {
    let det = Det::new();
    let s = store(&det).await;
    s.shim_starting(shim("a")).await.unwrap();
    assert!(s.shim_running("a".into(), identity(9)).await.unwrap());
    assert_eq!(s.shim_next_generation("a".into()).await.unwrap(), Some(1));
    let meta = OutputMeta {
        frames: vec!["{\"method\":\"fd/marker\"}".to_owned()],
        ..OutputMeta::default()
    };
    assert!(matches!(
        s.shim_commit_output_with("a".into(), 1, 0, Vec::new(), meta)
            .await
            .unwrap(),
        OutputCommit::Committed(_)
    ));
    let seq = s.shim_open_frames("a".into()).await.unwrap()[0].frame_seq;
    let old = s
        .shim_claim_frame("a".into(), 1, seq)
        .await
        .unwrap()
        .unwrap();
    // A takeover; the old owner is delayed.
    assert_eq!(s.shim_next_generation("a".into()).await.unwrap(), Some(2));
    for state in [FrameState::Done, FrameState::Pending] {
        assert!(
            !s.shim_frame_state("a".into(), 1, seq, old.clone(), state)
                .await
                .unwrap()
        );
    }
    let open = s.shim_open_frames("a".into()).await.unwrap();
    assert_eq!(open[0].state, FrameState::Claimed);
    // The new owner proves the old claim unwritten, claims again, and the
    // frame is written.
    assert!(
        s.shim_frame_state("a".into(), 2, seq, old.clone(), FrameState::Pending)
            .await
            .unwrap()
    );
    let new = s
        .shim_claim_frame("a".into(), 2, seq)
        .await
        .unwrap()
        .unwrap();
    assert_ne!(new, old);
    assert!(
        !s.shim_frame_state("a".into(), 2, seq, old.clone(), FrameState::Done)
            .await
            .unwrap(),
        "a superseded claim of the same owner changes nothing"
    );
    assert!(
        s.shim_frame_state("a".into(), 2, seq, new.clone(), FrameState::Done)
            .await
            .unwrap()
    );
    // The delayed old owner reports its stale claim unwritten: refused.
    for generation in [1, 2] {
        assert!(
            !s.shim_frame_state(
                "a".into(),
                generation,
                seq,
                old.clone(),
                FrameState::Pending
            )
            .await
            .unwrap()
        );
    }
    assert!(
        !s.shim_frame_state("a".into(), 2, seq, new.clone(), FrameState::Pending)
            .await
            .unwrap(),
        "a done frame never returns to pending"
    );
    assert!(s.shim_open_frames("a".into()).await.unwrap().is_empty());
    assert!(
        s.shim_claim_frame("a".into(), 2, seq)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        s.shim_frame_state("a".into(), 2, seq, new, FrameState::Claimed)
            .await
            .is_err()
    );
    // After the row ends nothing moves.
    let seq2 = {
        let meta = OutputMeta {
            frames: vec!["{\"method\":\"fd/two\"}".to_owned()],
            ..OutputMeta::default()
        };
        s.shim_commit_output_with("a".into(), 2, 0, Vec::new(), meta)
            .await
            .unwrap();
        s.shim_open_frames("a".into()).await.unwrap()[0].frame_seq
    };
    let tag = s
        .shim_claim_frame("a".into(), 2, seq2)
        .await
        .unwrap()
        .unwrap();
    assert!(
        s.shim_end("a".into(), ShimState::Lost, "x".into())
            .await
            .unwrap()
    );
    assert!(
        !s.shim_frame_state("a".into(), 2, seq2, tag, FrameState::Pending)
            .await
            .unwrap()
    );
}

// Review of DL-122: after an adapter state gap the store refuses every
// claim that could send bytes from the invalidated state, whatever the
// caller's memory says.
#[tokio::test]
async fn after_an_adapter_state_gap_no_frame_transition_or_stdin_claim_is_accepted() {
    let det = Det::new();
    let s = store(&det).await;
    s.shim_starting(shim("a")).await.unwrap();
    assert!(s.shim_running("a".into(), identity(9)).await.unwrap());
    assert_eq!(s.shim_next_generation("a".into()).await.unwrap(), Some(1));
    let meta = OutputMeta {
        frames: vec![
            "{\"method\":\"fd/one\"}".to_owned(),
            "{\"method\":\"fd/two\"}".to_owned(),
        ],
        ..OutputMeta::default()
    };
    s.shim_commit_output_with("a".into(), 1, 0, Vec::new(), meta)
        .await
        .unwrap();
    let frames = s.shim_open_frames("a".into()).await.unwrap();
    // Before the gap a claim is accepted.
    assert!(
        s.shim_claim_frame("a".into(), 1, frames[0].frame_seq)
            .await
            .unwrap()
            .is_some()
    );
    assert!(!s.shim_has_adapter_gap("a".into()).await.unwrap());

    assert!(s.shim_adapter_gap("a".into(), 1, 0, 10).await.unwrap());
    assert!(s.shim_has_adapter_gap("a".into()).await.unwrap());
    assert_eq!(
        s.shim_claim_frame("a".into(), 1, frames[1].frame_seq)
            .await
            .unwrap(),
        None,
        "a generated frame"
    );
    assert_eq!(
        s.shim_claim_write("a".into(), 1, None, 0, 1).await.unwrap(),
        None,
        "a stdin frame"
    );
    assert_eq!(
        s.shim_begin_transition("a".into(), 1, "e-1".into(), 1, Vec::new())
            .await
            .unwrap(),
        None,
        "a command transition"
    );
    // A stale generation cannot commit a second gap.
    assert!(!s.shim_adapter_gap("a".into(), 7, 0, 10).await.unwrap());
}

#[test]
fn v14_preserves_prior_adapter_invalidation_without_rewriting_events_or_blocks() {
    let mut conn = rusqlite::Connection::open_in_memory().unwrap();
    let migrations = crate::store::schema::migrations();
    migrations.to_version(&mut conn, 13).unwrap();
    for (id, block) in [
        ("marker", Some("adapter_state_gap")),
        ("hidden", Some("stdin_partial_write")),
        ("partial", Some("stdin_partial_write")),
        ("clean", None),
    ] {
        conn.execute(
            "INSERT INTO sessions (session_id, provider, provider_instance, cwd, state,
                 offline_approval_policy, summary, created_at, updated_at)
             VALUES (?1, 'claude', 'claude', '/w', 'running', 'wait', '{}', 1, 1)",
            [id],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO shims (shim_id, session_id, incarnation, state, socket_path,
                 input_blocked, adapter_checkpoint, created_at, updated_at)
             VALUES (?1, ?1, 'station', 'starting', '/socket', ?2, 'old checkpoint', 1, 1)",
            rusqlite::params![id, block],
        )
        .unwrap();
    }
    let payload = serde_json::to_string(&Event::Error {
        code: crate::store::shim_gaps::REASON_ADAPTER_STATE_GAP.to_owned(),
        message: "output gap".to_owned(),
        retry_at: None,
    })
    .unwrap();
    conn.execute(
        "INSERT INTO events (station_seq, session_id, session_seq, kind, payload, at)
         VALUES (1, 'hidden', 1, 'error', ?1, 2)",
        [&payload],
    )
    .unwrap();
    migrations.to_latest(&mut conn).unwrap();
    migrations.to_latest(&mut conn).unwrap();
    for (id, invalid, block) in [
        ("marker", true, Some("adapter_state_gap")),
        ("hidden", true, Some("stdin_partial_write")),
        ("partial", false, Some("stdin_partial_write")),
        ("clean", false, None),
    ] {
        let actual: (bool, Option<String>, Option<String>) = conn
            .query_row(
                "SELECT adapter_gap, adapter_checkpoint, input_blocked FROM shims WHERE shim_id = ?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(actual.0, invalid, "{id}");
        assert_eq!(
            actual.1.as_deref(),
            (!invalid).then_some("old checkpoint"),
            "{id}"
        );
        assert_eq!(actual.2.as_deref(), block, "{id}");
    }
    let stored: String = conn
        .query_row(
            "SELECT payload FROM events WHERE station_seq = 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(stored, payload);
}

// DL-124: the gap flag is independent of an earlier input block; the
// fences and the checkpoint guard still hold when `input_blocked` already
// had another reason.
#[tokio::test]
async fn an_adapter_state_gap_after_an_earlier_input_block_still_fences_every_claim() {
    let det = Det::new();
    let s = store(&det).await;
    s.shim_starting(shim("a")).await.unwrap();
    assert!(s.shim_running("a".into(), identity(9)).await.unwrap());
    assert_eq!(s.shim_next_generation("a".into()).await.unwrap(), Some(1));
    s.shim_block_input("a".into(), crate::store::shim_writes::REASON_STDIN_PARTIAL)
        .await
        .unwrap();
    assert!(!s.shim_has_adapter_gap("a".into()).await.unwrap());

    assert!(s.shim_adapter_gap("a".into(), 1, 0, 10).await.unwrap());
    assert!(s.shim_has_adapter_gap("a".into()).await.unwrap());
    // The earlier reason is kept for the owner.
    assert_eq!(
        s.shim_input_blocked("s1".into()).await.unwrap().as_deref(),
        Some(crate::store::shim_writes::REASON_STDIN_PARTIAL)
    );
    let meta = OutputMeta {
        frames: vec!["{\"method\":\"fd/one\"}".to_owned()],
        checkpoint: Some(fleetdeck_providers::checkpoint::AdapterCheckpoint {
            provider: ProviderKind::Codex,
            version: 1,
            state: serde_json::json!({}),
        }),
        ..OutputMeta::default()
    };
    s.shim_commit_output_with("a".into(), 1, 10, Vec::new(), meta)
        .await
        .unwrap();
    assert!(
        s.shim_checkpoint("a".into()).await.unwrap().is_none(),
        "no checkpoint after a gap"
    );
    let frame = s.shim_open_frames("a".into()).await.unwrap()[0].frame_seq;
    assert_eq!(
        s.shim_claim_frame("a".into(), 1, frame).await.unwrap(),
        None
    );
    assert_eq!(
        s.shim_claim_write("a".into(), 1, None, 0, 1).await.unwrap(),
        None
    );
    assert_eq!(
        s.shim_begin_transition("a".into(), 1, "e-1".into(), 1, Vec::new())
            .await
            .unwrap(),
        None
    );
}
