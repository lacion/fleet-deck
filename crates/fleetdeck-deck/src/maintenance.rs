//! The deck maintenance task (NV-11): it removes old audit rows, so that the
//! audit log does not grow without a bound. A second timer deletes the
//! expired display pairings every 30 s (R13 §5B item 2).

use std::time::Duration;

use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::store::DeckStore;

/// The wait before the first prune.
pub const FIRST: Duration = Duration::from_secs(60);

/// The time between two prunes.
pub const EVERY: Duration = Duration::from_secs(60 * 60);

/// The time between two prunes of the display pairings (R13 §5B).
pub const PAIRING_EVERY: Duration = Duration::from_secs(30);

/// Starts the maintenance task. It stops when `shutdown` turns true.
pub fn spawn(store: DeckStore, shutdown: watch::Receiver<bool>) -> JoinHandle<()> {
    tokio::spawn(async move {
        tokio::join!(
            run(store.clone(), FIRST, EVERY, shutdown.clone()),
            run_pairing_prune(store, PAIRING_EVERY, shutdown),
        );
    })
}

/// Deletes the expired display pairings at each tick.
async fn run_pairing_prune(store: DeckStore, every: Duration, mut shutdown: watch::Receiver<bool>) {
    let mut tick = tokio::time::interval_at(tokio::time::Instant::now() + every, every);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = tick.tick() => match store.pairing_prune(store.now_ms()).await {
                Ok(removed) => tracing::debug!(removed, "the pairing prune ran"),
                Err(e) => tracing::warn!(error = %e, "the pairing prune failed"),
            },
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    return;
                }
            }
        }
    }
}

async fn run(
    store: DeckStore,
    first: Duration,
    every: Duration,
    mut shutdown: watch::Receiver<bool>,
) {
    let mut tick = tokio::time::interval_at(tokio::time::Instant::now() + first, every);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = tick.tick() => match store.prune_audit().await {
                Ok(removed) => tracing::debug!(removed, "the audit prune ran"),
                Err(e) => tracing::warn!(error = %e, "the audit prune failed"),
            },
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    return;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use fleetdeck_testkit::Det;

    use super::*;
    use crate::store::audit::{AuditResult, NewAudit};

    #[tokio::test]
    async fn the_maintenance_task_prunes_the_audit_log_and_stops_on_shutdown() {
        let det = Det::new();
        let store = DeckStore::open_in_memory(det.clock(), det.ids())
            .await
            .unwrap();
        for i in 0..3 {
            store
                .audit(NewAudit::new(
                    "anonymous",
                    format!("a{i}"),
                    AuditResult::Refused,
                ))
                .await
                .unwrap();
        }
        det.advance(Duration::from_secs(91 * 24 * 60 * 60));
        let (stop, rx) = watch::channel(false);
        let task = tokio::spawn(run(
            store.clone(),
            Duration::from_millis(10),
            Duration::from_millis(10),
            rx,
        ));
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        while !store.audit_log(10).await.unwrap().is_empty() {
            assert!(tokio::time::Instant::now() < deadline, "not pruned");
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        stop.send(true).unwrap();
        tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn the_pairing_prune_timer_runs_and_stops_on_shutdown() {
        let det = Det::new();
        let store = DeckStore::open_in_memory(det.clock(), det.ids())
            .await
            .unwrap();
        let (id, _, _) = store
            .pairing_create("a".to_owned(), String::new())
            .await
            .unwrap();
        det.advance(Duration::from_secs(6 * 60));
        let (stop, rx) = watch::channel(false);
        let task = tokio::spawn(run_pairing_prune(
            store.clone(),
            Duration::from_millis(10),
            rx,
        ));
        // The timer runs the prune; the count of the deleted rows is tested
        // in `store::displays_pairing`.
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(store.pairing_get(id).await.unwrap().is_none());
        stop.send(true).unwrap();
        tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap();
    }
}
