//! The scan of the cards (R13 §6D item 5, §5E item 6, §5F item 4, §5G item 2).
//!
//! The cards carry `requested_at` but no `station_seq`. The boundary of
//! §5E item 6 is the `snapshot_mark` of the station: `apply_events` never
//! applies an event at or below the cursor, so after a snapshot every
//! applied event is above its watermark. A scan is in baseline mode for a
//! station when its mark changed since the last scan (or it is the first
//! scan after the deck started): a request that the scan sees then never
//! wakes. Every key whose `wake_reserve` answer is not `Reserved` is
//! remembered and never tried again (fail closed).

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

use fleetdeck_proto::{SessionId, StationId};
use tokio::sync::mpsc;

use super::job::{Job, ReqRef};
use super::{Shared, WakeCounters, last_component};
use crate::store::wake_seen::{WAKE_SUSPEND_MS, WakeReserve};
use crate::store::{DeckSession, SnapshotMark};

/// How often a scan reads the marks, the cards and the marks again before it
/// gives up (a snapshot landed in between).
const READ_TRIES: usize = 3;

type Marks = BTreeMap<StationId, Option<SnapshotMark>>;

/// A request on a card: the station, the session and the request id (G5).
type RequestKey = (StationId, SessionId, String);

/// A request that passed the checks of the wake rule and waits for a display.
struct Candidate {
    station: StationId,
    session: SessionId,
    request_id: String,
    requested_at: i64,
    /// The basename of the repository root of the session, if it has one.
    repo: Option<String>,
    mark: Option<SnapshotMark>,
}

/// True when `basename` passes the repository filter `filter`. A session
/// without a repository fails a repository filter (§5D item 17).
pub(super) fn repo_ok(filter: Option<&str>, basename: Option<&str>) -> bool {
    match filter {
        None => true,
        Some(f) => basename == Some(f),
    }
}

pub(super) struct Scanner {
    shared: Arc<Shared>,
    tx: mpsc::Sender<Job>,
    /// True until the first scan has finished: that scan is the baseline of
    /// the deck start.
    first: bool,
    /// The mark of each station at the last scan.
    marks: Marks,
    /// The requests that the task handled and that are still on a card.
    known: BTreeSet<RequestKey>,
    /// The start time (deck clock) of the last job of each monitor.
    cooldown: HashMap<(StationId, String), i64>,
    /// Until when a suppression warning is not repeated.
    warned_until: i64,
}

impl Scanner {
    pub(super) fn new(shared: Arc<Shared>, tx: mpsc::Sender<Job>) -> Self {
        Self {
            shared,
            tx,
            first: true,
            marks: Marks::new(),
            known: BTreeSet::new(),
            cooldown: HashMap::new(),
            warned_until: 0,
        }
    }

    fn counters(&self) -> &WakeCounters {
        &self.shared.counters
    }

    async fn read_marks(&self) -> Option<Marks> {
        let store = &self.shared.ctx.store;
        match store.stations().await {
            Ok(stations) => Some(
                stations
                    .into_iter()
                    .map(|s| {
                        let mark = store.snapshot_mark(&s.station_id);
                        (s.station_id, mark)
                    })
                    .collect(),
            ),
            Err(e) => {
                tracing::warn!(error = %e, "wake scan: stations");
                None
            }
        }
    }

    /// The marks and the cards of one moment: the marks must be the same
    /// before and after the cards are read, or a snapshot landed in between.
    async fn read(&self) -> Option<(Vec<DeckSession>, Marks)> {
        for _ in 0..READ_TRIES {
            let before = self.read_marks().await?;
            let cards = match self.shared.ctx.store.sessions().await {
                Ok(cards) => cards,
                Err(e) => {
                    tracing::warn!(error = %e, "wake scan: cards");
                    return None;
                }
            };
            if self.read_marks().await? == before {
                return Some((cards, before));
            }
        }
        tracing::debug!("wake scan: a snapshot landed during the read; the next change scans");
        None
    }

    /// One scan of the board.
    pub(super) async fn scan(&mut self) {
        let Some((cards, marks)) = self.read().await else {
            return;
        };
        let shared = Arc::clone(&self.shared);
        let store = &shared.ctx.store;
        let limits = shared.limits;
        let now = store.now_ms();
        let baseline: BTreeMap<&StationId, bool> = marks
            .iter()
            .map(|(station, mark)| {
                let changed = self.first
                    || match self.marks.get(station) {
                        Some(previous) => previous != mark,
                        None => mark.is_some(),
                    };
                (station, changed)
            })
            .collect();
        let mut present: BTreeSet<RequestKey> = BTreeSet::new();
        let mut candidates = Vec::new();
        for card in &cards {
            let station = &card.station_id;
            let mark = marks.get(station).cloned().flatten();
            let is_baseline = baseline.get(station).copied().unwrap_or(true);
            for request in &card.summary.open_requests {
                let request_id = request.request_id.as_str().to_owned();
                let session = &card.summary.session_id;
                let key = (station.clone(), session.clone(), request_id.clone());
                present.insert(key.clone());
                if !self.known.insert(key) {
                    continue;
                }
                let reserve = store
                    .wake_reserve(station.as_str(), session.as_str(), &request_id, now)
                    .await;
                self.note_reserve(reserve, now);
                if is_baseline {
                    WakeCounters::bump(&self.counters().baseline);
                    continue;
                }
                if reserve != WakeReserve::Reserved {
                    continue;
                }
                let age = now - request.requested_at;
                if age < -limits.skew_ms || age > limits.window_ms {
                    WakeCounters::bump(&self.counters().skip_age);
                    continue;
                }
                candidates.push(Candidate {
                    station: station.clone(),
                    session: session.clone(),
                    request_id,
                    requested_at: request.requested_at,
                    repo: card.summary.repo.as_ref().map(|r| last_component(&r.root)),
                    mark: mark.clone(),
                });
            }
        }
        drop(baseline);
        self.marks = marks;
        self.first = false;
        self.known.retain(|k| present.contains(k));
        self.dispatch(candidates, now).await;
        WakeCounters::bump(&self.counters().scans);
    }

    fn note_reserve(&mut self, reserve: WakeReserve, now: i64) {
        let counter = match reserve {
            WakeReserve::Reserved => &self.counters().reserved,
            WakeReserve::Seen => &self.counters().seen,
            WakeReserve::Suppressed => &self.counters().suppressed,
            WakeReserve::Error => &self.counters().reserve_error,
        };
        WakeCounters::bump(counter);
        if reserve == WakeReserve::Suppressed && now >= self.warned_until {
            tracing::warn!("the wake table is saturated; wakes are suppressed for 120 s");
            self.warned_until = now + WAKE_SUSPEND_MS;
        }
    }

    /// Joins the candidates into one job for each bound `(station, monitor)`
    /// and queues them.
    async fn dispatch(&mut self, candidates: Vec<Candidate>, now: i64) {
        if candidates.is_empty() {
            return;
        }
        let ctx = &self.shared.ctx;
        if !ctx.live.get().desktop_control {
            for _ in &candidates {
                WakeCounters::bump(&self.counters().skip_control);
            }
            return;
        }
        let displays = match ctx.store.display_list().await {
            Ok(displays) => displays,
            Err(e) => {
                tracing::warn!(error = %e, "wake scan: displays");
                return;
            }
        };
        let mut jobs: BTreeMap<(StationId, String), Job> = BTreeMap::new();
        let mut taken = vec![false; candidates.len()];
        for display in displays.iter().filter(|d| d.wake_on_needs_you) {
            let Some(binding) = &display.binding else {
                continue;
            };
            let scope = match ctx.store.display_scope(display.display_id.clone()).await {
                Ok(Some(scope)) => scope,
                Ok(None) => continue,
                Err(e) => {
                    tracing::warn!(error = %e, "wake scan: display scope");
                    continue;
                }
            };
            for (i, c) in candidates.iter().enumerate() {
                if c.station != binding.station_id
                    || !scope.stations.contains(&c.station)
                    || !repo_ok(scope.repo.as_deref(), c.repo.as_deref())
                {
                    continue;
                }
                let job = jobs
                    .entry((binding.station_id.clone(), binding.monitor.clone()))
                    .or_insert_with(|| Job {
                        station: binding.station_id.clone(),
                        monitor: binding.monitor.clone(),
                        display_id: display.display_id.clone(),
                        requests: Vec::new(),
                        mark: c.mark.clone(),
                    });
                // The first display that takes a request owns the job.
                if job.display_id == display.display_id {
                    job.requests.push(ReqRef {
                        session: c.session.clone(),
                        request_id: c.request_id.clone(),
                        requested_at: c.requested_at,
                    });
                    taken[i] = true;
                }
            }
        }
        for taken in taken {
            if !taken {
                WakeCounters::bump(&self.counters().skip_no_display);
            }
        }
        let cooldown_ms = self.shared.limits.cooldown_ms;
        self.cooldown.retain(|_, at| now - *at < cooldown_ms);
        for (key, job) in jobs {
            if self.cooldown.contains_key(&key) {
                WakeCounters::bump(&self.counters().skip_cooldown);
                continue;
            }
            self.cooldown.insert(key.clone(), now);
            if self.tx.try_send(job).is_err() {
                // A dropped job did not wake: it does not start a cooldown.
                self.cooldown.remove(&key);
                WakeCounters::bump(&self.counters().queue_full);
                tracing::warn!(station = %key.0, monitor = %key.1, "wake queue full; job dropped");
            }
        }
    }
}
