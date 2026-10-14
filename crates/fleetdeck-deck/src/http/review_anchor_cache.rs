//! The station calls of the review routes and their caches (DL-179, design
//! 2.2 and 2.3):
//!
//! - the diff of a snapshot, fetched from the station as the caller (the
//!   member flag for a member, so the station's deny list and private paths
//!   apply), parsed once and kept 60 s per (station, session, snapshot,
//!   member flag), at most 64 entries, the least recently used one leaves
//!   first;
//! - the member deny list of a station (`settings.get`), kept 60 s per
//!   station. A failure is "cannot be established" and is not kept.
//!
//! The caches are process-wide and their keys name the station id, which
//! the deck derives from the station key, so two boards never share an
//! entry by accident. Time is the deck clock.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::Duration;

use axum::http::StatusCode;
use fleetdeck_proto::StationId;
use fleetdeck_proto::files::{
    CheckpointRef, SessionDiff, SessionDiffQuery, TurnDiff, TurnDiffQuery, code as files_code,
};
use fleetdeck_proto::link::Outcome;
use fleetdeck_proto::member_deny::{MEMBER_PARAM, MemberDeny};
use fleetdeck_proto::settings::SettingsView;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use super::Board;
use super::guard::ApiError;
use crate::store::review_anchor::ParsedDiff;

/// How long a cached diff or deny list is used.
pub const TTL_MS: i64 = 60_000;
/// The most diffs that the cache holds.
pub const MAX_DIFFS: usize = 64;
/// The longest the deck waits for the station.
pub const WAIT: Duration = Duration::from_secs(20);

/// The identity of a diff snapshot (design 1).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Snapshot {
    /// The work tree against the base commit of the session.
    Spawn(String),
    /// A finalized turn by its checkpoint commit pair.
    Turn { from: String, to: String },
}

fn oid(text: &str) -> Option<String> {
    (matches!(text.len(), 40 | 64) && text.bytes().all(|b| b.is_ascii_hexdigit()))
        .then(|| text.to_ascii_lowercase())
}

impl Snapshot {
    /// Reads `spawn:<oid>` or `turn:<from>..<to>`; an oid has 40 or 64 hex
    /// characters. `None` for anything else.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        if let Some(base) = text.strip_prefix("spawn:") {
            return oid(base).map(Self::Spawn);
        }
        let pair = text.strip_prefix("turn:")?;
        let (from, to) = pair.split_once("..")?;
        Some(Self::Turn {
            from: oid(from)?,
            to: oid(to)?,
        })
    }

    /// The text that the deck stores.
    #[must_use]
    pub fn id(&self) -> String {
        match self {
            Self::Spawn(base) => format!("spawn:{base}"),
            Self::Turn { from, to } => format!("turn:{from}..{to}"),
        }
    }
}

type Key = (StationId, String, Snapshot, bool);

#[derive(Default)]
struct Lru {
    tick: u64,
    map: HashMap<Key, (i64, u64, Arc<ParsedDiff>)>,
}

impl Lru {
    fn get(&mut self, key: &Key, now: i64) -> Option<Arc<ParsedDiff>> {
        self.tick += 1;
        let tick = self.tick;
        let (at, used, diff) = self.map.get_mut(key)?;
        if now.saturating_sub(*at) >= TTL_MS {
            self.map.remove(key);
            return None;
        }
        *used = tick;
        Some(Arc::clone(diff))
    }

    fn put(&mut self, key: Key, now: i64, diff: Arc<ParsedDiff>) {
        self.tick += 1;
        if self.map.len() >= MAX_DIFFS
            && !self.map.contains_key(&key)
            && let Some(oldest) = self
                .map
                .iter()
                .min_by_key(|(_, (_, used, _))| *used)
                .map(|(k, _)| k.clone())
        {
            self.map.remove(&oldest);
        }
        self.map.insert(key, (now, self.tick, diff));
    }
}

fn diffs() -> &'static Mutex<Lru> {
    static CACHE: OnceLock<Mutex<Lru>> = OnceLock::new();
    CACHE.get_or_init(Mutex::default)
}

type DenyCache = Mutex<HashMap<StationId, (i64, MemberDeny)>>;

fn denies() -> &'static DenyCache {
    static CACHE: OnceLock<DenyCache> = OnceLock::new();
    CACHE.get_or_init(Mutex::default)
}

/// Forgets what the caches hold for one station (tests).
#[doc(hidden)]
pub fn clear_station(station: &StationId) {
    diffs()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .map
        .retain(|(s, ..), _| s != station);
    denies()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .remove(station);
}

fn unreachable_error(station: &StationId) -> ApiError {
    ApiError::new(
        StatusCode::CONFLICT,
        "station_unreachable",
        format!("station `{station}` has no live link to this deck"),
    )
}

/// One request to the station's live link.
async fn request(
    board: &Board,
    station: &StationId,
    method: &'static str,
    params: Value,
) -> Result<Outcome, ApiError> {
    let control = board
        .link
        .control_for(station)
        .ok_or_else(|| unreachable_error(station))?;
    tokio::time::timeout(WAIT, control.request(method, None, params))
        .await
        .map_err(|_| {
            ApiError::new(
                StatusCode::GATEWAY_TIMEOUT,
                "timeout",
                "the station did not answer in time",
            )
        })?
        .map_err(|_| unreachable_error(station))
}

/// The error of a station refusal. `unavailable` lists the codes that mean
/// "no such diff" and the error that they become.
fn refusal(code: &str, message: &str, unavailable: &[&str], then: ApiError) -> ApiError {
    if unavailable.contains(&code) {
        return then;
    }
    match code {
        files_code::UNKNOWN_SESSION => ApiError::new(
            StatusCode::NOT_FOUND,
            "unknown_session",
            "the station does not know this session",
        ),
        files_code::BUSY => ApiError::new(StatusCode::TOO_MANY_REQUESTS, "busy", message),
        files_code::TIMEOUT => ApiError::new(StatusCode::GATEWAY_TIMEOUT, "timeout", message),
        "forbidden" => ApiError::new(StatusCode::FORBIDDEN, "forbidden", message),
        _ => ApiError::new(
            StatusCode::BAD_GATEWAY,
            "station_error",
            format!("{code}: {message}"),
        ),
    }
}

fn decode<T: DeserializeOwned>(value: Value) -> Result<T, ApiError> {
    serde_json::from_value(value)
        .map_err(|e| ApiError::new(StatusCode::BAD_GATEWAY, "station_error", e.to_string()))
}

/// The `not_in_diff` error.
#[must_use]
pub fn not_in_diff(why: &str) -> ApiError {
    ApiError::new(StatusCode::BAD_REQUEST, "not_in_diff", why)
}

/// The `checkpoint_unavailable` error.
#[must_use]
pub fn checkpoint_unavailable() -> ApiError {
    ApiError::new(
        StatusCode::NOT_FOUND,
        "checkpoint_unavailable",
        "the station has no finalized turn with that checkpoint pair",
    )
}

fn with_member(mut params: Value, member: bool) -> Value {
    if member && let Some(o) = params.as_object_mut() {
        o.insert(MEMBER_PARAM.to_owned(), Value::Bool(true));
    }
    params
}

/// The turn diff of a checkpoint pair, as the caller may see it.
///
/// # Errors
/// 404 `checkpoint_unavailable` for a station that has no such row or does
/// not know the selector; other station refusals keep their meaning.
pub async fn turn_diff(
    board: &Board,
    station: &StationId,
    session: &str,
    pair: CheckpointRef,
    member: bool,
) -> Result<TurnDiff, ApiError> {
    let query = TurnDiffQuery {
        session_id: session.into(),
        turn_id: None,
        checkpoint: Some(pair),
        max_bytes: None,
    };
    let params = with_member(json!(query), member);
    match request(board, station, "files.diff", params).await? {
        Outcome::Ok(v) => decode(v),
        // An older station refuses the unknown field as `protocol_error`.
        Outcome::Err(e) => Err(refusal(
            &e.code,
            &e.message,
            &[
                files_code::CHECKPOINT_UNAVAILABLE,
                files_code::NO_DIFF,
                fleetdeck_link::methods::code::PROTOCOL_ERROR,
                crate::link::NOT_IMPLEMENTED,
            ],
            checkpoint_unavailable(),
        )),
    }
}

/// The parsed diff of `snapshot`, from the cache or from the station.
///
/// # Errors
/// 400 `not_in_diff` when the session has no such diff (another base commit,
/// no base commit, no diff); 404 `checkpoint_unavailable`; the station
/// refusals.
pub async fn diff(
    board: &Board,
    station: &StationId,
    session: &str,
    snapshot: &Snapshot,
    member: bool,
) -> Result<Arc<ParsedDiff>, ApiError> {
    let key: Key = (
        station.clone(),
        session.to_owned(),
        snapshot.clone(),
        member,
    );
    let now = board.store.now_ms();
    if let Some(hit) = diffs()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(&key, now)
    {
        return Ok(hit);
    }
    let patch = match snapshot {
        Snapshot::Spawn(base) => {
            let query = SessionDiffQuery {
                session_id: session.into(),
                max_bytes: None,
            };
            let params = with_member(json!(query), member);
            let answer: SessionDiff =
                match request(board, station, "files.session_diff", params).await? {
                    Outcome::Ok(v) => decode(v)?,
                    Outcome::Err(e) => {
                        return Err(refusal(
                            &e.code,
                            &e.message,
                            &[files_code::NO_DIFF, files_code::NO_BASE],
                            not_in_diff("the session has no diff against a base commit"),
                        ));
                    }
                };
            if &answer.base_oid != base {
                return Err(not_in_diff("the base commit of the session is another one"));
            }
            answer.patch
        }
        Snapshot::Turn { from, to } => {
            let pair = CheckpointRef {
                from_commit: from.clone(),
                to_commit: to.clone(),
            };
            turn_diff(board, station, session, pair, member)
                .await?
                .patch
        }
    };
    let parsed = Arc::new(ParsedDiff::parse(&patch));
    diffs().lock().unwrap_or_else(PoisonError::into_inner).put(
        key,
        board.store.now_ms(),
        Arc::clone(&parsed),
    );
    Ok(parsed)
}

/// The member deny list of the station: the default patterns and the
/// `member_deny` of its settings. `None` when it cannot be established (a
/// refusal, a bad answer, no live link, a timeout): callers fail closed.
pub async fn deny_list(board: &Board, station: &StationId) -> Option<MemberDeny> {
    let now = board.store.now_ms();
    if let Some((at, deny)) = denies()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(station)
        && now.saturating_sub(*at) < TTL_MS
    {
        return Some(deny.clone());
    }
    let Ok(Outcome::Ok(v)) = request(board, station, "settings.get", json!({})).await else {
        return None;
    };
    let view: SettingsView = serde_json::from_value(v).ok()?;
    let extra: Vec<String> = view
        .values
        .get("member_deny")?
        .as_array()?
        .iter()
        .map(|p| p.as_str().map(str::to_owned))
        .collect::<Option<_>>()?;
    let deny = MemberDeny::new(&extra);
    denies()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(station.clone(), (board.store.now_ms(), deny.clone()));
    Some(deny)
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = "0123456789abcdef0123456789abcdef01234567";
    const B: &str = "fedcba9876543210fedcba9876543210fedcba98";

    #[test]
    fn a_snapshot_has_hex_oids_and_a_stable_text() {
        assert_eq!(
            Snapshot::parse(&format!("spawn:{}", A.to_uppercase())),
            Some(Snapshot::Spawn(A.to_owned()))
        );
        let turn = Snapshot::parse(&format!("turn:{A}..{B}")).unwrap();
        assert_eq!(turn.id(), format!("turn:{A}..{B}"));
        for bad in [
            "spawn:abc",
            &format!("turn:{A}"),
            &format!("turn:{A}..xyz"),
            &format!("turn:{A}...{B}"),
            &format!("other:{A}"),
            &format!("spawn:{A}0"),
            "",
        ] {
            assert_eq!(Snapshot::parse(bad), None, "{bad}");
        }
        let long = "a".repeat(64);
        assert!(Snapshot::parse(&format!("spawn:{long}")).is_some());
    }

    fn key(n: usize) -> Key {
        (
            StationId::from("s"),
            n.to_string(),
            Snapshot::Spawn(A.to_owned()),
            false,
        )
    }

    #[test]
    fn the_diff_cache_expires_after_60_seconds_and_drops_the_least_recently_used() {
        let mut lru = Lru::default();
        let d = Arc::new(ParsedDiff::default());
        lru.put(key(0), 1_000, Arc::clone(&d));
        assert!(lru.get(&key(0), 1_000 + TTL_MS - 1).is_some());
        assert!(lru.get(&key(0), 1_000 + TTL_MS).is_none(), "expired");
        assert!(lru.map.is_empty());
        for n in 0..MAX_DIFFS {
            lru.put(key(n), 0, Arc::clone(&d));
        }
        // Use entry 0, so entry 1 is the least recently used.
        assert!(lru.get(&key(0), 1).is_some());
        lru.put(key(MAX_DIFFS), 2, Arc::clone(&d));
        assert_eq!(lru.map.len(), MAX_DIFFS);
        assert!(lru.map.contains_key(&key(0)));
        assert!(!lru.map.contains_key(&key(1)));
        assert!(lru.map.contains_key(&key(MAX_DIFFS)));
    }
}
