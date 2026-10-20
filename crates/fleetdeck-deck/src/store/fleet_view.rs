//! The fleet view (MCP P3, design `docs/v2/design/mcp-p3.md` §2.2, §2.3):
//! the deck side of the link method `fleet.view` (feature `fleet_view.v1`).
//!
//! Every session of a paired station can read the stations, the projects,
//! the live sessions (titles and states only) and itself, with or without a
//! grant. The rules run in this order, and each refusal is a link error:
//!
//! 1. the link layer checked the key, the role, the scope and the feature;
//! 2. the deck-wide concurrency ([`BUSY_PERMITS`], no queue);
//! 3. the station rate limit, counted also for refused requests;
//! 4. the caller session exists on the calling station (the read model);
//! 5. the session rate limit, counted after step 4;
//! 6. the page, built from the projection cache of the view (the projection
//!    is rebuilt at most once per view every [`CACHE_TTL`]);
//! 7. no audit row, one debug line, and never a write to `last_used_at`.
//!
//! The projections are in `fleet_view_build.rs`.

use std::collections::HashMap;
use std::hash::Hash;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use fleetdeck_proto::fleet_view::{
    FleetView, FleetViewPage, FleetViewRequest, PAGE_MAX_BYTES, PROJECTS_PAGE, SESSIONS_PAGE,
    STATIONS_PAGE, decode_cursor, encode_cursor,
};
use fleetdeck_proto::{SessionId, StationId};
use tokio::sync::Semaphore;

use super::fleet_view_build::Entry;
use super::{DeckError, DeckStore, call, coordinator_ops};

/// The most fleet views that the deck builds at the same time (step 2).
pub const BUSY_PERMITS: usize = 8;
/// The most views per station per minute (step 3).
pub const STATION_LIMIT: usize = 240;
/// The most views per `(station, caller)` per minute (step 5).
pub const SESSION_LIMIT: usize = 60;
/// The length of a rate window.
pub const WINDOW_MS: i64 = 60_000;
/// The most keys of a limiter map.
pub const MAX_KEYS: usize = 4096;
/// The age at which a projection is rebuilt (step 6).
pub const CACHE_TTL: Duration = Duration::from_secs(2);

/// The refusal of step 2.
pub const BUSY: (&str, &str) = ("busy", "the fleet view is busy, try again in a moment");
/// The refusal of step 3.
pub const STATION_LIMITED: (&str, &str) = (
    "rate_limited",
    "this station asked for too many fleet views",
);
/// The refusal of step 4 (and of a `me` view of an unlisted session).
pub const NOT_KNOWN: (&str, &str) = ("forbidden", "the session is not known on this station");
/// The refusal of step 5.
pub const SESSION_LIMITED: (&str, &str) = ("rate_limited", "at most 60 fleet views per minute");
/// The refusal of a cursor that is not valid.
pub const BAD_CURSOR: (&str, &str) = ("invalid_params", "the cursor is not valid");
/// The refusal of params that do not parse.
pub const BAD_PARAMS: (&str, &str) = ("invalid_params", "the fleet view request is not valid");

/// Why a fleet view is not answered.
#[derive(Debug)]
pub enum FleetViewError {
    /// A refusal: the link error code and its exact text.
    Refused(&'static str, &'static str),
    /// A store error.
    Store(DeckError),
}

impl From<DeckError> for FleetViewError {
    fn from(e: DeckError) -> Self {
        Self::Store(e)
    }
}

/// The sliding windows of one limiter: the timestamps (Unix ms) of the
/// admitted views per key, in memory only.
#[derive(Debug)]
pub(super) struct Windows<K>(Mutex<HashMap<K, std::collections::VecDeque<i64>>>);

impl<K> Default for Windows<K> {
    fn default() -> Self {
        Self(Mutex::default())
    }
}

impl<K: Hash + Eq + Clone> Windows<K> {
    /// Records a view of `key` at `now`; false when the window of `key`
    /// holds `limit` views already. The entries older than the window are
    /// removed first; a map holds at most [`MAX_KEYS`] keys (above it, the
    /// key with the oldest last view is removed).
    pub(super) fn admit(&self, key: &K, now: i64, limit: usize) -> bool {
        let mut map = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        map.retain(|_, q| q.back().is_some_and(|t| *t > now - WINDOW_MS));
        if !map.contains_key(key) && map.len() >= MAX_KEYS {
            let oldest = map
                .iter()
                .min_by_key(|(_, q)| q.back().copied().unwrap_or(i64::MIN))
                .map(|(k, _)| k.clone());
            if let Some(oldest) = oldest {
                map.remove(&oldest);
            }
        }
        let q = map.entry(key.clone()).or_default();
        while q.front().is_some_and(|t| *t <= now - WINDOW_MS) {
            q.pop_front();
        }
        if q.len() >= limit {
            return false;
        }
        q.push_back(now);
        true
    }

    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).len()
    }
}

/// The complete, sorted, field-limited projection of one view.
#[derive(Debug, Default)]
struct Cache {
    slot: tokio::sync::Mutex<Option<(Instant, Arc<Vec<Entry>>)>>,
    rebuilds: AtomicU64,
}

/// The state of the fleet view in the store: the limiters and the caches.
#[derive(Debug)]
pub(super) struct FleetViewState {
    busy: Semaphore,
    stations: Windows<StationId>,
    sessions: Windows<(StationId, SessionId)>,
    cache_stations: Cache,
    cache_projects: Cache,
    cache_sessions: Cache,
}

impl Default for FleetViewState {
    fn default() -> Self {
        Self {
            busy: Semaphore::new(BUSY_PERMITS),
            stations: Windows::default(),
            sessions: Windows::default(),
            cache_stations: Cache::default(),
            cache_projects: Cache::default(),
            cache_sessions: Cache::default(),
        }
    }
}

impl FleetViewState {
    fn cache(&self, view: FleetView) -> Option<&Cache> {
        match view {
            FleetView::Stations => Some(&self.cache_stations),
            FleetView::Projects => Some(&self.cache_projects),
            FleetView::Sessions => Some(&self.cache_sessions),
            FleetView::Me => None,
        }
    }
}

const fn page_size(view: FleetView) -> usize {
    match view {
        FleetView::Stations => STATIONS_PAGE,
        FleetView::Projects => PROJECTS_PAGE,
        FleetView::Sessions | FleetView::Me => SESSIONS_PAGE,
    }
}

/// The serialized bytes of `{"items":[` and of `],"next_cursor":` and `}`.
const PAGE_FIXED_BYTES: usize = 10 + 16 + 1;

/// The page of `view` that starts after `after` (the decoded cursor key),
/// from the projection `entries` (sorted by key). Items are added in order
/// while the whole serialized response stays within [`PAGE_MAX_BYTES`];
/// `next_cursor` is set when matching items remain.
fn page_of(
    view: FleetView,
    entries: &[Entry],
    after: Option<&[String]>,
    station: Option<&StationId>,
) -> FleetViewPage {
    let start = after.map_or(0, |key| entries.partition_point(|e| e.key.as_slice() <= key));
    let rest: Vec<&Entry> = entries[start..]
        .iter()
        .filter(|e| station.is_none_or(|s| e.stations.contains(s)))
        .collect();
    let mut picked: Vec<&Entry> = Vec::new();
    let mut items_bytes = 0_usize;
    for (i, entry) in rest.iter().enumerate().take(page_size(view)) {
        let cursor_bytes = if i + 1 == rest.len() {
            "null".len()
        } else {
            encode_cursor(&entry.key).len() + 2
        };
        let commas = usize::from(!picked.is_empty());
        let total = PAGE_FIXED_BYTES + items_bytes + commas + entry.len + cursor_bytes;
        if total > PAGE_MAX_BYTES {
            break;
        }
        items_bytes += commas + entry.len;
        picked.push(entry);
    }
    let next_cursor = (picked.len() < rest.len())
        .then(|| picked.last().map(|e| encode_cursor(&e.key)))
        .flatten();
    FleetViewPage {
        items: serde_json::Value::Array(picked.iter().map(|e| e.value.clone()).collect()),
        next_cursor,
    }
}

impl DeckStore {
    /// `fleet.view` of `station` (design §2.2). `params` are the params of
    /// the request, parsed after the station limit.
    ///
    /// # Errors
    /// [`FleetViewError::Refused`] with the link error code and text of the
    /// first rule that fails; [`FleetViewError::Store`] for a database error.
    pub async fn fleet_view(
        &self,
        station: &StationId,
        params: serde_json::Value,
    ) -> Result<FleetViewPage, FleetViewError> {
        let state = &self.inner.fleet_view;
        // 2. The deck-wide concurrency, at once.
        let Ok(_permit) = state.busy.try_acquire() else {
            return Err(FleetViewError::Refused(BUSY.0, BUSY.1));
        };
        // 3. The station limit, counted also for a refused request.
        if !state.stations.admit(station, self.now_ms(), STATION_LIMIT) {
            return Err(FleetViewError::Refused(STATION_LIMITED.0, STATION_LIMITED.1));
        }
        let Ok(request) = serde_json::from_value::<FleetViewRequest>(params) else {
            return Err(FleetViewError::Refused(BAD_PARAMS.0, BAD_PARAMS.1));
        };
        // 4. The caller is a session of the calling station.
        let (st, caller) = (station.clone(), request.caller.clone());
        let known = call(&self.inner.db, move |conn| {
            coordinator_ops::session_known(conn, &st, &caller)
        })
        .await?;
        if !known {
            return Err(FleetViewError::Refused(NOT_KNOWN.0, NOT_KNOWN.1));
        }
        // 5. The session limit, counted after step 4.
        let key = (station.clone(), request.caller.clone());
        if !state.sessions.admit(&key, self.now_ms(), SESSION_LIMIT) {
            return Err(FleetViewError::Refused(SESSION_LIMITED.0, SESSION_LIMITED.1));
        }
        // 7. A debug line: the station, the caller and the view only.
        tracing::debug!(
            station = %station,
            caller = %request.caller,
            view = request.view.name(),
            "fleet.view"
        );
        // 6. The page.
        if request.view == FleetView::Me {
            let me = self.build_me(station, &request.caller).await?;
            let Some(me) = me else {
                return Err(FleetViewError::Refused(NOT_KNOWN.0, NOT_KNOWN.1));
            };
            return Ok(FleetViewPage {
                items: me,
                next_cursor: None,
            });
        }
        let after = match request.cursor.as_deref() {
            Some(cursor) => Some(
                decode_cursor(request.view, cursor)
                    .map_err(|_| FleetViewError::Refused(BAD_CURSOR.0, BAD_CURSOR.1))?,
            ),
            None => None,
        };
        let entries = self.projection(request.view).await?;
        let station_filter = match request.view {
            FleetView::Projects => request.station.as_ref(),
            _ => None,
        };
        Ok(page_of(
            request.view,
            &entries,
            after.as_deref(),
            station_filter,
        ))
    }

    /// The projection of `view`, rebuilt under the cache lock when it is
    /// older than [`CACHE_TTL`]; concurrent requests wait for that rebuild.
    async fn projection(&self, view: FleetView) -> Result<Arc<Vec<Entry>>, DeckError> {
        let Some(cache) = self.inner.fleet_view.cache(view) else {
            return Ok(Arc::default());
        };
        let mut slot = cache.slot.lock().await;
        if let Some((built, entries)) = slot.as_ref()
            && built.elapsed() < CACHE_TTL
        {
            return Ok(Arc::clone(entries));
        }
        let entries = Arc::new(match view {
            FleetView::Stations => self.build_stations().await?,
            FleetView::Projects => self.build_projects().await?,
            FleetView::Sessions | FleetView::Me => self.build_sessions().await?,
        });
        cache.rebuilds.fetch_add(1, Ordering::SeqCst);
        *slot = Some((Instant::now(), Arc::clone(&entries)));
        Ok(entries)
    }

    /// How many times the projection of `view` was rebuilt (tests).
    #[cfg(test)]
    pub(crate) fn fleet_view_rebuilds(&self, view: FleetView) -> u64 {
        self.inner
            .fleet_view
            .cache(view)
            .map_or(0, |c| c.rebuilds.load(Ordering::SeqCst))
    }

    /// Drops the three projections, so the next view rebuilds them (tests).
    #[cfg(test)]
    pub(crate) async fn fleet_view_expire(&self) {
        let state = &self.inner.fleet_view;
        for cache in [&state.cache_stations, &state.cache_projects, &state.cache_sessions] {
            *cache.slot.lock().await = None;
        }
    }

    /// The keys of the limiter maps: stations, sessions (tests).
    #[cfg(test)]
    pub(crate) fn fleet_view_limiter_keys(&self) -> (usize, usize) {
        let state = &self.inner.fleet_view;
        (state.stations.len(), state.sessions.len())
    }

    /// Takes every permit of the busy semaphore (tests).
    #[cfg(test)]
    #[allow(clippy::expect_used)]
    pub(crate) fn fleet_view_hold_all_permits(&self) -> tokio::sync::SemaphorePermit<'_> {
        self.inner
            .fleet_view
            .busy
            .try_acquire_many(u32::try_from(BUSY_PERMITS).unwrap_or(u32::MAX))
            .expect("the permits are free")
    }
}
