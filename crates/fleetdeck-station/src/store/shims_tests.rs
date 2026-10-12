//! The `shims` table rules (ADR-005 §3, §4.1, §4.3; slice 4, DL-104).

use std::path::PathBuf;

use fleetdeck_proto::event::{Event, ProviderKind};
use fleetdeck_testkit::Det;

use super::*;
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
