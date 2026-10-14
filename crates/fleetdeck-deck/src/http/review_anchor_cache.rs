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
//!
//! Each station has a generation. `clear_station` raises it. A fill that
//! read the generation before its station call stores its answer only when
//! the generation is still the same, so a fill that started before a
//! settings change cannot put old data back (NV-33).

use std::collections::HashMap;
use std::future::Future;
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

type Generations = Mutex<HashMap<StationId, u64>>;

fn generations() -> &'static Generations {
    static GENERATIONS: OnceLock<Generations> = OnceLock::new();
    GENERATIONS.get_or_init(Mutex::default)
}

/// The generation of one station: how many times `clear_station` ran for
/// it. 0 for a station that was never cleared (NV-33).
fn generation(station: &StationId) -> u64 {
    generations()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(station)
        .copied()
        .unwrap_or(0)
}

/// Forgets what the caches hold for one station: a settings change may
/// change the member deny list (NV-33).
///
/// The generation rises first. A fill that read the old generation then
/// cannot store. A fill checks the generation under the cache lock, so a
/// fill that stored before the removals loses its entry to them (NV-33).
pub fn clear_station(station: &StationId) {
    {
        let mut all = generations().lock().unwrap_or_else(PoisonError::into_inner);
        let next = all.entry(station.clone()).or_insert(0);
        *next = next.saturating_add(1);
    }
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

/// How many entries the caches hold for one station: its diffs and its
/// deny list (tests).
#[doc(hidden)]
pub fn cached_entries(station: &StationId) -> usize {
    let diffs = diffs()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .map
        .keys()
        .filter(|(s, ..)| s == station)
        .count();
    let deny = denies()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .contains_key(station);
    diffs + usize::from(deny)
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
    fill_diff(key, || board.store.now_ms(), async {
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
        Ok(patch)
    })
    .await
}

/// Parses the patch that `fetch` returns and keeps it in the diff cache.
/// The generation of the station is read before the fetch. The parsed diff
/// is kept only when the generation is the same after it. The caller gets
/// the diff in both cases, because the anchor path applies the current
/// deny list (NV-33, a fill that started before `clear_station`).
async fn fill_diff(
    key: Key,
    now: impl Fn() -> i64,
    fetch: impl Future<Output = Result<String, ApiError>>,
) -> Result<Arc<ParsedDiff>, ApiError> {
    let before = generation(&key.0);
    let patch = fetch.await?;
    let parsed = Arc::new(ParsedDiff::parse(&patch));
    let mut cache = diffs().lock().unwrap_or_else(PoisonError::into_inner);
    if generation(&key.0) == before {
        cache.put(key, now(), Arc::clone(&parsed));
    }
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
    fill_deny(
        station,
        || board.store.now_ms(),
        || fetch_deny(board, station),
    )
    .await
}

/// One `settings.get` of the station, read as a deny list.
async fn fetch_deny(board: &Board, station: &StationId) -> Option<MemberDeny> {
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
    Some(MemberDeny::new(&extra))
}

/// Fills the deny cache with the answer of `fetch`. The generation is read
/// before each fetch. The answer is stored and returned only when the
/// generation is the same after it. When a `clear_station` ran during the
/// fetch, the fetch runs once more. When the generation changes again, or
/// the second fetch fails, the result is `None` and callers fail closed
/// (NV-33, a fill that started before `clear_station`).
async fn fill_deny<F, Fut>(
    station: &StationId,
    now: impl Fn() -> i64,
    mut fetch: F,
) -> Option<MemberDeny>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Option<MemberDeny>>,
{
    for _ in 0..2 {
        let before = generation(station);
        let deny = fetch().await?;
        let mut cache = denies().lock().unwrap_or_else(PoisonError::into_inner);
        if generation(station) == before {
            cache.insert(station.clone(), (now(), deny.clone()));
            return Some(deny);
        }
    }
    None
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

    fn deny_with(pattern: &str) -> MemberDeny {
        MemberDeny::new(&[pattern.to_owned()])
    }

    fn cached_deny(station: &StationId) -> bool {
        denies().lock().unwrap().contains_key(station)
    }

    #[test]
    fn clear_station_raises_the_generation_of_that_station_only() {
        let one = StationId::from("gen-one");
        let two = StationId::from("gen-two");
        assert_eq!(generation(&one), 0);
        clear_station(&one);
        assert_eq!(generation(&one), 1);
        clear_station(&one);
        assert_eq!(generation(&one), 2);
        assert_eq!(generation(&two), 0);
    }

    #[tokio::test]
    async fn a_deny_fill_without_a_clear_is_stored_and_returned() {
        let station = StationId::from("deny-plain");
        let got = fill_deny(&station, || 5, || async { Some(deny_with("*.plain")) }).await;
        assert!(got.unwrap().denies("a.plain"));
        assert!(cached_deny(&station));
    }

    #[tokio::test]
    async fn a_deny_fill_that_a_clear_overtook_is_fetched_again_and_the_new_answer_is_stored() {
        let station = StationId::from("deny-retry");
        let calls = std::cell::Cell::new(0);
        let got = fill_deny(
            &station,
            || 5,
            || {
                calls.set(calls.get() + 1);
                let first = calls.get() == 1;
                let station = station.clone();
                async move {
                    if first {
                        // The settings change lands while the old answer is pending.
                        clear_station(&station);
                        Some(deny_with("*.old"))
                    } else {
                        Some(deny_with("*.new"))
                    }
                }
            },
        )
        .await
        .unwrap();
        assert_eq!(calls.get(), 2);
        assert!(got.denies("a.new"));
        assert!(!got.denies("a.old"));
        let kept = denies().lock().unwrap().get(&station).cloned().unwrap();
        assert!(kept.1.denies("a.new"));
        assert!(!kept.1.denies("a.old"));
    }

    #[tokio::test]
    async fn a_deny_fill_that_a_clear_overtook_twice_is_not_stored_and_fails_closed() {
        let station = StationId::from("deny-twice");
        let calls = std::cell::Cell::new(0);
        let got = fill_deny(
            &station,
            || 5,
            || {
                calls.set(calls.get() + 1);
                let station = station.clone();
                async move {
                    clear_station(&station);
                    Some(deny_with("*.old"))
                }
            },
        )
        .await;
        assert_eq!(calls.get(), 2);
        assert!(got.is_none());
        assert!(!cached_deny(&station));
    }

    #[tokio::test]
    async fn a_deny_fill_whose_second_fetch_fails_fails_closed() {
        let station = StationId::from("deny-second-fails");
        let calls = std::cell::Cell::new(0);
        let got = fill_deny(
            &station,
            || 5,
            || {
                calls.set(calls.get() + 1);
                let first = calls.get() == 1;
                let station = station.clone();
                async move {
                    if first {
                        clear_station(&station);
                        Some(deny_with("*.old"))
                    } else {
                        None
                    }
                }
            },
        )
        .await;
        assert_eq!(calls.get(), 2);
        assert!(got.is_none());
        assert!(!cached_deny(&station));
    }

    fn diff_key(station: &StationId) -> Key {
        (
            station.clone(),
            "session".to_owned(),
            Snapshot::Spawn(A.to_owned()),
            true,
        )
    }

    fn diff_cached(key: &Key) -> bool {
        diffs().lock().unwrap().map.contains_key(key)
    }

    #[tokio::test]
    async fn a_diff_fill_without_a_clear_is_stored_and_returned() {
        let station = StationId::from("diff-plain");
        let key = diff_key(&station);
        let got = fill_diff(key.clone(), || 5, async { Ok(String::new()) }).await;
        assert!(got.is_ok());
        assert!(diff_cached(&key));
    }

    #[tokio::test]
    async fn a_diff_fill_that_a_clear_overtook_is_returned_but_not_stored() {
        let station = StationId::from("diff-overtaken");
        let key = diff_key(&station);
        let other = StationId::from("diff-untouched");
        let other_key = diff_key(&other);
        let other_got = fill_diff(other_key.clone(), || 5, async { Ok(String::new()) }).await;
        assert!(other_got.is_ok());
        let clearing = station.clone();
        let got = fill_diff(key.clone(), || 5, async move {
            // The settings change lands while the fetch is pending.
            clear_station(&clearing);
            Ok(String::new())
        })
        .await;
        assert!(got.is_ok(), "the caller still gets the diff");
        assert!(!diff_cached(&key));
        assert!(diff_cached(&other_key), "another station keeps its entry");
    }
}
