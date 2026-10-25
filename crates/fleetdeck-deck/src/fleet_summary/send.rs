//! The sender task of the fleet summary (`omarchy-p4.md` §3.2, B1).
//!
//! The task keeps a [`Schedule`] (pure, tested with explicit times) and a
//! `JoinSet` of push tasks, one at most for each station. It builds the
//! summary once for all stations that are due, and only then.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use fleetdeck_link::control::Control;
use fleetdeck_proto::StationId;
use fleetdeck_proto::fleet_summary::FleetSummary;
use fleetdeck_proto::link::Outcome;
use fleetdeck_proto::version::feature;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinSet;
use tokio::time::Instant;

use super::build::{StationEntry, build, pick_board};
use super::{Limits, SummaryCounters};
use crate::http::endpoints::Endpoints;
use crate::link::DeckLink;
use crate::store::{DeckError, DeckStore, LinkFeature};

/// The link method that carries the summary.
const METHOD: &str = "fleet.summary";

/// What the task shares with its push tasks.
pub(super) struct Shared {
    pub store: DeckStore,
    pub link: Arc<DeckLink>,
    pub endpoints: Arc<Endpoints>,
    pub limits: Limits,
    pub counters: Arc<SummaryCounters>,
}

/// The state of one station in the schedule.
#[derive(Debug, Default)]
struct Slot {
    /// The station is eligible now.
    eligible: bool,
    /// A change, a link up or a setting turned on, waits for a push.
    pending: bool,
    /// A push is in flight.
    in_flight: bool,
    /// When the last push started.
    last_push: Option<Instant>,
}

/// When each station gets a push: at most one in `debounce_ms`, one in
/// flight, and a keepalive every `keepalive_ms`.
#[derive(Debug)]
pub(super) struct Schedule {
    limits: Limits,
    slots: HashMap<StationId, Slot>,
}

impl Schedule {
    pub(super) fn new(limits: Limits) -> Self {
        Self {
            limits,
            slots: HashMap::new(),
        }
    }

    /// Takes the eligible stations of this moment. `changed` is true after a
    /// change of the board. A station that becomes eligible (a link comes
    /// up, a setting turns on) is pending; a station that is not eligible
    /// loses its pending push.
    pub(super) fn observe(&mut self, eligible: &HashSet<StationId>, changed: bool) {
        for (id, slot) in &mut self.slots {
            if !eligible.contains(id) {
                slot.eligible = false;
                slot.pending = false;
            }
        }
        for id in eligible {
            let slot = self.slots.entry(id.clone()).or_default();
            if !slot.eligible {
                slot.eligible = true;
                slot.pending = true;
            } else if changed {
                slot.pending = true;
            }
        }
    }

    /// The stations that get a push now. Each one is marked in flight.
    pub(super) fn due(&mut self, now: Instant) -> Vec<StationId> {
        let debounce = Duration::from_millis(self.limits.debounce_ms);
        let keepalive = Duration::from_millis(self.limits.keepalive_ms);
        // A station that is not eligible keeps its time of the last push for
        // one debounce window, so a link that flaps is still limited.
        self.slots.retain(|_, s| {
            s.eligible
                || s.in_flight
                || s.last_push
                    .is_some_and(|t| now.saturating_duration_since(t) < debounce)
        });
        let mut out = Vec::new();
        for (id, slot) in &mut self.slots {
            if !slot.eligible || slot.in_flight {
                continue;
            }
            let waited = slot.last_push.map(|t| now.saturating_duration_since(t));
            let window_open = waited.is_none_or(|w| w >= debounce);
            let keepalive_due = waited.is_some_and(|w| w >= keepalive);
            if (slot.pending && window_open) || keepalive_due {
                slot.pending = false;
                slot.in_flight = true;
                slot.last_push = Some(now);
                out.push(id.clone());
            }
        }
        out.sort();
        out
    }

    /// The push to `station` ended.
    pub(super) fn finish(&mut self, station: &StationId) {
        if let Some(slot) = self.slots.get_mut(station) {
            slot.in_flight = false;
        }
    }

    /// The next time that [`Schedule::due`] can give a station, or `None`
    /// when nothing waits. A station in flight wakes the task when it ends.
    pub(super) fn next_wake(&self, now: Instant) -> Option<Instant> {
        let debounce = Duration::from_millis(self.limits.debounce_ms);
        let keepalive = Duration::from_millis(self.limits.keepalive_ms);
        self.slots
            .values()
            .filter(|s| s.eligible && !s.in_flight)
            .flat_map(|s| {
                let pending = s.pending.then(|| s.last_push.map_or(now, |t| t + debounce));
                let kept = s.last_push.map(|t| t + keepalive);
                [pending, kept]
            })
            .flatten()
            .min()
    }
}

/// The stations that the task may send to now: the link is up, every live
/// link negotiated `desktop_shell.v1` and the last `station.info` says
/// `fleet_summary = true`.
fn eligible(store: &DeckStore) -> HashSet<StationId> {
    store
        .station_infos()
        .into_iter()
        .filter(|(id, (info, _))| {
            info.fleet_summary == Some(true)
                && store.is_reachable(id)
                && store.link_feature(id, feature::DESKTOP_SHELL) == LinkFeature::Negotiated
        })
        .map(|(id, _)| id)
        .collect()
}

/// Builds the summary of this moment. The board origin is read once.
async fn build_now(shared: &Shared) -> Result<FleetSummary, DeckError> {
    let store = &shared.store;
    let cards = store.sessions().await?;
    let records = store.stations().await?;
    let infos = store.station_infos();
    let stations: Vec<StationEntry> = records
        .into_iter()
        .map(|r| StationEntry {
            up: store.is_reachable(&r.station_id),
            cpu: infos
                .get(&r.station_id)
                .and_then(|(info, _)| info.resources.as_ref())
                .and_then(|res| res.cpu.clone()),
            id: r.station_id,
            name: r.name,
        })
        .collect();
    let now = store.now_ms();
    let board = pick_board(shared.endpoints.entries(now).into_iter().map(|e| e.origin));
    SummaryCounters::bump(&shared.counters.builds);
    Ok(build(&cards, &stations, now, board.as_deref()))
}

pub(super) async fn run(shared: Arc<Shared>, mut shutdown: watch::Receiver<bool>) {
    let mut board = shared.store.watch_board();
    let (done_tx, mut done_rx) = mpsc::unbounded_channel::<StationId>();
    let mut pushes = JoinSet::new();
    let mut schedule = Schedule::new(shared.limits);
    // The first pass sees the stations that are eligible at the start.
    let mut changed = true;
    loop {
        if *shutdown.borrow() {
            break;
        }
        schedule.observe(&eligible(&shared.store), changed);
        changed = false;
        let due = schedule.due(Instant::now());
        if !due.is_empty() {
            dispatch(&shared, &mut schedule, &mut pushes, &done_tx, due).await;
        }
        let wake = schedule.next_wake(Instant::now());
        let timer = async {
            match wake {
                Some(at) => tokio::time::sleep_until(at).await,
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            result = board.changed() => {
                if result.is_err() {
                    break;
                }
                changed = true;
            }
            Some(id) = done_rx.recv() => schedule.finish(&id),
            () = timer => {}
            result = shutdown.changed() => {
                if result.is_err() || *shutdown.borrow() {
                    break;
                }
            }
        }
        while pushes.try_join_next().is_some() {}
    }
    // A push in flight is dropped with the task; the stations keep their
    // last summary until the next one.
    pushes.shutdown().await;
}

/// Builds the summary once and starts one push task for each station in
/// `due`.
async fn dispatch(
    shared: &Arc<Shared>,
    schedule: &mut Schedule,
    pushes: &mut JoinSet<()>,
    done: &mpsc::UnboundedSender<StationId>,
    due: Vec<StationId>,
) {
    let summary = match build_now(shared).await {
        Ok(summary) => summary,
        Err(e) => {
            tracing::warn!(error = %e, "fleet summary build failed");
            for id in &due {
                SummaryCounters::bump(&shared.counters.failed);
                schedule.finish(id);
            }
            return;
        }
    };
    let params = match serde_json::to_value(&summary) {
        Ok(params) => params,
        Err(e) => {
            tracing::warn!(error = %e, "fleet summary serialization failed");
            for id in &due {
                SummaryCounters::bump(&shared.counters.failed);
                schedule.finish(id);
            }
            return;
        }
    };
    // The build awaited the store: the setting or the link may have changed.
    let still = eligible(&shared.store);
    for id in due {
        if !still.contains(&id) {
            schedule.finish(&id);
            continue;
        }
        let Some(control) = shared.link.control_for(&id) else {
            SummaryCounters::bump(&shared.counters.failed);
            schedule.finish(&id);
            continue;
        };
        pushes.spawn(push(
            Arc::clone(shared),
            id,
            control,
            params.clone(),
            done.clone(),
        ));
    }
}

/// Sends one summary and counts the outcome. Only the station and the wire
/// code are logged, never the summary.
async fn push(
    shared: Arc<Shared>,
    id: StationId,
    control: Control,
    params: serde_json::Value,
    done: mpsc::UnboundedSender<StationId>,
) {
    let wait = Duration::from_millis(shared.limits.timeout_ms);
    let counters = &shared.counters;
    match tokio::time::timeout(wait, control.request(METHOD, None, params)).await {
        Ok(Ok(Outcome::Ok(_))) => SummaryCounters::bump(&counters.sent),
        Ok(Ok(Outcome::Err(e))) => {
            SummaryCounters::bump(&counters.failed);
            tracing::debug!(station = %id, code = %e.code, "fleet summary refused");
        }
        Ok(Err(_)) => {
            SummaryCounters::bump(&counters.failed);
            tracing::debug!(station = %id, "fleet summary not sent: link error");
        }
        Err(_) => {
            SummaryCounters::bump(&counters.failed);
            tracing::debug!(station = %id, "fleet summary not answered in time");
        }
    }
    // The task ends when the channel is closed; there is nobody to tell then.
    let _ = done.send(id);
}
