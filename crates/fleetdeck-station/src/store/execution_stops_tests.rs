//! The witness and stop rows of BG-4 (migration V39).

#![allow(clippy::unwrap_used)]

use std::time::Duration;

use fleetdeck_testkit::Det;

use super::*;
use crate::store::{NewSession, OfflineApprovalPolicy, StoreConfig};

impl Store {
    /// Inserts a stop row in `state` (tests only).
    pub(crate) async fn execution_stop_seed(
        &self,
        (command_id, session, shim_id): (&str, &str, &str),
        state: &str,
        created_at: i64,
    ) {
        let (command_id, session, shim_id, state) = (
            command_id.to_owned(),
            session.to_owned(),
            shim_id.to_owned(),
            state.to_owned(),
        );
        call(&self.inner.db, move |conn| {
            conn.execute(
                "INSERT INTO execution_stops (command_id, session_id, shim_id, plan_id, state,
                     created_at, updated_at) VALUES (?1, ?2, ?3, 'p', ?4, ?5, ?5)",
                params![command_id, session, shim_id, state, created_at],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    }
}

async fn store(det: &Det) -> Store {
    let s = Store::open_in_memory(det.clock(), det.ids(), StoreConfig::default())
        .await
        .unwrap();
    s.recover(Vec::new()).await.unwrap();
    s.create_session(NewSession {
        session_id: "s1".into(),
        provider: fleetdeck_proto::event::ProviderKind::Claude,
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

fn done(state: StopOutcome, code: Option<&'static str>) -> StopResult {
    StopResult {
        state,
        code,
        term_sent: 3,
        kill_sent: 1,
        gone: 3,
        skipped: 1,
    }
}

#[tokio::test]
async fn a_shim_keeps_at_most_512_witnesses_and_the_oldest_go_first() {
    let det = Det::new();
    let store = store(&det).await;
    let old: Vec<(u32, u64)> = (1..=300).map(|pid| (pid, u64::from(pid))).collect();
    store
        .execution_witnesses_record("shim".to_owned(), "boot".to_owned(), old)
        .await
        .unwrap();
    det.advance(Duration::from_secs(1));
    let new: Vec<(u32, u64)> = (301..=600).map(|pid| (pid, u64::from(pid))).collect();
    store
        .execution_witnesses_record("shim".to_owned(), "boot".to_owned(), new)
        .await
        .unwrap();
    let kept = store.execution_witnesses("shim".to_owned()).await.unwrap();
    assert_eq!(kept.len(), 512);
    let pids: Vec<u32> = kept.iter().map(|w| w.pid).collect();
    assert!(pids.iter().all(|pid| *pid > 88), "the 88 oldest went");
    assert!(pids.contains(&600) && pids.contains(&89));
    // Another shim keeps its own rows, and a known pair is refreshed.
    store
        .execution_witnesses_record("other".to_owned(), "boot".to_owned(), vec![(7, 7), (7, 7)])
        .await
        .unwrap();
    assert_eq!(
        store
            .execution_witnesses("other".to_owned())
            .await
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        store
            .execution_witnesses("shim".to_owned())
            .await
            .unwrap()
            .len(),
        512
    );
}

#[tokio::test]
async fn a_stop_row_is_authorized_then_attempted_then_settled_once() {
    let det = Det::new();
    let store = store(&det).await;
    store
        .execution_stop_seed(("c1", "s1", "shim"), AUTHORIZED, 1)
        .await;
    let row = store
        .execution_stop("c1".to_owned())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.state, AUTHORIZED);
    assert!(row.outcome().is_none(), "an open row has no outcome");
    assert!(store.execution_stop_attempt("c1".to_owned()).await.unwrap());
    assert!(
        !store.execution_stop_attempt("c1".to_owned()).await.unwrap(),
        "an attempted row is not attempted again"
    );
    let result = done(StopOutcome::Done, Some(code::PROCESSES_REMAIN));
    assert!(
        store
            .execution_stop_settle("c1".to_owned(), result.clone())
            .await
            .unwrap()
    );
    assert!(
        !store
            .execution_stop_settle("c1".to_owned(), StopResult::failed(code::PLAN_CHANGED))
            .await
            .unwrap(),
        "a settled row never changes"
    );
    let row = store
        .execution_stop("c1".to_owned())
        .await
        .unwrap()
        .unwrap();
    let outcome = row.outcome().unwrap();
    assert_eq!(outcome.outcome, StopOutcome::Done);
    assert_eq!(outcome.code.as_deref(), Some(code::PROCESSES_REMAIN));
    assert_eq!(
        (
            outcome.term_sent,
            outcome.kill_sent,
            outcome.gone,
            outcome.skipped
        ),
        (3, 1, 3, 1)
    );
    assert!(!store.execution_stop_attempt("c1".to_owned()).await.unwrap());
}

#[tokio::test]
async fn the_last_stop_is_the_newest_settled_row_of_the_shim() {
    let det = Det::new();
    let store = store(&det).await;
    for (id, state, at) in [
        ("a", DONE, 1),
        ("b", FAILED, 2),
        ("c", AUTHORIZED, 3),
        ("d", ATTEMPTED, 4),
    ] {
        store
            .execution_stop_seed((id, "s1", "shim"), state, at)
            .await;
    }
    store
        .execution_stop_seed(("e", "s1", "other"), DONE, 9)
        .await;
    let last = store
        .execution_stop_last("shim".to_owned())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(last.command_id, "b", "open rows are skipped");
    assert!(
        store
            .execution_stop_last("none".to_owned())
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn recovery_settles_attempted_as_unknown_and_authorized_as_failed() {
    let det = Det::new();
    let store = store(&det).await;
    for (id, state) in [
        ("auth", AUTHORIZED),
        ("tried", ATTEMPTED),
        ("fin", DONE),
        ("lost", UNKNOWN),
    ] {
        store
            .execution_stop_seed((id, "s1", "shim"), state, 1)
            .await;
    }
    assert_eq!(store.recover_execution_stops().await.unwrap(), (1, 1));
    let state = |id: &'static str| {
        let store = store.clone();
        async move {
            let row = store.execution_stop(id.to_owned()).await.unwrap().unwrap();
            (row.state, row.code)
        }
    };
    assert_eq!(
        state("tried").await,
        (UNKNOWN.to_owned(), Some(code::OUTCOME_UNKNOWN.to_owned()))
    );
    assert_eq!(
        state("auth").await,
        (FAILED.to_owned(), Some(code::PLAN_CHANGED.to_owned()))
    );
    assert_eq!(state("fin").await, (DONE.to_owned(), None));
    assert_eq!(state("lost").await, (UNKNOWN.to_owned(), None));
    assert_eq!(store.recover_execution_stops().await.unwrap(), (0, 0));
}
