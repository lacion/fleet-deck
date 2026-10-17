//! The wake rule of the wall (R13 §6.6, §6D items 3 to 5, 9 and 10, §5E items
//! 5 and 6, §5F item 4, §5G item 2).
//!
//! One serial task scans the cards on each change of the board
//! ([`scan`]). A request that the task sees for the first time is recorded
//! with [`crate::store::DeckStore::wake_reserve`]. In baseline mode (the
//! first scan after the deck starts, and the scan after a new snapshot of a
//! station) the request never wakes. In live mode only `Reserved` leads to a
//! candidate, and only when the request is at most 60 s old (5 s future
//! skew). A candidate becomes one job for each bound `(station, monitor)`,
//! with a cooldown of 30 s. At most four workers run the jobs ([`job`]): a
//! fresh `desktop.info`, the monitor state, the quiet hours ([`quiet`]), a
//! last check that the request is still open, and then
//! [`crate::desktop_relay::act`] with [`crate::desktop_relay::Actor::Wake`].
//! The relay writes the audit row. A routine skip is a counter
//! ([`WakeCounters`]), not an audit row. The wake never retries an action.

mod job;
mod quiet;
mod scan;

#[cfg(test)]
mod tests;

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use tokio::sync::{Mutex, mpsc, watch};
use tokio::task::{JoinHandle, JoinSet};

use crate::desktop_relay::RelayCtx;
use crate::link::DeckLink;
use crate::live::LiveCell;
use crate::store::DeckStore;

pub use quiet::{allows_wake, fresh_offset, inside as quiet_inside};

/// How long the shutdown waits for the workers before it aborts them.
const WORKER_GRACE: Duration = Duration::from_secs(5);

/// The limits of the wake. [`Limits::default`] holds the values of the
/// design; a test makes them smaller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// A request is a candidate when it is at most this old (ms).
    pub window_ms: i64,
    /// A request may be this far in the future (ms).
    pub skew_ms: i64,
    /// The time after a job for a `(station, monitor)` during which the next
    /// one is skipped (ms).
    pub cooldown_ms: i64,
    /// The number of workers. `0` runs no worker (a test).
    pub workers: usize,
    /// The jobs that wait for a worker. A full queue drops the job.
    pub queue: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            window_ms: 60_000,
            skew_ms: 5_000,
            cooldown_ms: 30_000,
            workers: 4,
            queue: 32,
        }
    }
}

/// How often each outcome happened since the task started. These are the
/// counters of §6D item 10: a routine skip is counted, not audited.
#[derive(Debug, Default)]
pub struct WakeCounters {
    /// The scans that finished (a test waits on it).
    pub scans: AtomicU64,
    /// A request recorded in baseline mode.
    pub baseline: AtomicU64,
    /// `wake_reserve` gave `Reserved`.
    pub reserved: AtomicU64,
    /// `wake_reserve` gave `Seen`.
    pub seen: AtomicU64,
    /// `wake_reserve` gave `Suppressed`.
    pub suppressed: AtomicU64,
    /// `wake_reserve` gave `Error`.
    pub reserve_error: AtomicU64,
    /// A reserved request outside the age window.
    pub skip_age: AtomicU64,
    /// `desktop_control` was off.
    pub skip_control: AtomicU64,
    /// A candidate that no bound display could take.
    pub skip_no_display: AtomicU64,
    /// A job inside the cooldown of its monitor.
    pub skip_cooldown: AtomicU64,
    /// The display changed before the send (binding, switch or filter).
    pub skip_display: AtomicU64,
    /// `desktop.info` failed.
    pub skip_info: AtomicU64,
    /// The monitor is not named, not off, or its state is unknown.
    pub skip_monitor: AtomicU64,
    /// Quiet hours, or an offset that is unknown or stale.
    pub skip_quiet: AtomicU64,
    /// No request of the job was still open and in the filter.
    pub skip_closed: AtomicU64,
    /// A snapshot of the station arrived between the scan and the send.
    pub skip_raced: AtomicU64,
    /// A job that was too old when a worker took it.
    pub expired: AtomicU64,
    /// A job that the full queue dropped.
    pub queue_full: AtomicU64,
    /// `desktop_relay::act` answered `Ok`.
    pub sent_ok: AtomicU64,
    /// `desktop_relay::act` answered an error (a timeout included).
    pub sent_failed: AtomicU64,
}

impl WakeCounters {
    fn bump(counter: &AtomicU64) {
        counter.fetch_add(1, Ordering::Relaxed);
    }

    /// The value of one counter.
    #[must_use]
    pub fn get(counter: &AtomicU64) -> u64 {
        counter.load(Ordering::Relaxed)
    }
}

/// What the scan and the workers share.
pub(crate) struct Shared {
    pub ctx: RelayCtx,
    pub limits: Limits,
    pub counters: Arc<WakeCounters>,
}

/// The last path component of a path on a station (any separator): the
/// displayed basename of a repository root.
pub(crate) fn last_component(root: &str) -> String {
    root.trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or_default()
        .to_owned()
}

/// Starts the wake task. It stops when `shutdown` turns true.
pub fn spawn(
    store: DeckStore,
    link: Arc<DeckLink>,
    live: LiveCell,
    shutdown: watch::Receiver<bool>,
) -> JoinHandle<()> {
    spawn_with(
        store,
        link,
        live,
        shutdown,
        Limits::default(),
        Arc::new(WakeCounters::default()),
    )
}

/// [`spawn`] with explicit limits and counters (a test reads the counters).
pub fn spawn_with(
    store: DeckStore,
    link: Arc<DeckLink>,
    live: LiveCell,
    shutdown: watch::Receiver<bool>,
    limits: Limits,
    counters: Arc<WakeCounters>,
) -> JoinHandle<()> {
    let shared = Arc::new(Shared {
        ctx: RelayCtx { store, link, live },
        limits,
        counters,
    });
    tokio::spawn(run(shared, shutdown))
}

async fn run(shared: Arc<Shared>, mut shutdown: watch::Receiver<bool>) {
    let (tx, rx) = mpsc::channel(shared.limits.queue.max(1));
    let rx = Arc::new(Mutex::new(rx));
    let mut workers = JoinSet::new();
    for _ in 0..shared.limits.workers {
        workers.spawn(worker(Arc::clone(&shared), Arc::clone(&rx)));
    }
    let mut board = shared.ctx.store.watch_board();
    let mut scanner = scan::Scanner::new(Arc::clone(&shared), tx);
    // The first scan is the baseline of the deck start.
    loop {
        if *shutdown.borrow() {
            break;
        }
        scanner.scan().await;
        tokio::select! {
            changed = board.changed() => {
                if changed.is_err() {
                    break;
                }
            }
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    break;
                }
            }
        }
    }
    // The queue closes with the scanner; a worker ends after its job.
    drop(scanner);
    let drained = tokio::time::timeout(WORKER_GRACE, async {
        while workers.join_next().await.is_some() {}
    })
    .await;
    if drained.is_err() {
        tracing::warn!("wake workers did not stop in time; aborting them");
        workers.abort_all();
    }
}

async fn worker(shared: Arc<Shared>, rx: Arc<Mutex<mpsc::Receiver<job::Job>>>) {
    loop {
        let next = rx.lock().await.recv().await;
        match next {
            Some(job) => job::run(&shared, job).await,
            None => return,
        }
    }
}
