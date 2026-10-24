//! The fleet summary that the deck pushes to the stations for the desktop
//! shell (R13 P4 task B1, `docs/v2/design/omarchy-p4.md` §3.1 and §3.2).
//!
//! - [`build`] is the pure builder of [`fleetdeck_proto::fleet_summary::FleetSummary`].
//!   The summary has no free-form text: no session title, no tool name, no
//!   request title, no chat text, no path (the sentinel test proves it).
//! - [`send`] is one serial task on `DeckStore::watch_board()`, built like
//!   [`crate::wake`]. A station is eligible when its link is up, it
//!   negotiated `desktop_shell.v1` and its last `station.info` says
//!   `fleet_summary = true`. The task sends `fleet.summary` to an eligible
//!   station at most once in `debounce_ms` (the latest state at the end of the
//!   window), when a link comes up, when a `station.info` turns the setting
//!   on, and as a keepalive. One request is in flight for each station.
//!   A failure (an `unsupported` answer, an error, a timeout) is counted in
//!   [`SummaryCounters`]; the next change sends again. The task writes no
//!   audit row: a push is not an owner action. It builds nothing when no
//!   station is eligible.

mod build;
mod send;
mod usage;

#[cfg(test)]
mod tests;

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::http::endpoints::Endpoints;
use crate::link::DeckLink;
use crate::store::DeckStore;

pub use build::{StationEntry, build, pick_board};

/// The limits of the sender. [`Limits::default`] holds the values of the
/// design; a test makes them smaller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// The shortest time between two pushes to one station (ms).
    pub debounce_ms: u64,
    /// A station gets a push at least this often (ms).
    pub keepalive_ms: u64,
    /// The wait for the answer of one push (ms).
    pub timeout_ms: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            debounce_ms: 2000,
            keepalive_ms: 60_000,
            timeout_ms: 5000,
        }
    }
}

/// How often each outcome happened since the task started.
#[derive(Debug, Default)]
pub struct SummaryCounters {
    /// The summaries that the task built (a test reads it: no eligible
    /// station means no build).
    pub builds: AtomicU64,
    /// A push that the station answered `Ok`.
    pub sent: AtomicU64,
    /// A push that failed: an error answer (`unsupported` included), a link
    /// error, a timeout, a missing link or a failed build.
    pub failed: AtomicU64,
}

impl SummaryCounters {
    fn bump(counter: &AtomicU64) {
        counter.fetch_add(1, Ordering::Relaxed);
    }

    /// The value of one counter.
    #[must_use]
    pub fn get(counter: &AtomicU64) -> u64 {
        counter.load(Ordering::Relaxed)
    }
}

/// Starts the sender task. It stops when `shutdown` turns true.
pub fn spawn(
    store: DeckStore,
    link: Arc<DeckLink>,
    endpoints: Arc<Endpoints>,
    shutdown: watch::Receiver<bool>,
) -> JoinHandle<()> {
    spawn_with(
        store,
        link,
        endpoints,
        shutdown,
        Limits::default(),
        Arc::new(SummaryCounters::default()),
    )
}

/// [`spawn`] with explicit limits and counters (a test reads the counters).
pub fn spawn_with(
    store: DeckStore,
    link: Arc<DeckLink>,
    endpoints: Arc<Endpoints>,
    shutdown: watch::Receiver<bool>,
    limits: Limits,
    counters: Arc<SummaryCounters>,
) -> JoinHandle<()> {
    let shared = Arc::new(send::Shared {
        store,
        link,
        endpoints,
        limits,
        counters,
    });
    tokio::spawn(send::run(shared, shutdown))
}
