//! The home attempt sender and poller (design §8.4, decisions B6, B15; FG-5
//! phase 2, M5). The deck never calls `project.ensure_home` from a route: the
//! route stores an attempt in state `sent`, and this task delivers its
//! stored request unchanged (the same `attempt_id` and `request_digest`) and
//! follows a clone with `project.home_status`.
//!
//! - One task: [`run`]. It wakes on [`HomesKick`] (the add and retry routes,
//!   each link start) and otherwise sleeps until the earliest `next_send_at`
//!   or the next 10 s poll of a `cloning` home.
//! - A send that times out, fails in transport, finds no link, or gets an
//!   error that does not end the attempt, stays `sent` with the backoff
//!   (2, 4, 8, ... 60 s). A link without `fleet_git.homes.v1` sends nothing
//!   and waits for the next link start.
//! - An answer applies only through the fence (the store).

use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use fleetdeck_proto::StationId;
use fleetdeck_proto::fleet_git::{HomeStatus, HomeStatusParams};
use fleetdeck_proto::link::Outcome;
use fleetdeck_proto::version::feature;
use tokio::sync::{Notify, watch};

use crate::link::{DeckLink, fleet_git_sync};
use crate::store::home_attempts::{Applied, Due, fails_attempt};
use crate::store::{DeckStore, LinkFeature};

/// The deadline of `project.ensure_home` and of `project.home_status`.
pub const DEADLINE: Duration = Duration::from_secs(10);
/// How often a `cloning` home is polled.
pub const POLL_EVERY: Duration = Duration::from_secs(10);
/// The longest sleep when nothing is due.
const IDLE: Duration = Duration::from_secs(60);
/// The shortest sleep, so that a store error never spins the loop.
const FLOOR: Duration = Duration::from_millis(100);

/// What wakes the task (decision B19). A kick keeps one permit, so a kick
/// before the task waits is not lost.
#[derive(Debug, Default)]
pub struct HomesKick {
    notify: Notify,
    /// The stations whose link started since the last turn: each `cloning`
    /// home of such a station is polled once.
    started: Mutex<BTreeSet<String>>,
}

impl HomesKick {
    /// An attempt was stored: send it now.
    pub fn kick(&self) {
        self.notify.notify_one();
    }

    /// The link of `station` started.
    pub fn link_started(&self, station: &str) {
        self.started
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(station.to_owned());
        self.notify.notify_one();
    }

    fn take_started(&self) -> BTreeSet<String> {
        std::mem::take(&mut *self.started.lock().unwrap_or_else(PoisonError::into_inner))
    }
}

/// What the task does (decision B6e).
#[derive(Debug, Clone, Copy)]
pub struct HomesRunOptions {
    /// `false`: the task sends no `project.ensure_home` (the M5 test).
    pub send: bool,
}

/// Runs the sender and the poller until `shutdown` is set.
pub async fn run(
    store: DeckStore,
    links: Arc<DeckLink>,
    kick: Arc<HomesKick>,
    mut shutdown: watch::Receiver<bool>,
    options: HomesRunOptions,
) {
    let mut polled: HashMap<String, Instant> = HashMap::new();
    loop {
        if *shutdown.borrow() {
            return;
        }
        let started = kick.take_started();
        if options.send {
            send_due(&store, &links).await;
        }
        poll(&store, &links, &started, &mut polled).await;
        let wait = next_wait(&store, options, &polled).await;
        tokio::select! {
            () = kick.notify.notified() => {}
            () = tokio::time::sleep(wait) => {}
            changed = shutdown.changed() => {
                // A dropped sender also ends the task.
                if changed.is_err() {
                    return;
                }
            }
        }
    }
}

/// How long to sleep: until the earliest due send, or the next poll.
async fn next_wait(
    store: &DeckStore,
    options: HomesRunOptions,
    polled: &HashMap<String, Instant>,
) -> Duration {
    let mut wait = IDLE;
    if options.send
        && let Ok(Some(at)) = store.home_attempts_next_due().await
    {
        let ms = u64::try_from(at.saturating_sub(store.now_ms())).unwrap_or(0);
        wait = wait.min(Duration::from_millis(ms));
    }
    if let Ok(cloning) = store.cloning_homes().await
        && !cloning.is_empty()
    {
        let next = cloning
            .iter()
            .map(|c| {
                polled
                    .get(&c.attempt_id)
                    .map_or(Duration::ZERO, |t| POLL_EVERY.saturating_sub(t.elapsed()))
            })
            .min()
            .unwrap_or(POLL_EVERY);
        wait = wait.min(next);
    }
    wait.max(FLOOR)
}

/// Sends every attempt that is due.
async fn send_due(store: &DeckStore, links: &Arc<DeckLink>) {
    let due = match store.home_attempts_due(store.now_ms()).await {
        Ok(due) => due,
        Err(e) => {
            tracing::warn!(error = %e, "home attempts");
            return;
        }
    };
    for attempt in due {
        send_one(store, links, attempt).await;
    }
}

async fn send_one(store: &DeckStore, links: &Arc<DeckLink>, attempt: Due) {
    let station = StationId::from(attempt.station_id.clone());
    let id = attempt.attempt_id.clone();
    let log =
        |e: crate::store::DeckError| tracing::warn!(error = %e, attempt = %id, "home attempt");
    match store.link_feature(&station, feature::FLEET_GIT_HOMES) {
        // No send, no `send_attempts` change: the next link start sends it.
        LinkFeature::Without => {
            if let Err(e) = store.home_attempt_wait_for_link(id.clone()).await {
                log(e);
            }
            return;
        }
        LinkFeature::Down => return backoff(store, &id).await,
        LinkFeature::Negotiated => {}
    }
    let Some(control) = links.control_for(&station) else {
        return backoff(store, &id).await;
    };
    let Ok(params) = serde_json::to_value(&attempt.request) else {
        return backoff(store, &id).await;
    };
    let reply = tokio::time::timeout(
        DEADLINE,
        control.request("project.ensure_home", None, params),
    )
    .await;
    match reply {
        Ok(Ok(Outcome::Ok(v))) => match serde_json::from_value::<HomeStatus>(v) {
            Ok(status) => apply(store, links, &station, status).await,
            Err(e) => {
                tracing::warn!(attempt = %id, error = %e, "home status does not parse");
                backoff(store, &id).await;
            }
        },
        Ok(Ok(Outcome::Err(e))) if fails_attempt(&e.code) => {
            match store.home_attempt_fail(id.clone(), e.code, e.message).await {
                Ok(applied) => after(links, &station, applied).await,
                Err(e) => log(e),
            }
        }
        Ok(Ok(Outcome::Err(e))) => {
            tracing::debug!(attempt = %id, code = %e.code, "project.ensure_home refused");
            backoff(store, &id).await;
        }
        Ok(Err(e)) => {
            tracing::debug!(attempt = %id, error = %e, "project.ensure_home: link ended");
            backoff(store, &id).await;
        }
        Err(_) => {
            tracing::debug!(attempt = %id, "project.ensure_home timed out");
            backoff(store, &id).await;
        }
    }
}

async fn backoff(store: &DeckStore, id: &str) {
    if let Err(e) = store
        .home_attempt_backoff(id.to_owned(), store.now_ms())
        .await
    {
        tracing::warn!(error = %e, attempt = %id, "home attempt backoff");
    }
}

/// Applies an answer through the fence; a drop is logged, a change sends the
/// station its snapshot (the home list and the checks follow).
async fn apply(store: &DeckStore, links: &Arc<DeckLink>, station: &StationId, status: HomeStatus) {
    let id = status.attempt_id.clone();
    match store.home_status_apply(status).await {
        Ok(applied) => after(links, station, applied).await,
        Err(e) => tracing::warn!(error = %e, attempt = %id, "home status"),
    }
}

async fn after(links: &Arc<DeckLink>, station: &StationId, applied: Applied) {
    match applied {
        Applied::Stale => tracing::warn!(station = %station, "home_status_stale"),
        Applied::Unchanged => {}
        Applied::Changed { .. } => fleet_git_sync::sync_changed(links, station.clone()).await,
    }
}

/// Polls the `cloning` homes that are due: every 10 s, and once for each
/// station whose link started.
async fn poll(
    store: &DeckStore,
    links: &Arc<DeckLink>,
    started: &BTreeSet<String>,
    polled: &mut HashMap<String, Instant>,
) {
    let Ok(cloning) = store.cloning_homes().await else {
        return;
    };
    polled.retain(|id, _| cloning.iter().any(|c| &c.attempt_id == id));
    for home in cloning {
        let due = started.contains(&home.station_id)
            || polled
                .get(&home.attempt_id)
                .is_none_or(|t| t.elapsed() >= POLL_EVERY);
        if !due {
            continue;
        }
        let station = StationId::from(home.station_id.clone());
        if store.link_feature(&station, feature::FLEET_GIT_HOMES) != LinkFeature::Negotiated {
            continue;
        }
        let Some(control) = links.control_for(&station) else {
            continue;
        };
        polled.insert(home.attempt_id.clone(), Instant::now());
        let params = HomeStatusParams {
            project_id: home.project_id.clone(),
            attempt_id: home.attempt_id.clone(),
        };
        let Ok(params) = serde_json::to_value(&params) else {
            continue;
        };
        let reply = tokio::time::timeout(
            DEADLINE,
            control.request("project.home_status", None, params),
        )
        .await;
        match reply {
            Ok(Ok(Outcome::Ok(v))) => match serde_json::from_value::<HomeStatus>(v) {
                Ok(status) => apply(store, links, &station, status).await,
                Err(e) => tracing::warn!(error = %e, "home status does not parse"),
            },
            Ok(Ok(Outcome::Err(e))) if e.code == "unknown_attempt" => {
                // The station lost the attempt: the stored request goes out again.
                if let Err(e) = store.home_attempt_unknown(home.attempt_id.clone()).await {
                    tracing::warn!(error = %e, "home attempt");
                }
            }
            Ok(Ok(Outcome::Err(e))) if fails_attempt(&e.code) => {
                match store
                    .home_attempt_fail(home.attempt_id.clone(), e.code, e.message)
                    .await
                {
                    Ok(applied) => after(links, &station, applied).await,
                    Err(e) => tracing::warn!(error = %e, "home attempt"),
                }
            }
            // Any other answer changes nothing: the home is polled again.
            Ok(Ok(Outcome::Err(e))) => {
                tracing::debug!(code = %e.code, "project.home_status refused");
            }
            Ok(Err(e)) => tracing::debug!(error = %e, "project.home_status: link ended"),
            Err(_) => tracing::debug!("project.home_status timed out"),
        }
    }
}
