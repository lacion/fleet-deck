//! The change summary route of a wall display (R13 §5A item 5, §5C item 3,
//! §5D items 13 and 14, R11):
//! `GET /api/displays/self/sessions/{station}/{session}/changes?patch=0|1`.
//!
//! The answer is a [`DisplayChanges`] from the station method
//! `files.display_changes` (feature `display_changes.v1`). The station
//! applies the private-path protection and the member deny list; the deck
//! never builds the answer itself and never falls back to an owner payload.
//!
//! Order of the checks: the login and the effective scope (401), the session
//! inside the scope (one 404 `unknown_session` for every cause, so there is
//! no existence oracle), `show_diffs` for a patch (403 `diffs_off`), then the
//! link. The same checks run again after the station answer.
//!
//! The deck joins identical computations (the same board, station, session
//! and `patch`) and lets one display have one computation at a time (a
//! different one gives 429 `busy`). Both maps are process-global; the board
//! address in the key keeps the tests with several boards apart.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use axum::extract::{Extension, Path, Query, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use fleetdeck_link::control::Control;
use fleetdeck_link::methods::code as link_code;
use fleetdeck_proto::display_changes::{
    DisplayChanges, DisplayChangesQuery, MAX_DISPLAY_CHANGES, MAX_DISPLAY_PATCH_FILE,
};
use fleetdeck_proto::files::code as file_code;
use fleetdeck_proto::link::Outcome;
use fleetdeck_proto::version::feature;
use fleetdeck_proto::{SessionId, StationId};
use serde::Deserialize;
use tokio::sync::watch;

use super::Board;
use super::display_view::{in_scope, scope_of};
use super::file_routes::FILES_WAIT;
use super::guard::ApiError;
use crate::store::{DisplayLogin, Principal};

/// The routes of this part; the caller adds the guard.
pub fn routes() -> Router<Arc<Board>> {
    Router::new().route(
        "/api/displays/self/sessions/{station}/{session}/changes",
        get(changes),
    )
}

/// The query of the route: `patch` is `0` or `1`.
#[derive(Debug, Default, Deserialize)]
struct Params {
    patch: Option<String>,
}

/// A refusal that a joined computation shares between its callers.
#[derive(Debug, Clone)]
struct Failure {
    status: StatusCode,
    code: &'static str,
    text: &'static str,
}

impl Failure {
    const fn new(status: StatusCode, code: &'static str, text: &'static str) -> Self {
        Self { status, code, text }
    }

    fn api(&self) -> ApiError {
        ApiError::new(self.status, self.code, self.text)
    }
}

fn unreachable_station() -> Failure {
    Failure::new(
        StatusCode::CONFLICT,
        "station_unreachable",
        "The station is not connected.",
    )
}

fn unsupported() -> Failure {
    Failure::new(
        StatusCode::CONFLICT,
        "changes_unsupported",
        "This station cannot send change summaries.",
    )
}

fn station_failed() -> Failure {
    Failure::new(
        StatusCode::BAD_GATEWAY,
        "station_error",
        "The station could not read the changes.",
    )
}

fn timed_out() -> Failure {
    Failure::new(
        StatusCode::GATEWAY_TIMEOUT,
        "timeout",
        "The station did not answer in time.",
    )
}

/// The fixed answer for a station error code (no station text reaches a
/// display).
fn station_failure(code: &str) -> Failure {
    match code {
        file_code::UNKNOWN_SESSION => unknown_session_failure(),
        file_code::NO_DIFF => Failure::new(
            StatusCode::NOT_FOUND,
            "no_diff",
            "This session has no changes to show.",
        ),
        file_code::NO_BASE => Failure::new(
            StatusCode::CONFLICT,
            "no_base",
            "This session has no base commit.",
        ),
        file_code::BUSY => Failure::new(
            StatusCode::TOO_MANY_REQUESTS,
            "busy",
            "The station is busy; try again.",
        ),
        file_code::TIMEOUT => timed_out(),
        link_code::FEATURE_NOT_NEGOTIATED => unsupported(),
        _ => station_failed(),
    }
}

fn unknown_session_failure() -> Failure {
    Failure::new(StatusCode::NOT_FOUND, "unknown_session", "No such session.")
}

/// What a computation publishes: the answer or the refusal.
type Answer = Result<Arc<DisplayChanges>, Failure>;
type Slot = watch::Receiver<Option<Answer>>;
type Key = (usize, StationId, SessionId, bool);

#[derive(Default)]
struct Shared {
    /// The computations in flight.
    jobs: HashMap<Key, Slot>,
    /// The computation of each display in flight, with the number of
    /// requests that wait for it.
    busy: HashMap<String, (Key, usize)>,
}

fn state() -> &'static Mutex<Shared> {
    static STATE: OnceLock<Mutex<Shared>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(Shared::default()))
}

fn lock() -> std::sync::MutexGuard<'static, Shared> {
    state().lock().unwrap_or_else(PoisonError::into_inner)
}

/// Frees the place of a display when its request ends, also when the client
/// goes away.
struct Busy {
    display_id: String,
}

impl Drop for Busy {
    fn drop(&mut self) {
        let mut st = lock();
        if let Some((_, count)) = st.busy.get_mut(&self.display_id) {
            *count -= 1;
            if *count == 0 {
                st.busy.remove(&self.display_id);
            }
        }
    }
}

/// `GET /api/displays/self/sessions/{station}/{session}/changes`.
///
/// # Errors
/// 400 `invalid_query`; 401 `login_required`; 403 `display_only`,
/// `diffs_off`; 404 `unknown_session`, `no_diff`; 409 `station_unreachable`,
/// `changes_unsupported`, `no_base`; 429 `busy`; 502 `station_error`; 504
/// `timeout`.
async fn changes(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    Path((station, session)): Path<(String, String)>,
    Query(params): Query<Params>,
) -> Result<Response, ApiError> {
    let Principal::Display(login) = principal else {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "display_only",
            "only a wall display login does this",
        ));
    };
    let patch = match params.patch.as_deref() {
        None | Some("0") => false,
        Some("1") => true,
        Some(_) => {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_query",
                "patch is 0 or 1",
            ));
        }
    };
    let station = StationId::new(station);
    let session = SessionId::from(session);
    authorize(&board, &login, &station, &session, patch).await?;
    let control = board
        .link
        .control_for(&station)
        .ok_or_else(|| unreachable_station().api())?;
    if !control.params().negotiated.has(feature::DISPLAY_CHANGES) {
        return Err(unsupported().api());
    }
    let answer = compute(&board, &login, control, &station, &session, patch).await?;
    // The policy again after the await: the scope, the session and
    // `show_diffs` of this moment.
    authorize(&board, &login, &station, &session, patch).await?;
    let body = finish(answer.as_ref().clone(), patch);
    let mut response = Json(body).into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(response)
}

/// The scope, the session and, for a patch, `show_diffs` of this moment.
async fn authorize(
    board: &Arc<Board>,
    login: &DisplayLogin,
    station: &StationId,
    session: &SessionId,
    patch: bool,
) -> Result<(), ApiError> {
    let scope = scope_of(board, login).await?;
    let card = board
        .store
        .session_summary(station.clone(), session.clone())
        .await
        .map_err(ApiError::internal)?;
    let visible =
        card.is_some_and(|c| in_scope(&scope, station, c.repo.as_ref().map(|r| r.root.as_str())));
    if !visible {
        return Err(unknown_session_failure().api());
    }
    if patch {
        let record = board
            .store
            .display_get(login.display_id.clone())
            .await
            .map_err(ApiError::internal)?
            .ok_or_else(|| {
                ApiError::new(
                    StatusCode::UNAUTHORIZED,
                    "login_required",
                    "the display login ended",
                )
            })?;
        if !record.show_diffs {
            return Err(ApiError::new(
                StatusCode::FORBIDDEN,
                "diffs_off",
                "Patches are off for this display.",
            ));
        }
    }
    Ok(())
}

/// Joins or starts the computation and waits for its answer.
async fn compute(
    board: &Arc<Board>,
    login: &DisplayLogin,
    control: Control,
    station: &StationId,
    session: &SessionId,
    patch: bool,
) -> Result<Arc<DisplayChanges>, ApiError> {
    let key: Key = (
        Arc::as_ptr(board) as usize,
        station.clone(),
        session.clone(),
        patch,
    );
    let (mut slot, _busy) = {
        let mut st = lock();
        if let Some((open, _)) = st.busy.get(&login.display_id)
            && *open != key
        {
            return Err(ApiError::new(
                StatusCode::TOO_MANY_REQUESTS,
                "busy",
                "A change computation of this display is still running.",
            ));
        }
        let slot = if let Some(slot) = st.jobs.get(&key) {
            slot.clone()
        } else {
            let (tx, rx) = watch::channel(None);
            st.jobs.insert(key.clone(), rx.clone());
            let query = DisplayChangesQuery {
                session_id: session.clone(),
                patch,
                max_bytes: None,
            };
            let job_key = key.clone();
            tokio::spawn(async move {
                let answer = fetch(control, query).await;
                lock().jobs.remove(&job_key);
                tx.send_replace(Some(answer));
            });
            rx
        };
        st.busy
            .entry(login.display_id.clone())
            .and_modify(|(_, count)| *count += 1)
            .or_insert((key, 1));
        (
            slot,
            Busy {
                display_id: login.display_id.clone(),
            },
        )
    };
    let answer = slot
        .wait_for(Option::is_some)
        .await
        .map_err(|_| station_failed().api())?
        .clone()
        .ok_or_else(|| station_failed().api())?;
    answer.map_err(|f| f.api())
}

/// The station call of one computation.
async fn fetch(control: Control, query: DisplayChangesQuery) -> Answer {
    let params = serde_json::to_value(&query).map_err(|_| station_failed())?;
    let outcome = tokio::time::timeout(
        FILES_WAIT,
        control.request("files.display_changes", None, params),
    )
    .await
    .map_err(|_| timed_out())?
    .map_err(|_| unreachable_station())?;
    match outcome {
        Outcome::Ok(value) => serde_json::from_value::<DisplayChanges>(value)
            .map(Arc::new)
            .map_err(|_| station_failed()),
        Outcome::Err(e) => Err(station_failure(&e.code)),
    }
}

/// The serialized size of a value (the body of the answer is compact JSON).
fn size<T: serde::Serialize>(value: &T) -> usize {
    serde_json::to_vec(value).map_or(usize::MAX, |v| v.len())
}

/// The answer as the display gets it: a hidden entry loses everything but
/// its marker, a section above [`MAX_DISPLAY_PATCH_FILE`] is left out, no
/// patch appears when none was asked for, and the serialized answer stays
/// within [`MAX_DISPLAY_CHANGES`] (first the patches of the last files go,
/// then the last files, which sets `truncated`).
fn finish(mut c: DisplayChanges, patch: bool) -> DisplayChanges {
    for f in &mut c.files {
        if f.hidden {
            f.path = None;
            f.added = None;
            f.removed = None;
            f.binary = false;
            f.patch = None;
            f.too_large = false;
        } else if !patch {
            f.patch = None;
            f.too_large = false;
        } else if f
            .patch
            .as_ref()
            .is_some_and(|p| p.len() as u64 > MAX_DISPLAY_PATCH_FILE)
        {
            f.patch = None;
            f.too_large = true;
        }
    }
    let limit = MAX_DISPLAY_CHANGES as usize;
    let files = std::mem::take(&mut c.files);
    let mut total = size(&c);
    let mut sizes: Vec<usize> = files.iter().map(|f| size(f) + 1).collect();
    total = total.saturating_add(sizes.iter().sum());
    let mut files = files;
    for (f, s) in files.iter_mut().zip(sizes.iter_mut()).rev() {
        if total <= limit {
            break;
        }
        if f.patch.is_some() {
            f.patch = None;
            f.too_large = true;
            let now = size(f) + 1;
            total = total.saturating_sub(*s - now.min(*s));
            *s = now;
        }
    }
    while total > limit {
        let Some(s) = sizes.pop() else { break };
        files.pop();
        total = total.saturating_sub(s);
        c.truncated = true;
    }
    c.files = files;
    c
}

#[cfg(test)]
#[path = "display_changes_tests.rs"]
mod display_changes_tests;
