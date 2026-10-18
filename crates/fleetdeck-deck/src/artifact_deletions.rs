//! The station deletion job of an artifact (R13 P5a task C, A4 deck side,
//! `docs/v2/design/omarchy-p5a.md` §3.2).
//!
//! [`send_delete`] makes one `artifacts.delete` call (5 s). [`spawn`] starts
//! the retry task. It runs at the deck start, on every change of
//! `DeckStore::watch_board()` (the reachability of a station changes there)
//! and on a 1 s tick. For each station whose link is up it first tries every
//! pending row with the reason `offline` or `unsupported` that was not tried
//! on this link yet (a link that comes up tries them first), then the
//! `retry` rows whose time is due. One call at a time runs for each station.
//! The task writes no audit row: a retry is not an owner action.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use fleetdeck_link::control::Control;
use fleetdeck_link::methods::code as link_code;
use fleetdeck_proto::StationId;
use fleetdeck_proto::artifacts::{ArtifactDeleteParams, ArtifactDeleted, ArtifactDeletedState};
use fleetdeck_proto::link::Outcome;
use tokio::sync::watch;
use tokio::task::{JoinHandle, JoinSet};

use crate::link::DeckLink;
use crate::store::DeckStore;
use crate::store::artifact_tombstones::{TombstoneDue, TryOutcome};

/// The longest wait for the answer of one `artifacts.delete` call.
pub const CALL_WAIT: Duration = Duration::from_secs(5);
/// The time between two passes of the retry task.
pub const TICK: Duration = Duration::from_secs(1);

const METHOD: &str = "artifacts.delete";

/// Sends `artifacts.delete` for `artifact_id` to the live link of `station`.
/// No live link is `Offline`.
pub async fn send_delete(link: &DeckLink, station: &StationId, artifact_id: &str) -> TryOutcome {
    match link.control_for(station) {
        Some(control) => send_on(&control, artifact_id).await,
        None => TryOutcome::Offline,
    }
}

/// One call on `control` and its result: a feature that the link did not
/// negotiate, or the wire code `unsupported` or `not_implemented`, is
/// `Unsupported`; the answer `deleted` for this id is `Deleted`; every other
/// answer, error or the timeout is `Retry`.
async fn send_on(control: &Control, artifact_id: &str) -> TryOutcome {
    let params = serde_json::json!(ArtifactDeleteParams {
        artifact_id: artifact_id.to_owned(),
    });
    match tokio::time::timeout(CALL_WAIT, control.request(METHOD, None, params)).await {
        Ok(Ok(Outcome::Ok(value))) => match serde_json::from_value::<ArtifactDeleted>(value) {
            Ok(done)
                if done.state == ArtifactDeletedState::Deleted
                    && done.artifact_id == artifact_id =>
            {
                TryOutcome::Deleted
            }
            _ => TryOutcome::Retry,
        },
        Ok(Ok(Outcome::Err(e)))
            if matches!(
                e.code.as_str(),
                link_code::FEATURE_NOT_NEGOTIATED | "unsupported" | "not_implemented"
            ) =>
        {
            TryOutcome::Unsupported
        }
        _ => TryOutcome::Retry,
    }
}

/// What the task remembers of the link of a station: the handle that it
/// tried on (its `ended()` tells that the link is gone) and the ids that it
/// tried on it.
struct LinkState {
    control: Control,
    tried: HashSet<String>,
}

struct Shared {
    store: DeckStore,
    link: Arc<DeckLink>,
    links: Mutex<HashMap<StationId, LinkState>>,
}

/// Starts the retry task. It stops when `shutdown` turns true.
pub fn spawn(
    store: DeckStore,
    link: Arc<DeckLink>,
    shutdown: watch::Receiver<bool>,
) -> JoinHandle<()> {
    let shared = Arc::new(Shared {
        store,
        link,
        links: Mutex::new(HashMap::new()),
    });
    tokio::spawn(run(shared, shutdown))
}

async fn run(shared: Arc<Shared>, mut shutdown: watch::Receiver<bool>) {
    let mut board = shared.store.watch_board();
    let mut tick = tokio::time::interval(TICK);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut running: HashSet<StationId> = HashSet::new();
    let mut jobs: JoinSet<StationId> = JoinSet::new();
    loop {
        if *shutdown.borrow() {
            break;
        }
        start_passes(&shared, &mut running, &mut jobs).await;
        tokio::select! {
            result = board.changed() => {
                if result.is_err() {
                    break;
                }
            }
            _ = tick.tick() => {}
            Some(done) = jobs.join_next(), if !jobs.is_empty() => {
                if let Ok(station) = done {
                    running.remove(&station);
                }
            }
            result = shutdown.changed() => {
                if result.is_err() || *shutdown.borrow() {
                    break;
                }
            }
        }
    }
    jobs.shutdown().await;
}

/// Starts one pass for each station with a live link and no pass running.
async fn start_passes(
    shared: &Arc<Shared>,
    running: &mut HashSet<StationId>,
    jobs: &mut JoinSet<StationId>,
) {
    let stations = match shared.store.stations().await {
        Ok(stations) => stations,
        Err(e) => {
            tracing::warn!(error = %e, "artifact deletions: stations not read");
            return;
        }
    };
    for rec in stations {
        let id = rec.station_id;
        if running.contains(&id) || shared.link.control_for(&id).is_none() {
            continue;
        }
        running.insert(id.clone());
        let shared = Arc::clone(shared);
        jobs.spawn(async move {
            pass(&shared, &id).await;
            id
        });
    }
}

/// The ids to try now: the rows that wait for a link and were not tried on
/// this link, then the `retry` rows that are due.
fn order(pending: Vec<(String, TombstoneDue)>, tried: &HashSet<String>) -> Vec<String> {
    let on_link = pending
        .iter()
        .filter(|(id, due)| *due == TombstoneDue::OnLink && !tried.contains(id));
    let due = pending.iter().filter(|(_, due)| *due == TombstoneDue::Due);
    on_link.chain(due).map(|(id, _)| id.clone()).collect()
}

/// One pass over the pending rows of `station`, one call at a time.
async fn pass(shared: &Shared, station: &StationId) {
    let pending = match shared.store.tombstones_pending(station.clone()).await {
        Ok(pending) => pending,
        Err(e) => {
            tracing::warn!(error = %e, "artifact deletions: rows not read");
            return;
        }
    };
    if pending.is_empty() {
        return;
    }
    let Some(control) = shared.link.control_for(station) else {
        return;
    };
    let ids = {
        let mut links = shared.links.lock().unwrap_or_else(PoisonError::into_inner);
        // A link that ended is a new link: its state is dropped.
        if links
            .get(station)
            .is_some_and(|state| state.control.ended().is_some())
        {
            links.remove(station);
        }
        let state = links.entry(station.clone()).or_insert_with(|| LinkState {
            control: control.clone(),
            tried: HashSet::new(),
        });
        let ids = order(pending, &state.tried);
        state.tried.extend(ids.iter().cloned());
        ids
    };
    for id in ids {
        let outcome = send_on(&control, &id).await;
        if let Err(e) = shared
            .store
            .tombstone_set(station.clone(), id, outcome, shared.store.now_ms())
            .await
        {
            tracing::warn!(error = %e, "artifact deletions: result not stored");
        }
    }
}

#[cfg(test)]
#[path = "artifact_deletions_tests.rs"]
mod tests;
