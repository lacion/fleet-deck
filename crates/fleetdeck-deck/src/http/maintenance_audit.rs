//! The completion audit of a maintenance run (BG-1, design
//! `docs/v2/design/board-gaps.md` section 2.8 and 2.9): one
//! `station.maintenance` row with `stage=completed` for each maintenance ID.
//! A watcher ([`watch_completion`]) writes it after an accepted open, without
//! a browser that polls; a `maintenance.list` read that passes through the
//! deck writes it too. A claim map (the rules of the BG-4 outcome row, see
//! `execution_routes.rs`) keeps the row to one for each ID: a claim in flight
//! and a row that is written are different states. After a restart of the
//! deck the map is empty; a later list read then writes the row again.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, LazyLock, Mutex, PoisonError};
use std::time::Duration;

use fleetdeck_proto::desktop_tools::{
    MaintenanceInfo, MaintenanceList, MaintenanceState, PostCheck,
};
use fleetdeck_proto::link::Outcome;
use serde_json::json;

use super::Board;
use super::execution_routes::until_written;
use super::settings_routes::station_call_wait;
use crate::store::{AuditResult, NewAudit};

/// The audit action of a maintenance run.
pub(super) const ACTION: &str = "station.maintenance";

/// The longest wait for one `maintenance.list` of the watcher.
const LIST_WAIT: Duration = Duration::from_secs(10);

/// How many maintenance IDs the deck remembers; the written IDs are forgotten
/// when the map is full.
const AUDITED_CAP: usize = 1_024;

/// What the deck knows of the completion row of a maintenance ID.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mark {
    /// A writer holds the claim and its write has not ended.
    InFlight,
    /// The row is written.
    Written,
}

/// The answer of [`claim`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Claim {
    /// The caller holds the claim now and must write the row.
    Mine,
    /// Another writer holds it: the row is not proven written.
    InFlight,
    /// The row is written.
    Written,
}

/// The maintenance IDs whose completion row the deck writes or wrote in this
/// process.
static COMPLETED: LazyLock<Mutex<HashMap<String, Mark>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// The maintenance IDs that a watcher watches now.
static WATCHING: LazyLock<Mutex<HashSet<String>>> = LazyLock::new(|| Mutex::new(HashSet::new()));

/// Takes the claim of a maintenance ID when no writer holds it and its row is
/// not written. Otherwise tells why not.
fn claim(maintenance_id: &str) -> Claim {
    let mut seen = COMPLETED.lock().unwrap_or_else(PoisonError::into_inner);
    match seen.get(maintenance_id) {
        Some(Mark::InFlight) => return Claim::InFlight,
        Some(Mark::Written) => return Claim::Written,
        None => {}
    }
    if seen.len() >= AUDITED_CAP {
        // Only the written rows are forgotten; a claim in flight stays.
        seen.retain(|_, mark| *mark == Mark::InFlight);
    }
    seen.insert(maintenance_id.to_owned(), Mark::InFlight);
    Claim::Mine
}

/// Gives a claimed maintenance ID back, after a failed write.
fn release(maintenance_id: &str) {
    COMPLETED
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .remove(maintenance_id);
}

/// Records that the row of a claimed maintenance ID is written.
fn written(maintenance_id: &str) {
    COMPLETED
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(maintenance_id.to_owned(), Mark::Written);
}

/// The audit detail of a completed run: the fixed words of the outcome, the
/// exit code and the post-check. No terminal byte.
fn completion_detail(run: &MaintenanceInfo) -> String {
    let outcome = match run.state {
        MaintenanceState::Lost => "lost",
        _ => "exited",
    };
    let code = run
        .exit_code
        .map_or_else(|| "-".to_owned(), |code| code.to_string());
    let post = match run.post_check {
        Some(PostCheck::Ok) => "ok",
        Some(PostCheck::TransactionChanged) => "transaction_changed",
        None => "-",
    };
    format!(
        "stage=completed maintenance={} outcome={outcome} code={code} post={post}",
        run.maintenance_id
    )
}

/// One `stage=completed` row for a run that has ended, when the deck did not
/// write the row of its maintenance ID yet in this process. A failed write
/// gives the ID back: a later read or the watcher can write the row again.
/// Returns `true` when the run has not ended, or when the row is written (by
/// this call or by another). Returns `false` when the write failed or when
/// the write of another writer has not ended: the watcher tries again.
pub(super) async fn audit_completion(
    board: &Board,
    actor: &str,
    station: &str,
    run: &MaintenanceInfo,
) -> bool {
    // A run that is not finished, or an ID that is not a station ID, writes
    // no row.
    if !matches!(run.state, MaintenanceState::Exited | MaintenanceState::Lost)
        || !fleetdeck_proto::desktop_tools::is_hex_id(&run.maintenance_id)
    {
        return true;
    }
    match claim(&run.maintenance_id) {
        Claim::Written => return true,
        Claim::InFlight => return false,
        Claim::Mine => {}
    }
    let clean = run.state == MaintenanceState::Exited
        && run.exit_code == Some(0)
        && run.post_check == Some(PostCheck::Ok);
    let result = if clean {
        AuditResult::Ok
    } else {
        AuditResult::Failed
    };
    let row = NewAudit::new(actor, ACTION, result)
        .target(station)
        .detail(completion_detail(run));
    if let Err(e) = board.store.audit(row).await {
        tracing::warn!(error = %e, maintenance = %run.maintenance_id, "the maintenance completion row was not written");
        release(&run.maintenance_id);
        return false;
    }
    written(&run.maintenance_id);
    true
}

/// One `maintenance.list` of the watcher; `None` for any failure.
async fn list(board: &Board, station: &str) -> Option<MaintenanceList> {
    let (_, outcome) = station_call_wait(board, station, "maintenance.list", json!({}), LIST_WAIT)
        .await
        .ok()?;
    match outcome {
        Outcome::Ok(value) => serde_json::from_value(value).ok(),
        Outcome::Err(_) => None,
    }
}

/// Leaves [`WATCHING`] when the watcher ends.
struct Watching(String);

impl Drop for Watching {
    fn drop(&mut self) {
        WATCHING
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&self.0);
    }
}

/// Watches for the end of an accepted run without a browser: reads
/// `maintenance.list` every `board.maintenance_watch.poll`, for at most
/// `board.maintenance_watch.limit`, until the run is `exited` or `lost`; then
/// it writes the completion row (again after a failed write, for at most an
/// hour). A run that is not in the list ends the watch without a row. A read
/// that fails is not the end: the watcher waits and goes on. One watcher runs
/// for each maintenance ID.
pub(super) fn watch_completion(
    board: Arc<Board>,
    actor: String,
    station: String,
    maintenance_id: String,
) {
    let fresh = WATCHING
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(maintenance_id.clone());
    if !fresh {
        return;
    }
    tokio::spawn(async move {
        let _leave = Watching(maintenance_id.clone());
        let watch = board.maintenance_watch;
        let until = tokio::time::Instant::now() + watch.limit;
        while tokio::time::Instant::now() < until {
            if let Some(runs) = list(&board, &station).await {
                let Some(run) = runs
                    .runs
                    .iter()
                    .find(|run| run.maintenance_id == maintenance_id)
                else {
                    return;
                };
                if matches!(run.state, MaintenanceState::Exited | MaintenanceState::Lost) {
                    until_written(watch.poll, "completed", || {
                        audit_completion(&board, &actor, &station, run)
                    })
                    .await;
                    return;
                }
            }
            tokio::time::sleep(watch.poll).await;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::{Claim, claim, release, written};

    #[test]
    fn claims_are_mine_then_in_flight_then_written() {
        assert_eq!(claim("maintenance-claim-test"), Claim::Mine);
        assert_eq!(claim("maintenance-claim-test"), Claim::InFlight);
        written("maintenance-claim-test");
        assert_eq!(claim("maintenance-claim-test"), Claim::Written);
        release("maintenance-claim-test");
        assert_eq!(claim("maintenance-claim-test"), Claim::Mine);
        release("maintenance-claim-test");
    }
}
