//! The cleanup rows of the station store (FG-3, design §6.3, §6.6).

use std::time::Duration;

use fleetdeck_proto::event::ProviderKind;
use fleetdeck_proto::fleet_git::CleanupState;
use fleetdeck_proto::{Event, EventBody, SessionId};
use fleetdeck_testkit::Det;

use super::CleanupRow;
use crate::store::git_state::tests::{new_git, new_session, store};
use crate::store::{NewEvent, ReadResult, Store};

fn ended() -> NewEvent {
    Event::SessionEnded {
        reason: "done".to_owned(),
        exit_code: Some(0),
    }
    .into()
}

fn started() -> NewEvent {
    Event::SessionStarted {
        provider: ProviderKind::Claude,
        provider_instance: "claude".into(),
        provider_version: None,
        model: None,
        cwd: "/w".to_owned(),
        mode: None,
        capabilities: Box::new(fleetdeck_proto::capability::Capabilities::minimal()),
        resumed: None,
        provider_refs: fleetdeck_proto::event::ProviderRefs::new(),
        title: None,
        branch: None,
        repo: None,
    }
    .into()
}

async fn with_session(det: &Det) -> Store {
    let store = store(det).await;
    store.create_session(new_session("s1")).await.unwrap();
    store
        .git_record_session(new_git("s1", "p1"), None)
        .await
        .unwrap();
    store
}

async fn row(store: &Store) -> CleanupRow {
    store.git_cleanup_rows().await.unwrap().remove(0)
}

async fn cleanup_events(store: &Store) -> Vec<(CleanupState, Option<String>)> {
    let ReadResult::Events { events, .. } = store.read_after(0, 1000).await.unwrap() else {
        panic!("snapshot required");
    };
    events
        .into_iter()
        .filter_map(|e| match e.event {
            EventBody::Known(Event::SessionCleanup { state, reason }) => Some((state, reason)),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn ended_at_follows_the_latest_end_and_clears_when_the_session_runs_again() {
    let det = Det::new();
    let store = with_session(&det).await;
    assert_eq!(row(&store).await.ended_at, None);
    det.advance(Duration::from_secs(10));
    let first = i64::try_from(det.now_ms()).unwrap();
    store.append("s1".into(), vec![ended()]).await.unwrap();
    assert_eq!(row(&store).await.ended_at, Some(first));
    // A resume clears it; the next end sets it again (decision 24).
    det.advance(Duration::from_secs(60));
    store.append("s1".into(), vec![started()]).await.unwrap();
    assert_eq!(row(&store).await.ended_at, None);
    det.advance(Duration::from_secs(60));
    let second = i64::try_from(det.now_ms()).unwrap();
    store.append("s1".into(), vec![ended()]).await.unwrap();
    assert_eq!(row(&store).await.ended_at, Some(second));
    // A session without a `session_git` row is not touched.
    store.create_session(new_session("plain")).await.unwrap();
    store.append("plain".into(), vec![ended()]).await.unwrap();
    assert_eq!(store.git_cleanup_rows().await.unwrap().len(), 1);
}

#[tokio::test]
async fn the_state_is_written_and_announced_only_when_the_pair_changes() {
    let det = Det::new();
    let store = with_session(&det).await;
    let id = SessionId::from("s1");
    let waiting = |due: &str| (CleanupState::Waiting, Some(due.to_owned()));
    let record = |(state, reason): (CleanupState, Option<String>)| {
        let store = store.clone();
        let id = id.clone();
        async move { store.git_cleanup_record(id, state, reason).await.unwrap() }
    };
    assert!(record(waiting("100")).await);
    // `waiting` stays `waiting` with a new due time: no write, no event.
    assert!(!record(waiting("200")).await);
    assert_eq!(row(&store).await.cleanup_state.as_deref(), Some("waiting"));
    let dirty = (CleanupState::Kept, Some("dirty".to_owned()));
    assert!(record(dirty.clone()).await);
    assert!(!record(dirty).await);
    let other = (CleanupState::Kept, Some("not_pushed".to_owned()));
    assert!(record(other).await);
    assert_eq!(
        row(&store).await.cleanup_state.as_deref(),
        Some("kept:not_pushed")
    );
    assert!(record((CleanupState::Trashed, None)).await);
    assert_eq!(
        cleanup_events(&store).await,
        vec![
            waiting("100"),
            (CleanupState::Kept, Some("dirty".to_owned())),
            (CleanupState::Kept, Some("not_pushed".to_owned())),
            (CleanupState::Trashed, None),
        ]
    );
    // A trashed session is no longer a candidate.
    assert!(store.git_cleanup_rows().await.unwrap().is_empty());
    // A session without a row is not recorded.
    assert!(
        !store
            .git_cleanup_record("nobody".into(), CleanupState::Waiting, None)
            .await
            .unwrap()
    );
}
