//! The land queue task (design §11.2; FG-8): admission and the status poll.
//!
//! - [`run`] admits the first land that can run after each queue change, each
//!   terminal land, each link start and every 30 s (`admit_next`), and asks
//!   `fleet_git.land_status` for each `running` land after each link start and
//!   every 60 s.
//! - The outbox sender is [`crate::fleet_git_land_outbox`], the fence sender
//!   is [`crate::fleet_git_land_abandon`]. All three wake on [`LandKick`].
//! - An unknown answer, a timeout or a lost link changes nothing: the next
//!   turn asks again.

use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use fleetdeck_proto::StationId;
use fleetdeck_proto::event::{Event, EventBody};
use fleetdeck_proto::fleet_git::{LandStatusAnswer, LandStatusParams};
use fleetdeck_proto::link::Outcome;
use fleetdeck_proto::version::feature;
use tokio::sync::{Notify, watch};

use crate::link::DeckLink;
use crate::store::{DeckStore, LinkFeature};

/// The deadline of `fleet_git.land_status` (decision Q4).
pub const STATUS_DEADLINE: Duration = Duration::from_secs(10);
/// How often a `running` land is asked for its status.
pub const POLL_EVERY: Duration = Duration::from_secs(60);
/// The longest sleep: admission also runs every 30 s.
const TICK: Duration = Duration::from_secs(30);

/// What wakes the three land tasks (decision Q12): one permit each, so a kick
/// before a task waits is not lost.
#[derive(Debug, Default)]
pub struct LandKick {
    pub(crate) admit: Notify,
    pub(crate) outbox: Notify,
    pub(crate) abandon: Notify,
    /// The stations whose link started since the last turn of the poll.
    started: Mutex<BTreeSet<String>>,
}

impl LandKick {
    /// The queue changed: every task looks at its rows.
    pub fn kick(&self) {
        self.admit.notify_one();
        self.outbox.notify_one();
        self.abandon.notify_one();
    }

    /// The link of `station` started: send, poll and admit again.
    pub fn link_started(&self, station: &str) {
        self.started
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(station.to_owned());
        self.kick();
    }

    fn take_started(&self) -> BTreeSet<String> {
        std::mem::take(&mut *self.started.lock().unwrap_or_else(PoisonError::into_inner))
    }
}

/// Runs admission and the status poll until `shutdown` is set.
pub async fn run(
    store: DeckStore,
    links: Arc<DeckLink>,
    kick: Arc<LandKick>,
    mut shutdown: watch::Receiver<bool>,
) {
    let mut polled: HashMap<String, Instant> = HashMap::new();
    let mut live = store.subscribe_live();
    loop {
        if *shutdown.borrow() {
            return;
        }
        let started = kick.take_started();
        admit_all(&store, &kick).await;
        if poll(&store, &links, &started, &mut polled).await {
            admit_all(&store, &kick).await;
        }
        let tick = tokio::time::sleep(TICK);
        tokio::pin!(tick);
        loop {
            tokio::select! {
                () = kick.admit.notified() => break,
                () = &mut tick => break,
                batch = live.recv() => match batch {
                    // A batch with a `session.land` event can end a land.
                    Ok(b) if !b.events.iter().any(|e| {
                        matches!(&e.event, EventBody::Known(Event::SessionLand(_)))
                    }) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                    _ => break,
                },
                changed = shutdown.changed() => {
                    if changed.is_err() {
                        return;
                    }
                    break;
                }
            }
        }
    }
}

/// Admits lands until none changes, and wakes the outbox sender.
async fn admit_all(store: &DeckStore, kick: &LandKick) {
    // A bound, so a store error that keeps reporting a change cannot spin.
    for _ in 0..64 {
        match store.land_admit_next().await {
            Ok(true) => kick.outbox.notify_one(),
            Ok(false) => return,
            Err(e) => {
                tracing::warn!(error = %e, "land admission");
                return;
            }
        }
    }
}

/// Asks the status of each `running` land that is due. Returns whether a row
/// changed.
async fn poll(
    store: &DeckStore,
    links: &Arc<DeckLink>,
    started: &BTreeSet<String>,
    polled: &mut HashMap<String, Instant>,
) -> bool {
    let running = match store.land_running().await {
        Ok(rows) => rows,
        Err(e) => {
            tracing::warn!(error = %e, "land poll");
            return false;
        }
    };
    polled.retain(|id, _| running.iter().any(|(r, _)| r == id));
    let mut changed = false;
    for (land_id, station) in running {
        let due = started.contains(&station)
            || polled
                .get(&land_id)
                .is_none_or(|t| t.elapsed() >= POLL_EVERY);
        if !due {
            continue;
        }
        let station = StationId::from(station);
        if store.link_feature(&station, feature::FLEET_GIT_LAND) != LinkFeature::Negotiated {
            continue;
        }
        let Some(answer) = status(links, &station, &land_id).await else {
            continue;
        };
        polled.insert(land_id.clone(), Instant::now());
        match store.land_apply_status(land_id.clone(), answer).await {
            Ok(c) => changed |= c,
            Err(e) => tracing::warn!(error = %e, land = %land_id, "land status"),
        }
    }
    changed
}

/// One `fleet_git.land_status` call; `None` for any failure.
pub(crate) async fn status(
    links: &DeckLink,
    station: &StationId,
    land_id: &str,
) -> Option<LandStatusAnswer> {
    let control = links.control_for(station)?;
    let params = serde_json::to_value(LandStatusParams {
        land_id: land_id.to_owned(),
    })
    .ok()?;
    let reply = tokio::time::timeout(
        STATUS_DEADLINE,
        control.request("fleet_git.land_status", None, params),
    )
    .await;
    match reply {
        Ok(Ok(Outcome::Ok(v))) => match serde_json::from_value(v) {
            Ok(answer) => Some(answer),
            Err(e) => {
                tracing::warn!(land = %land_id, error = %e, "land status does not parse");
                None
            }
        },
        Ok(Ok(Outcome::Err(e))) => {
            tracing::debug!(land = %land_id, code = %e.code, "fleet_git.land_status refused");
            None
        }
        Ok(Err(e)) => {
            tracing::debug!(land = %land_id, error = %e, "fleet_git.land_status: link ended");
            None
        }
        Err(_) => {
            tracing::debug!(land = %land_id, "fleet_git.land_status timed out");
            None
        }
    }
}
