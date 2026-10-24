//! The fence sender of "Stop checking" (design §11.2, M3, revision 4). The
//! route only records `abandon_state = requested`; this task sends
//! `fleet_git.land_abandon` for each such row: at once, after each link
//! start, and with a backoff of 2, 4, 8, ... 60 s. The row, and so the queue,
//! is freed only by the station's `fenced` answer. A `busy` answer, a
//! timeout and a lost link change nothing.

use std::collections::HashSet;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use fleetdeck_proto::StationId;
use fleetdeck_proto::fleet_git::{
    LandAbandonAnswer, LandAbandonParams, LandState, LandStatusAnswer,
};
use fleetdeck_proto::link::Outcome;
use fleetdeck_proto::version::feature;
use tokio::sync::watch;

use crate::fleet_git_land::LandKick;
use crate::link::DeckLink;
use crate::store::{DeckStore, LinkFeature};

/// The deadline of `fleet_git.land_abandon` (design §11.2, revision 4).
pub const ABANDON_DEADLINE: Duration = Duration::from_secs(130);
/// The longest sleep when nothing is due.
const IDLE: Duration = Duration::from_secs(30);
/// The shortest sleep, so that a store error never spins the loop.
const FLOOR: Duration = Duration::from_millis(100);

/// One `fleet_git.land_abandon` call; `None` for any failure.
pub(crate) async fn abandon_call(
    links: &DeckLink,
    station: &StationId,
    land_id: &str,
) -> Option<LandAbandonAnswer> {
    let control = links.control_for(station)?;
    let params = serde_json::to_value(LandAbandonParams {
        land_id: land_id.to_owned(),
    })
    .ok()?;
    let reply = tokio::time::timeout(
        ABANDON_DEADLINE,
        control.request("fleet_git.land_abandon", None, params),
    )
    .await;
    match reply {
        Ok(Ok(Outcome::Ok(v))) => match serde_json::from_value(v) {
            Ok(answer) => Some(answer),
            Err(e) => {
                tracing::warn!(land = %land_id, error = %e, "land abandon does not parse");
                None
            }
        },
        Ok(Ok(Outcome::Err(e))) => {
            tracing::debug!(land = %land_id, code = %e.code, "fleet_git.land_abandon refused");
            None
        }
        Ok(Err(e)) => {
            tracing::debug!(land = %land_id, error = %e, "fleet_git.land_abandon: link ended");
            None
        }
        Err(_) => {
            tracing::debug!(land = %land_id, "fleet_git.land_abandon timed out");
            None
        }
    }
}

/// Runs the fence sender until `shutdown` is set. Each call is its own task,
/// so a wait of up to 130 s blocks nothing else.
pub async fn run(
    store: DeckStore,
    links: Arc<DeckLink>,
    kick: Arc<LandKick>,
    mut shutdown: watch::Receiver<bool>,
) {
    let active: Arc<Mutex<HashSet<String>>> = Arc::default();
    loop {
        if *shutdown.borrow() {
            return;
        }
        start_due(&store, &links, &kick, &active).await;
        let wait = match store.land_abandon_next_due().await {
            Ok(Some(at)) => {
                let ms = u64::try_from(at.saturating_sub(store.now_ms())).unwrap_or(0);
                Duration::from_millis(ms).min(IDLE)
            }
            _ => IDLE,
        };
        tokio::select! {
            () = kick.abandon.notified() => {}
            () = tokio::time::sleep(wait.max(FLOOR)) => {}
            changed = shutdown.changed() => {
                if changed.is_err() {
                    return;
                }
            }
        }
    }
}

async fn start_due(
    store: &DeckStore,
    links: &Arc<DeckLink>,
    kick: &Arc<LandKick>,
    active: &Arc<Mutex<HashSet<String>>>,
) {
    let due = match store.land_abandon_due().await {
        Ok(due) => due,
        Err(e) => {
            tracing::warn!(error = %e, "land abandon");
            return;
        }
    };
    for (land_id, station) in due {
        if !active
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(land_id.clone())
        {
            continue;
        }
        // The count and the next time are stored before the call, so a
        // crash or a long wait never makes a tight loop.
        if let Err(e) = store.land_abandon_sent(land_id.clone()).await {
            tracing::warn!(error = %e, land = %land_id, "land abandon count");
            active
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .remove(&land_id);
            continue;
        }
        let (store, links, kick, active) = (
            store.clone(),
            Arc::clone(links),
            Arc::clone(kick),
            Arc::clone(active),
        );
        tokio::spawn(async move {
            one(&store, &links, &station, &land_id).await;
            active
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .remove(&land_id);
            kick.admit.notify_one();
        });
    }
}

async fn one(store: &DeckStore, links: &Arc<DeckLink>, station: &str, land_id: &str) {
    let station = StationId::from(station.to_owned());
    if store.link_feature(&station, feature::FLEET_GIT_LAND) != LinkFeature::Negotiated {
        return;
    }
    match abandon_call(links, &station, land_id).await {
        Some(LandAbandonAnswer::Fenced { write, run_state }) => {
            if let Err(e) = store
                .land_abandon_fenced(land_id.to_owned(), write, run_state)
                .await
            {
                tracing::warn!(error = %e, land = %land_id, "land fence");
                return;
            }
            // A terminal state other than `unresolved` takes the outcome
            // fields of the station's status.
            if run_state.is_terminal()
                && run_state != LandState::Unresolved
                && let Some(LandStatusAnswer::Known(status)) =
                    crate::fleet_git_land::status(links, &station, land_id).await
                && let Err(e) = store.land_fill_outcome(land_id.to_owned(), *status).await
            {
                tracing::warn!(error = %e, land = %land_id, "land outcome");
            }
        }
        // `busy` records nothing: the row stays `requested`.
        Some(LandAbandonAnswer::Busy) | None => {}
    }
}
