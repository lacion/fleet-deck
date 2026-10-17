//! The Fleet Git snapshot sender (design §3.3, FG-1 to FG-4): the deck sends
//! `fleet_git.projects_set` with the full Fleet Git state of a station
//! after its link is up (after the first `station.info`) and after each
//! change of a project that touches the station.
//!
//! - The revision and the digest come from `DeckStore::git_snapshot_next`
//!   (`max(stored + 1, now in ms)`, SHA-256 of the snapshot at revision 0).
//! - One send at a time for each station, so revisions go out in order.
//! - The request has a deadline of 10 s and no retry loop: a lost send is
//!   repeated on the next link-up and on the next change.
//! - `stored: false` with a station revision at or above ours: the deck adopts
//!   a revision above both and sends once more.
//! - A station without `fleet_git.v1` gets no call. With no live link the
//!   state is stored and `sent_at` stays empty.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex, PoisonError};
use std::time::Duration;

use fleetdeck_proto::StationId;
use fleetdeck_proto::fleet_git::SnapshotAck;
use fleetdeck_proto::link::Outcome;
use fleetdeck_proto::version::feature;

use super::DeckLink;
use crate::store::LinkFeature;

/// The deadline of one `fleet_git.projects_set` request.
pub const SEND_DEADLINE: Duration = Duration::from_secs(10);

/// One lock for each station: the sends of a station do not overlap.
static LOCKS: LazyLock<Mutex<HashMap<StationId, Arc<tokio::sync::Mutex<()>>>>> =
    LazyLock::new(Mutex::default);

fn lock_of(station: &StationId) -> Arc<tokio::sync::Mutex<()>> {
    let mut locks = LOCKS.lock().unwrap_or_else(PoisonError::into_inner);
    Arc::clone(locks.entry(station.clone()).or_default())
}

/// The link of `station` came up (the first `station.info` answered): sends
/// the snapshot, also when the station acknowledged this state before. The
/// home attempts of the station are due at once, and its `cloning` homes are
/// polled once (FG-5, decision B6a).
pub async fn sync_station(ctx: &Arc<DeckLink>, station: StationId) {
    let now = ctx.store.now_ms();
    if let Err(e) = ctx
        .store
        .home_attempts_link_start(station.0.clone(), now)
        .await
    {
        tracing::warn!(station = %station, error = %e, "fleet git sync: home attempts");
    }
    ctx.homes_kick.link_started(station.as_str());
    ctx.land_kick.link_started(station.as_str());
    ctx.move_kick.link_started(station.as_str());
    ctx.radar_kick.link_started(station.as_str());
    send(ctx, &station, true).await;
}

/// A change touched `station`: sends the snapshot unless the station
/// acknowledged this very state.
pub async fn sync_changed(ctx: &Arc<DeckLink>, station: StationId) {
    send(ctx, &station, false).await;
}

/// A project changed: sends the snapshot to every station that has a home of
/// it.
pub async fn sync_project(ctx: &Arc<DeckLink>, project_id: String) {
    match ctx.store.project(project_id).await {
        Ok(Some(project)) => {
            for station in home_stations(&project) {
                sync_changed(ctx, station).await;
            }
        }
        Ok(None) => {}
        Err(e) => tracing::warn!(error = %e, "fleet git sync: project"),
    }
}

/// The stations that have a home of `project`; its copy when it has none.
#[must_use]
pub fn home_stations(project: &fleetdeck_proto::integrations::Project) -> Vec<StationId> {
    if project.homes.is_empty() {
        return vec![StationId::from(project.station_id.clone())];
    }
    project.homes.iter().map(|h| h.station_id.clone()).collect()
}

/// Runs [`sync_changed`] on its own task (a route does not wait for the
/// station).
pub fn spawn_changed(ctx: &Arc<DeckLink>, station: StationId) {
    let ctx = Arc::clone(ctx);
    tokio::spawn(async move { sync_changed(&ctx, station).await });
}

/// Runs [`sync_project`] on its own task.
pub fn spawn_project(ctx: &Arc<DeckLink>, project_id: String) {
    let ctx = Arc::clone(ctx);
    tokio::spawn(async move { sync_project(&ctx, project_id).await });
}

async fn send(ctx: &Arc<DeckLink>, station: &StationId, force: bool) {
    let lock = lock_of(station);
    let _one_at_a_time = lock.lock().await;
    // A second try only after `stored: false` with a revision at or above
    // ours.
    for attempt in 0..2 {
        let snapshot = match ctx.store.git_snapshot_next(station, force).await {
            Ok(Some(snapshot)) => snapshot,
            Ok(None) => return,
            Err(e) => {
                tracing::warn!(station = %station, error = %e, "fleet git snapshot");
                return;
            }
        };
        // A station that did not negotiate the feature gets no call; with
        // no live link the state is stored and goes out at the next link-up.
        if ctx.store.link_feature(station, feature::FLEET_GIT) != LinkFeature::Negotiated {
            return;
        }
        let Some(control) = ctx.control_for(station) else {
            return;
        };
        let Ok(params) = serde_json::to_value(&snapshot) else {
            return;
        };
        let outcome = tokio::time::timeout(
            SEND_DEADLINE,
            control.request("fleet_git.projects_set", None, params),
        )
        .await;
        let ack = match outcome {
            Ok(Ok(Outcome::Ok(v))) => match serde_json::from_value::<SnapshotAck>(v) {
                Ok(ack) => ack,
                Err(e) => {
                    tracing::warn!(station = %station, error = %e, "fleet git ack does not parse");
                    return;
                }
            },
            Ok(Ok(Outcome::Err(e))) => {
                tracing::warn!(station = %station, code = %e.code, "fleet git snapshot refused");
                return;
            }
            Ok(Err(e)) => {
                tracing::debug!(station = %station, error = %e, "fleet git snapshot: link ended");
                return;
            }
            Err(_) => {
                tracing::debug!(station = %station, "fleet git snapshot timed out");
                return;
            }
        };
        if ack.stored {
            if let Err(e) = ctx.store.git_snapshot_acked(station, snapshot, ack).await {
                tracing::warn!(station = %station, error = %e, "fleet git ack");
            }
            return;
        }
        // Stale for the station: adopt a revision above both, send again once.
        if attempt == 1 {
            tracing::warn!(station = %station, "fleet git snapshot stayed stale");
            return;
        }
        if let Err(e) = ctx.store.git_snapshot_adopt(station, ack.revision).await {
            tracing::warn!(station = %station, error = %e, "fleet git adopt");
            return;
        }
    }
}
