//! The deck store: `deck.db` (PLAN §7, §10).
//!
//! - [`stations`]: the station registry with pins, grants and the
//!   revocation generation (G5, one transaction per change).
//! - [`read_model`]: the session read model and the condensed timeline,
//!   applied from the station event streams with the resync cursor, the
//!   snapshot batch and the incarnation rules (G4).
//!
//! - [`commands`]: the command router's stored envelopes and states (G3).
//! - [`outbox`]: deck-owned intent delivered as routed commands (A5).
//! - [`coordinator`]: the receipts of `coordinator.call` (G7).
//! - [`auth`]: the owner, login sessions, login links and API tokens
//!   (A13, L15); [`audit`]: the audit log of every mutation (§3 rule 7).
//!
//! Station reachability is link state, not stored state: the deck keeps it
//! in memory and the board status projection reads it (§7.2).

pub mod activity;
pub mod archive;
pub mod artifact_tombstones;
pub mod artifacts;
mod assist;
pub mod audit;
pub mod auth;
pub mod board_fence;
pub mod branch_map;
pub mod chat;
pub mod commands;
pub mod conflicts;
pub mod coordinator;
pub mod coordinator_desktop;
pub(crate) mod coordinator_fleet;
mod coordinator_git;
mod coordinator_input_any_schema;
mod coordinator_input_schema;
pub(crate) mod coordinator_ops;
pub mod coordinator_query;
mod coordinator_query_reads;
mod coordinator_receipt_grant_schema;
mod coordinator_recording_schema;
pub mod desktop_captures;
pub(crate) mod desktop_recordings;
pub mod display_directive_schema;
pub mod display_directives;
pub mod display_directives_coordinator;
pub mod display_directives_station;
#[cfg(test)]
mod display_gen_tests;
pub mod display_v25;
pub mod displays;
pub mod displays_auth;
pub mod displays_binding;
pub mod displays_pairing;
pub mod extras;
pub mod fleet;
pub mod fleet_git_prefs;
pub mod fleet_git_schema;
pub mod fleet_git_start;
pub mod fleet_view;
mod fleet_view_build;
#[cfg(test)]
mod fleet_view_paging_tests;
#[cfg(test)]
mod fleet_view_tests;
pub mod git_snapshots;
pub mod grant_scopes;
mod grant_spawn_caps;
pub mod grants;
mod grants_input;
mod grants_lifecycle;
pub mod home_attempts;
#[cfg(test)]
mod knobs_age_tests;
mod knobs_store;
mod land_actions;
mod land_admit;
pub mod land_outbox;
pub mod land_queue;
mod land_ready;
pub mod land_text;
#[cfg(test)]
mod land_world;
pub mod members;
pub mod outbox;
pub mod pairing;
pub mod plans;
pub mod presence;
pub mod project_home_ops;
pub mod project_homes;
pub mod project_sessions;
pub mod projects;
pub mod projects_git;
pub(crate) mod projects_legacy;
pub mod provision_reports;
pub mod radar;
pub mod read_cursors;
pub mod read_model;
mod recording_length_schema;
pub mod review;
pub mod review_anchor;
mod review_edit;
pub mod review_send;
pub mod review_text;
pub mod schedules;
pub mod schema;
mod session_idle_schema;
pub mod session_moves;
mod session_moves_ready;
pub mod session_moves_steps;
pub mod shares;
pub mod slug_reservations;
pub mod station_machines;
pub mod station_palettes;
pub mod stations;
pub mod wake_seen;
pub mod worker_grant;

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use fleetdeck_core::clock::SharedClock;
use fleetdeck_core::db::{Db, DbError};
use fleetdeck_core::ids::{SharedIds, new_id};
use fleetdeck_proto::link::StationInfo;
use fleetdeck_proto::{EventEnvelope, IncarnationId, StationId};
use rusqlite::{Connection, OptionalExtension};
use tokio::sync::{broadcast, watch};

pub use audit::{AuditEntry, AuditResult, NewAudit};
pub use auth::{ApiToken, LoginMethod, LoginSession, NewLogin, Principal};
pub use commands::{DeckCommand, NewCommand, Settle};
pub use coordinator_query::{QueryAnswer, ReadShape, StationRead};
pub use displays::DisplayLogin;
pub use fleet::{Completed, NewMail, NewTask};
pub use grants::{Grant, GrantClass, GrantState, NewGrant};
pub use members::{MemberAction, MemberId, ShareGrant, ShareScope, member_allows};
pub use outbox::{NewOutboxItem, OutboxItem, OutboxState};
pub use presence::SocialEvent;
pub use read_model::{DeckSession, TIMELINE_CAP};
pub use schedules::StoredSchedule;
pub use stations::{Direction, StationRecord};

/// What the deck knows about `coordinator.v2` on the links of a station
/// (ADR-008 §3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkV2 {
    /// No live link.
    Down,
    /// A live link, and at least one live link did not negotiate it.
    Without,
    /// Every live link negotiated it.
    Negotiated,
}

/// Whether every live link of a station negotiated a feature.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkFeature {
    /// No live link.
    Down,
    /// Every live link negotiated the feature.
    Negotiated,
    /// A live link did not negotiate the feature.
    Without,
}

#[derive(Debug, thiserror::Error)]
pub enum DeckError {
    #[error(transparent)]
    Db(#[from] DbError),
    #[error("stored JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("unknown station {0}")]
    UnknownStation(String),
    #[error("a station named `{0}` exists already")]
    NameTaken(String),
    #[error("a deck pairs only with stations")]
    NotAStation,
    #[error(
        "station {station} sent events of incarnation {got}, the deck holds {expected:?}; a snapshot is required"
    )]
    IncarnationMismatch {
        station: StationId,
        expected: Option<IncarnationId>,
        got: IncarnationId,
    },
    #[error("the deck has not synced with station {0} yet; try again when it is reachable")]
    NotSynced(String),
    #[error("unknown command {0}")]
    UnknownCommand(String),
    #[error("command id {0} is in use for another command")]
    CommandIdReused(String),
    #[error("{0}")]
    Invalid(String),
    #[error("unknown plan {0}")]
    UnknownPlan(String),
    #[error("unknown schedule {0}")]
    UnknownSchedule(String),
    #[error("password hashing: {0}")]
    Hash(String),
    #[error("corrupt store: {0}")]
    Corrupt(String),
    #[error("a stale snapshot: the deck holds a newer state of this station")]
    StaleSnapshot,
    #[error("the link of this snapshot is no longer the current link of the station")]
    Superseded,
    /// A limit is full (R13 §5B: pairings); the caller tries again later.
    #[error("{0}")]
    Busy(String),
    /// A rule of the homes of a project refuses a write (FG-5): `use_homes`,
    /// `home_busy`.
    #[error("{}", .0.message)]
    HomeRefused(project_homes::HomeRefusal),
    /// A browser session was not created because its address is exposed to
    /// an input operation (R13 P7 §7.6 rule 2). Nothing was inserted.
    #[error("sign-in is blocked while an agent types on the machine of this address")]
    SignInBlocked,
}

impl From<rusqlite::Error> for DeckError {
    fn from(e: rusqlite::Error) -> Self {
        Self::Db(DbError::Sqlite(e))
    }
}

pub type Result<T, E = DeckError> = std::result::Result<T, E>;

/// Batches of folded events that a slow board WebSocket may lag behind;
/// it then catches up from the stored timeline.
const LIVE_BUFFER: usize = 256;

/// Events of one station that one transaction folded (also the streaming
/// deltas that the condensed timeline drops).
#[derive(Debug, Clone, PartialEq)]
pub struct LiveEvents {
    pub station_id: StationId,
    pub incarnation: IncarnationId,
    pub events: Vec<EventEnvelope>,
}

struct Inner {
    db: Db,
    clock: SharedClock,
    ids: SharedIds,
    incarnation: IncarnationId,
    /// Counts command state changes (the router waits on it).
    commands: watch::Sender<u64>,
    /// Counts changes of the board view: cards, stations, reachability
    /// (the board WebSocket sends a delta after each, P3-9b).
    board: watch::Sender<u64>,
    /// The events folded into the read model, after their commit.
    live: broadcast::Sender<Arc<LiveEvents>>,
    /// The stations with a live link (§7.2 `unreachable`).
    reachable: Mutex<BTreeSet<StationId>>,
    /// The last `station.info` of each station with its Unix ms, in this
    /// deck process (M2 P4-2: version, capacity, providers).
    infos: Mutex<BTreeMap<StationId, (StationInfo, i64)>>,
    /// Whether two sessions that change one file get a mail (DL-145).
    conflict_mail: AtomicBool,
    /// The owner knobs (a settings publish writes them).
    knobs: std::sync::RwLock<crate::knobs::DeckKnobs>,
    /// The deck setting `fleet_control` (ADR-008 §3): every fleet-control
    /// decision and send check reads it here, so it is always current.
    fleet_control: AtomicBool,
    /// The features that each live link of a station negotiated, by link
    /// token (ADR-008 §3 "every send needs v2"); in memory only.
    link_features: Mutex<BTreeMap<StationId, BTreeMap<u64, BTreeSet<String>>>>,
    /// The remote address of each live link of a station, by link token; in
    /// memory only (the devices list reads it).
    link_peers: Mutex<BTreeMap<StationId, BTreeMap<u64, std::net::IpAddr>>>,
    /// Presence, chat and mob-driver updates for the board sockets (R11).
    social: broadcast::Sender<Arc<presence::SocialEvent>>,
    /// Who has a session open now (R11.5); in memory only.
    presence: Mutex<presence::PresenceMap>,
    /// The next id of a board socket.
    sockets: std::sync::atomic::AtomicU64,
    /// The conflict mails that wait for a generated sentence (DL-167).
    sentence_jobs: Mutex<Vec<conflicts::SentenceJob>>,
    /// The `ask_coordinator` calls of the workers in the last hour (R2.3).
    ask_limits: coordinator_fleet::AskLimiter,
    /// The limiters and the projection caches of the fleet view (MCP P3).
    fleet_view: fleet_view::FleetViewState,
    /// The stations whose derived `coordinate` scope changed and whose link
    /// cache entry is not refreshed yet (`grant_scopes`).
    scope_dirty: Mutex<BTreeSet<StationId>>,
    /// Counts the notes in `scope_dirty` (the follower of the link cache
    /// waits on it).
    scope_changes: watch::Sender<u64>,
    /// The last snapshot that each station applied, in this deck process
    /// (the wake rule reads it, R13 §5E item 6).
    snapshot_marks: Mutex<BTreeMap<StationId, SnapshotMark>>,
    /// The board sign-out on the target machine (R13 P7 §7.6): the socket
    /// registry, the cache of the exposed addresses and the local station.
    fence: crate::board_fence::FenceState,
}

/// The last snapshot batch that the read model applied for a station: the
/// `station_seq` watermark of the batch is the initialization boundary of the
/// wake rule (R13 §5E item 6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotMark {
    pub incarnation: IncarnationId,
    /// The `station_seq` at which the snapshot was taken.
    pub watermark: u64,
    /// Unix ms at which the deck applied it.
    pub applied_at_ms: i64,
}

impl Inner {
    /// The deck setting `fleet_control` and the `coordinator.v2` state of
    /// every station with a live link, right now. A database closure calls
    /// it, so a decision reads the values of its own turn.
    pub(crate) fn fleet_env(&self) -> coordinator::FleetEnv {
        let links = self
            .link_features
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        coordinator::FleetEnv {
            fleet_control: self.fleet_control.load(Ordering::SeqCst),
            links: links
                .iter()
                .map(|(s, l)| (s.clone(), v2_state(l)))
                .collect(),
            // The land feature of each station (FG-10: a coordinator land).
            land: feature_states(&links, fleetdeck_proto::version::feature::FLEET_GIT_LAND),
            // The move feature of each station (FG-9: a coordinator move).
            moves: feature_states(&links, fleetdeck_proto::version::feature::FLEET_GIT_MOVE),
        }
    }
}

/// The deck store. Clones share one database thread.
#[derive(Clone)]
pub struct DeckStore {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for DeckStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeckStore")
            .field("db", &self.inner.db)
            .field("incarnation", &self.inner.incarnation)
            .finish_non_exhaustive()
    }
}

impl DeckStore {
    /// Opens (or creates) `deck.db`. A new database gets a new incarnation.
    ///
    /// # Errors
    /// The database cannot open or migrate, or `meta` is corrupt.
    pub async fn open(path: &Path, clock: SharedClock, ids: SharedIds) -> Result<Self> {
        let db = Db::open(path, schema::migrations())?;
        Self::start(db, clock, ids).await
    }

    /// An in-memory store (tests).
    ///
    /// # Errors
    /// See [`Self::open`].
    pub async fn open_in_memory(clock: SharedClock, ids: SharedIds) -> Result<Self> {
        let db = Db::open_in_memory(schema::migrations())?;
        Self::start(db, clock, ids).await
    }

    async fn start(db: Db, clock: SharedClock, ids: SharedIds) -> Result<Self> {
        // The copy of the position-0 home is made again when it differs
        // (design §3.9 item 6).
        call(&db, |conn| projects_legacy::repair(conn).map(|_| ())).await?;
        let fresh: IncarnationId = new_id(ids.as_ref());
        let owner: String = new_id(ids.as_ref());
        let now = now_ms(&clock);
        let incarnation = call(&db, move |conn| {
            let tx = conn.transaction()?;
            let found: Option<String> = tx
                .query_row("SELECT incarnation_id FROM meta WHERE id = 1", [], |r| {
                    r.get(0)
                })
                .optional()?;
            let inc = match found {
                Some(inc) => inc,
                None => {
                    tx.execute(
                        "INSERT INTO meta (id, incarnation_id, created_at) VALUES (1, ?1, ?2)",
                        rusqlite::params![fresh.as_str(), now],
                    )?;
                    // D6: one owner now; the id keeps room for teams.
                    tx.execute(
                        "INSERT OR IGNORE INTO owner (id, owner_id, created_at) VALUES (1, ?1, ?2)",
                        rusqlite::params![owner.as_str(), now],
                    )?;
                    fresh.0.clone()
                }
            };
            tx.commit()?;
            Ok(IncarnationId::from(inc))
        })
        .await?;
        let store = Self {
            inner: Arc::new(Inner {
                db,
                clock,
                ids,
                incarnation,
                commands: watch::channel(0).0,
                board: watch::channel(0).0,
                live: broadcast::channel(LIVE_BUFFER).0,
                reachable: Mutex::default(),
                infos: Mutex::default(),
                conflict_mail: AtomicBool::new(true),
                knobs: std::sync::RwLock::default(),
                fleet_control: AtomicBool::new(false),
                link_features: Mutex::default(),
                link_peers: Mutex::default(),
                social: broadcast::channel(LIVE_BUFFER).0,
                presence: Mutex::default(),
                sockets: std::sync::atomic::AtomicU64::new(1),
                sentence_jobs: Mutex::default(),
                ask_limits: coordinator_fleet::AskLimiter::default(),
                fleet_view: fleet_view::FleetViewState::default(),
                scope_dirty: Mutex::default(),
                scope_changes: watch::channel(0).0,
                snapshot_marks: Mutex::default(),
                fence: crate::board_fence::FenceState::default(),
            }),
        };
        // Deck start (grant stations design §2.3 item 8): end every stale
        // grant and rebuild the derived `coordinate` scopes before the deck
        // accepts any link. The link cache is filled from the stored scopes
        // afterwards, so the notes of this pass are not needed.
        store.sweep_grants().await?;
        drop(store.take_scope_changes());
        Ok(store)
    }

    /// The last snapshot that `station` applied in this deck process.
    #[must_use]
    pub fn snapshot_mark(&self, station: &StationId) -> Option<SnapshotMark> {
        self.inner
            .snapshot_marks
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(station)
            .cloned()
    }

    pub(crate) fn note_snapshot(&self, station: &StationId, mark: SnapshotMark) {
        self.inner
            .snapshot_marks
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(station.clone(), mark);
    }

    /// The deck incarnation (`coordinator.call` targets it, ADR-004 §9.3).
    #[must_use]
    pub fn incarnation(&self) -> &IncarnationId {
        &self.inner.incarnation
    }

    /// The database (the link epochs share it, G1).
    #[must_use]
    pub fn db(&self) -> Db {
        self.inner.db.clone()
    }

    /// Turns the conflict mail on or off (`deck.toml` `conflict_mail`, DL-145).
    pub fn set_conflict_mail(&self, on: bool) {
        self.inner.conflict_mail.store(on, Ordering::Relaxed);
    }

    /// Turns fleet control on or off (the deck setting `fleet_control`,
    /// ADR-008 §3).
    pub fn set_fleet_control(&self, on: bool) {
        self.inner.fleet_control.store(on, Ordering::SeqCst);
    }

    /// The deck setting `fleet_control`.
    #[must_use]
    pub fn fleet_control(&self) -> bool {
        self.inner.fleet_control.load(Ordering::SeqCst)
    }

    /// Records the features that link `token` of `station` negotiated.
    pub fn set_link_features(&self, station: &StationId, token: u64, features: Vec<String>) {
        let mut map = self.link_features();
        map.entry(station.clone())
            .or_default()
            .insert(token, features.into_iter().collect());
    }

    /// Records the remote address of link `token` of `station`.
    pub fn set_link_peer(&self, station: &StationId, token: u64, ip: std::net::IpAddr) {
        let mut map = self.link_peers();
        map.entry(station.clone())
            .or_default()
            .insert(token, ip.to_canonical());
    }

    /// The remote address of the newest live link of `station` (the one
    /// with the highest token), or `None` without a live link.
    #[must_use]
    pub fn link_peer(&self, station: &StationId) -> Option<std::net::IpAddr> {
        let map = self.link_peers();
        map.get(station)
            .and_then(|links| links.last_key_value().map(|(_, ip)| *ip))
    }

    /// The remote addresses of every live station link.
    #[must_use]
    pub fn link_peer_ips(&self) -> BTreeSet<std::net::IpAddr> {
        let map = self.link_peers();
        map.values()
            .flat_map(|links| links.values().copied())
            .collect()
    }

    /// Forgets link `token` of `station` (the link ended), with its remote
    /// address.
    pub fn clear_link_features(&self, station: &StationId, token: u64) {
        {
            let mut peers = self.link_peers();
            if let Some(links) = peers.get_mut(station) {
                links.remove(&token);
                if links.is_empty() {
                    peers.remove(station);
                }
            }
        }
        let mut map = self.link_features();
        if let Some(links) = map.get_mut(station) {
            links.remove(&token);
            if links.is_empty() {
                map.remove(station);
            }
        }
    }

    /// Whether `station` has a live link and whether every live link
    /// negotiated `coordinator.v2` (the intersection over its links; ADR-008
    /// §3, fail closed).
    #[must_use]
    pub fn link_v2(&self, station: &StationId) -> LinkV2 {
        let map = self.link_features();
        map.get(station).map_or(LinkV2::Down, v2_state)
    }

    /// Whether `station` has a live link and whether every live link negotiated
    /// `feature` (the intersection over its links, fail closed).
    #[must_use]
    pub fn link_feature(&self, station: &StationId, feature: &str) -> LinkFeature {
        let map = self.link_features();
        let Some(links) = map.get(station).filter(|l| !l.is_empty()) else {
            return LinkFeature::Down;
        };
        if links.values().all(|f| f.contains(feature)) {
            LinkFeature::Negotiated
        } else {
            LinkFeature::Without
        }
    }

    fn link_peers(
        &self,
    ) -> std::sync::MutexGuard<'_, BTreeMap<StationId, BTreeMap<u64, std::net::IpAddr>>> {
        self.inner
            .link_peers
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    fn link_features(
        &self,
    ) -> std::sync::MutexGuard<'_, BTreeMap<StationId, BTreeMap<u64, BTreeSet<String>>>> {
        self.inner
            .link_features
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// The deck clock in Unix milliseconds.
    #[must_use]
    pub fn now_ms(&self) -> i64 {
        now_ms(&self.inner.clock)
    }

    /// Records whether the link to `station` is up.
    pub fn set_reachable(&self, station: &StationId, up: bool) {
        let changed = {
            let mut set = self.reachable_set();
            if up {
                set.insert(station.clone())
            } else {
                set.remove(station)
            }
        };
        if changed {
            self.board_changed();
        }
    }

    /// The board version: it grows after each change of the cards, the
    /// stations or their reachability.
    #[must_use]
    pub fn watch_board(&self) -> watch::Receiver<u64> {
        self.inner.board.subscribe()
    }

    pub(crate) fn board_changed(&self) {
        self.inner.board.send_modify(|n| *n += 1);
    }

    /// The events folded from now on.
    #[must_use]
    pub fn subscribe_live(&self) -> broadcast::Receiver<Arc<LiveEvents>> {
        self.inner.live.subscribe()
    }

    pub(crate) fn publish_live(&self, live: LiveEvents) {
        // No receiver is not an error.
        let _ = self.inner.live.send(Arc::new(live));
    }

    /// Keeps the `station.info` answer of `station` for the board.
    pub fn set_station_info(&self, station: &StationId, mut info: StationInfo) {
        if let Some(desktop) = info.desktop.as_mut() {
            crate::desktop_capture::stamp_features(self, station, desktop);
        }
        let at = self.now_ms();
        let changed = {
            let mut infos = self
                .inner
                .infos
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            let same = infos.get(station).is_some_and(|(old, _)| *old == info);
            infos.insert(station.clone(), (info, at));
            !same
        };
        if changed {
            self.board_changed();
        }
    }

    /// Replaces only the `desktop` field of the kept `station.info` of
    /// `station`, in one step under the lock of the cache (R13 §6D item 6).
    /// It does nothing when the deck holds no `station.info` of the station.
    /// The time of the entry does not move: the rest of the answer is not
    /// newer.
    pub fn update_station_desktop(
        &self,
        station: &StationId,
        mut desktop: fleetdeck_proto::desktop::DesktopInfo,
    ) {
        crate::desktop_capture::stamp_features(self, station, &mut desktop);
        let changed = {
            let mut infos = self
                .inner
                .infos
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            match infos.get_mut(station) {
                Some((info, _)) if info.desktop.as_ref() != Some(&desktop) => {
                    info.desktop = Some(desktop);
                    true
                }
                _ => false,
            }
        };
        if changed {
            self.board_changed();
        }
    }

    /// Every kept `station.info` (the coordinator decision reads them).
    #[must_use]
    pub fn station_infos(&self) -> BTreeMap<StationId, (StationInfo, i64)> {
        self.inner
            .infos
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// The last `station.info` of `station` and when it came.
    #[must_use]
    pub fn station_info(&self, station: &StationId) -> Option<(StationInfo, i64)> {
        self.inner
            .infos
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(station)
            .cloned()
    }

    /// True while the link to `station` is up.
    #[must_use]
    pub fn is_reachable(&self, station: &StationId) -> bool {
        self.reachable_set().contains(station)
    }

    fn reachable_set(&self) -> std::sync::MutexGuard<'_, BTreeSet<StationId>> {
        self.inner
            .reachable
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// The persistent revocation generation (G5).
    ///
    /// # Errors
    /// A database error.
    pub async fn revocation_generation(&self) -> Result<u64> {
        call(&self.inner.db, |conn| {
            let n: i64 = conn.query_row(
                "SELECT revocation_generation FROM meta WHERE id = 1",
                [],
                |r| r.get(0),
            )?;
            to_u64(n)
        })
        .await
    }
}

/// Whether every live link of each station negotiated `feature`.
fn feature_states(
    links: &BTreeMap<StationId, BTreeMap<u64, BTreeSet<String>>>,
    feature: &str,
) -> BTreeMap<String, LinkFeature> {
    links
        .iter()
        .map(|(s, l)| {
            let all = !l.is_empty() && l.values().all(|f| f.contains(feature));
            let state = if all {
                LinkFeature::Negotiated
            } else {
                LinkFeature::Without
            };
            (s.as_str().to_owned(), state)
        })
        .collect()
}

fn v2_state(links: &BTreeMap<u64, BTreeSet<String>>) -> LinkV2 {
    let v2 = fleetdeck_proto::version::feature::COORDINATOR_V2;
    if links.is_empty() {
        LinkV2::Down
    } else if links.values().all(|f| f.contains(v2)) {
        LinkV2::Negotiated
    } else {
        LinkV2::Without
    }
}

pub(crate) fn bump_generation(conn: &Connection) -> Result<u64> {
    let n: i64 = conn.query_row(
        "UPDATE meta SET revocation_generation = revocation_generation + 1 WHERE id = 1
         RETURNING revocation_generation",
        [],
        |r| r.get(0),
    )?;
    to_u64(n)
}

pub(crate) async fn call<R: Send + 'static>(
    db: &Db,
    f: impl FnOnce(&mut Connection) -> Result<R> + Send + 'static,
) -> Result<R> {
    db.call(move |conn| Ok(f(conn))).await?
}

fn now_ms(clock: &SharedClock) -> i64 {
    i64::try_from(clock.now_ms()).unwrap_or(i64::MAX)
}

pub(crate) fn to_u64(v: i64) -> Result<u64> {
    u64::try_from(v).map_err(|_| DeckError::Corrupt(format!("negative sequence {v}")))
}

pub(crate) fn to_i64(v: u64) -> Result<i64> {
    i64::try_from(v).map_err(|_| DeckError::Corrupt(format!("sequence {v} too large")))
}

#[cfg(test)]
mod read_model_tests;
#[cfg(test)]
mod review_send_tests;
#[cfg(test)]
mod review_tests;
#[cfg(test)]
mod tests;
