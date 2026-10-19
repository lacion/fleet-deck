//! The desktop tools routes and the maintenance channel of a station (BG-1,
//! design `docs/v2/design/board-gaps.md` section 2.9). Six routes, all owner
//! only (the board login: `Scope::Admin` of a token is not enough):
//!
//! - `GET .../desktop-tools`: link method `station.tools` (feature
//!   `station_tools.v1`).
//! - `GET .../desktop-tools/plan`: `station.desktop_tools_plan`. The deck
//!   binds the plan to the station and to the login that read it.
//! - `POST .../maintenance`: `maintenance.open`, with the elevation. The plan
//!   is claimed by one request ID ([`claim_plan`]). Each call writes one
//!   `station.maintenance` row: `stage=opened` or `stage=refused`.
//! - `GET .../maintenance`: `maintenance.list`. A run that has ended gets its
//!   completion row here too (`maintenance_audit.rs`).
//! - `POST .../maintenance/{id}/resize`: `maintenance.resize`.
//! - `GET .../maintenance/{id}/attach`: a WebSocket on the data stream of
//!   `maintenance.attach`, relayed as a shell. The end of the relay only
//!   closes the stream.
//!
//! The deck answers with its own fixed text for each station code, never with
//! the text of the station. The audit holds IDs and fixed words, never a
//! password, a terminal byte or a message.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex, PoisonError};
use std::time::Duration;

use axum::Json;
use axum::extract::rejection::JsonRejection;
use axum::extract::ws::WebSocketUpgrade;
use axum::extract::{Extension, Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use fleetdeck_proto::desktop_tools::{
    DesktopToolsPlan, MaintenanceAttach, MaintenanceInfo, MaintenanceList, MaintenanceOpen,
    MaintenanceResize, MaintenanceState, StationTools, code, is_hex_id, size_ok,
};
use fleetdeck_proto::link::{Outcome, WireError};
use fleetdeck_proto::shell::ShellAttached;
use fleetdeck_proto::version::feature;
use serde::Deserialize;
use serde_json::{Value, json};

use super::Board;
use super::artifact_delete::ELEVATION_HEADER;
use super::guard::{ApiError, owner_only};
use super::maintenance_audit::{self, audit_completion, watch_completion};
use super::settings_routes::{is_elevated, owner, station_call_wait};
use super::shell_routes::{MAX_INPUT_MESSAGE, relay};
use crate::board_fence::Ticket;
use crate::store::{AuditResult, LinkFeature, LoginSession, NewAudit, Principal, StationRecord};

const TOOLS_WAIT: Duration = Duration::from_secs(30);
const PLAN_WAIT: Duration = Duration::from_secs(75);
const OPEN_WAIT: Duration = Duration::from_secs(75);
const SHORT_WAIT: Duration = Duration::from_secs(10);

/// How long the deck remembers a plan: the life of a plan on the station.
const PLAN_MEMORY_MS: i64 = 120_000;

const UNSUPPORTED_TEXT: &str = "This station's build cannot install desktop tools from the board.";
const ELEVATION_TEXT: &str = "This action needs the owner's elevation.";
const INVALID_TEXT: &str = "The request is not valid.";

/// Who may use a plan, and the request that claimed it.
struct Binding {
    station: String,
    session_hash: String,
    expires_at: i64,
    /// The request ID that claimed the plan; no other request can use it.
    claimed_by: Option<String>,
}

/// The plans that passed through the deck, by `plan_id`.
static BINDINGS: LazyLock<Mutex<HashMap<String, Binding>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Remembers that `session_hash` read the plan `plan_id` of `station`. The
/// expired entries go at each insert.
fn remember_plan(plan_id: &str, station: &str, session_hash: &str, now: i64) {
    let mut bindings = BINDINGS.lock().unwrap_or_else(PoisonError::into_inner);
    bindings.retain(|_, b| b.expires_at > now);
    bindings.insert(
        plan_id.to_owned(),
        Binding {
            station: station.to_owned(),
            session_hash: session_hash.to_owned(),
            expires_at: now.saturating_add(PLAN_MEMORY_MS),
            claimed_by: None,
        },
    );
}

/// True when the plan `plan_id` is live, belongs to this station and this
/// login, and is free or already claimed by `request_id`. The first call
/// claims the plan. The open does not remove the entry: it expires.
fn claim_plan(
    plan_id: &str,
    station: &str,
    session_hash: &str,
    request_id: &str,
    now: i64,
) -> bool {
    let mut bindings = BINDINGS.lock().unwrap_or_else(PoisonError::into_inner);
    let Some(binding) = bindings.get_mut(plan_id) else {
        return false;
    };
    if binding.expires_at <= now
        || binding.station != station
        || binding.session_hash != session_hash
    {
        return false;
    }
    match &binding.claimed_by {
        None => {
            binding.claimed_by = Some(request_id.to_owned());
            true
        }
        Some(claimed) => claimed == request_id,
    }
}

fn unsupported_tools() -> ApiError {
    ApiError::new(StatusCode::NOT_IMPLEMENTED, "unsupported", UNSUPPORTED_TEXT)
}

fn invalid_request() -> ApiError {
    ApiError::new(StatusCode::BAD_REQUEST, "invalid_request", INVALID_TEXT)
}

fn bad_answer() -> ApiError {
    ApiError::new(
        StatusCode::BAD_GATEWAY,
        "station_error",
        "the station answered with data that the deck does not know",
    )
}

/// A 409 with the name and the fixed text of one of the codes.
fn conflict(name: &'static str) -> ApiError {
    ApiError::new(
        StatusCode::CONFLICT,
        name,
        code::text(name).unwrap_or(INVALID_TEXT),
    )
}

/// The codes of the station that the deck answers with 409.
const CONFLICTS: [&str; 6] = [
    code::TRANSACTION_REFUSED,
    code::MAINTENANCE_BUSY,
    code::REQUEST_ID_CONFLICT,
    code::SHELL_DISABLED,
    code::PLAN_CHANGED,
    code::MAINTENANCE_ENDED,
];

/// The error for a station answer: a fixed status, code and text for each
/// wire code; never the message of the station.
fn wire_failure(e: &WireError) -> ApiError {
    if let Some(name) = CONFLICTS.iter().find(|name| **name == e.code) {
        return conflict(name);
    }
    match e.code.as_str() {
        code::UNKNOWN_MAINTENANCE => ApiError::new(
            StatusCode::NOT_FOUND,
            code::UNKNOWN_MAINTENANCE,
            code::text(code::UNKNOWN_MAINTENANCE).unwrap_or(INVALID_TEXT),
        ),
        "invalid_params" | "invalid_request" => invalid_request(),
        "unavailable" | "unsupported" | "feature_not_negotiated" | "unknown_method" => {
            unsupported_tools()
        }
        _ => ApiError::new(
            StatusCode::BAD_GATEWAY,
            "station_error",
            "the station answered with an error",
        ),
    }
}

fn unreachable_error(rec: &StationRecord) -> ApiError {
    ApiError::new(
        StatusCode::CONFLICT,
        "station_unreachable",
        format!("station `{}` has no live link to this deck", rec.name),
    )
}

/// The station `name`, when its live links did not refuse `feature`. An
/// unknown station is a 404 here; a station without a live link goes on to
/// the call, which answers 409 `station_unreachable`.
async fn require_feature(
    board: &Board,
    name: &str,
    feature: &str,
) -> Result<StationRecord, ApiError> {
    let rec = board
        .store
        .find_station(name.to_owned())
        .await
        .map_err(ApiError::internal)?
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::NOT_FOUND,
                "unknown_station",
                format!("no station is named `{name}`"),
            )
        })?;
    if board.store.link_feature(&rec.station_id, feature) == LinkFeature::Without {
        return Err(unsupported_tools());
    }
    Ok(rec)
}

/// One call of a link method and its parsed answer.
async fn call<T: serde::de::DeserializeOwned>(
    board: &Board,
    name: &str,
    method: &str,
    params: Value,
    wait: Duration,
) -> Result<T, ApiError> {
    let (_, outcome) = station_call_wait(board, name, method, params, wait).await?;
    match outcome {
        Outcome::Ok(v) => serde_json::from_value(v).map_err(|_| bad_answer()),
        Outcome::Err(e) => Err(wire_failure(&e)),
    }
}

/// `GET /api/stations/{station}/desktop-tools`: link method `station.tools`.
///
/// # Errors
/// 403 `owner_only`; 404 `unknown_station`; 409 `station_unreachable`; 501
/// `unsupported`; 502 `station_error`; 504 `timeout`.
pub async fn tools(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    Path(name): Path<String>,
) -> Result<Json<StationTools>, ApiError> {
    owner(
        &board,
        &principal,
        "station.desktop_tools",
        &name,
        "reads the desktop tools of a station",
    )
    .await?;
    require_feature(&board, &name, feature::STATION_TOOLS).await?;
    call(&board, &name, "station.tools", json!({}), TOOLS_WAIT)
        .await
        .map(Json)
}

/// `GET /api/stations/{station}/desktop-tools/plan`: link method
/// `station.desktop_tools_plan`. A plan with a `plan_id` is bound to the
/// station and to the owner login.
///
/// # Errors
/// As [`tools`].
pub async fn plan(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    Path(name): Path<String>,
) -> Result<Json<DesktopToolsPlan>, ApiError> {
    let session = owner(
        &board,
        &principal,
        "station.desktop_tools_plan",
        &name,
        "reads the desktop tools plan of a station",
    )
    .await?;
    require_feature(&board, &name, feature::DESKTOP_TOOL_INSTALL).await?;
    let plan: DesktopToolsPlan = call(
        &board,
        &name,
        "station.desktop_tools_plan",
        json!({}),
        PLAN_WAIT,
    )
    .await?;
    if let Some(plan_id) = &plan.plan_id {
        remember_plan(plan_id, &name, &session.session_hash, board.store.now_ms());
    }
    Ok(Json(plan))
}

/// `POST /api/stations/{station}/maintenance` with the header
/// `x-fleetdeck-elevation`: link method `maintenance.open`.
///
/// # Errors
/// 400 `invalid_request`, `elevation_required`; 403 `owner_only`; 404
/// `unknown_station`; 409 `plan_changed`, `maintenance_busy`,
/// `request_id_conflict`, `shell_disabled`, `transaction_refused`,
/// `station_unreachable`; 501 `unsupported`; 502 `station_error`; 504
/// `timeout`.
pub async fn open(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    Path(name): Path<String>,
    headers: HeaderMap,
    body: Result<Json<MaintenanceOpen>, JsonRejection>,
) -> Result<Json<MaintenanceInfo>, ApiError> {
    let session = owner(
        &board,
        &principal,
        "station.maintenance_open",
        &name,
        "installs desktop tools on a station",
    )
    .await?;
    let actor = principal.actor();
    let mut request = None;
    let result = run_open(&board, &name, &session, &headers, body, &mut request).await;
    audit_open(&board, &actor, &name, request.as_deref(), &result).await;
    if let Ok(run) = &result {
        watch_completion(Arc::clone(&board), actor, name, run.maintenance_id.clone());
    }
    result.map(Json)
}

/// The checks and the call of an open. `request` gets the request ID for the
/// audit when it is a valid one.
async fn run_open(
    board: &Board,
    name: &str,
    session: &LoginSession,
    headers: &HeaderMap,
    body: Result<Json<MaintenanceOpen>, JsonRejection>,
    request: &mut Option<String>,
) -> Result<MaintenanceInfo, ApiError> {
    require_feature(board, name, feature::DESKTOP_TOOL_INSTALL).await?;
    let Json(open) = body.map_err(|_| invalid_request())?;
    if is_hex_id(&open.request_id) {
        *request = Some(open.request_id.clone());
    }
    let token = headers.get(ELEVATION_HEADER).and_then(|v| v.to_str().ok());
    if !is_elevated(board, session, token) {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "elevation_required",
            ELEVATION_TEXT,
        ));
    }
    if !is_hex_id(&open.request_id) || !is_hex_id(&open.plan_id) || !size_ok(open.cols, open.rows) {
        return Err(invalid_request());
    }
    if !claim_plan(
        &open.plan_id,
        name,
        &session.session_hash,
        &open.request_id,
        board.store.now_ms(),
    ) {
        return Err(conflict(code::PLAN_CHANGED));
    }
    let params = serde_json::to_value(&open).map_err(ApiError::internal)?;
    call(board, name, "maintenance.open", params, OPEN_WAIT).await
}

/// The `station.maintenance` row of an open: `stage=opened` for a success,
/// `stage=refused` with the code of the error for any refusal. No password,
/// no terminal byte, no message.
async fn audit_open(
    board: &Board,
    actor: &str,
    station: &str,
    request: Option<&str>,
    result: &Result<MaintenanceInfo, ApiError>,
) {
    let request = request.unwrap_or("-");
    let row = match result {
        Ok(run) => {
            NewAudit::new(actor, maintenance_audit::ACTION, AuditResult::Ok).detail(format!(
                "stage=opened request={request} maintenance={} tools={}",
                run.maintenance_id,
                run.tools.join(",")
            ))
        }
        Err(e) => NewAudit::new(actor, maintenance_audit::ACTION, AuditResult::Refused)
            .detail(format!("stage=refused code={} request={request}", e.code)),
    };
    board.store.audit_or_log(row.target(station)).await;
}

/// `GET /api/stations/{station}/maintenance`: link method `maintenance.list`.
/// Each run that has ended also gets its completion row, once.
///
/// # Errors
/// 403 `owner_only`; 404 `unknown_station`; 409 `station_unreachable`; 501
/// `unsupported`; 502 `station_error`; 504 `timeout`.
pub async fn list(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    Path(name): Path<String>,
) -> Result<Json<MaintenanceList>, ApiError> {
    owner(
        &board,
        &principal,
        "station.maintenance_list",
        &name,
        "lists the installations of a station",
    )
    .await?;
    require_feature(&board, &name, feature::DESKTOP_TOOL_INSTALL).await?;
    let runs: MaintenanceList =
        call(&board, &name, "maintenance.list", json!({}), SHORT_WAIT).await?;
    let actor = principal.actor();
    for run in runs
        .runs
        .iter()
        .filter(|run| matches!(run.state, MaintenanceState::Exited | MaintenanceState::Lost))
    {
        audit_completion(&board, &actor, &name, run).await;
    }
    Ok(Json(runs))
}

/// The body of the resize route.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResizeBody {
    cols: u16,
    rows: u16,
}

/// `POST /api/stations/{station}/maintenance/{id}/resize`: link method
/// `maintenance.resize`.
///
/// # Errors
/// 400 `invalid_request`; 403 `owner_only`; 404 `unknown_station`,
/// `unknown_maintenance`; 409 `station_unreachable`, `maintenance_ended`;
/// 501 `unsupported`; 502 `station_error`; 504 `timeout`.
pub async fn resize(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    Path((name, id)): Path<(String, String)>,
    body: Result<Json<ResizeBody>, JsonRejection>,
) -> Result<StatusCode, ApiError> {
    owner(
        &board,
        &principal,
        "station.maintenance_resize",
        &name,
        "resizes the installation terminal of a station",
    )
    .await?;
    require_feature(&board, &name, feature::DESKTOP_TOOL_INSTALL).await?;
    let Json(size) = body.map_err(|_| invalid_request())?;
    if !is_hex_id(&id) || !size_ok(size.cols, size.rows) {
        return Err(invalid_request());
    }
    let params = serde_json::to_value(MaintenanceResize {
        maintenance_id: id,
        cols: size.cols,
        rows: size.rows,
    })
    .map_err(ApiError::internal)?;
    call::<Value>(&board, &name, "maintenance.resize", params, SHORT_WAIT).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `GET /api/stations/{station}/maintenance/{id}/attach` (WebSocket): link
/// method `maintenance.attach`, then the relay of a shell.
///
/// # Errors
/// Before the upgrade: 400 `invalid_request`; 403 `owner_only`; 404
/// `unknown_station`, `unknown_maintenance`; 409 `station_unreachable`; 501
/// `unsupported`; 502 `station_error`; 504 `timeout`.
pub async fn attach(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    Path((name, id)): Path<(String, String)>,
    ticket: Option<Extension<Ticket>>,
    ws: WebSocketUpgrade,
) -> Result<Response, ApiError> {
    owner_only(&principal, "attaches to an installation terminal")?;
    let rec = require_feature(&board, &name, feature::DESKTOP_TOOL_INSTALL).await?;
    if !is_hex_id(&id) {
        return Err(invalid_request());
    }
    let (control, hub) = board
        .link
        .data_for(&rec.station_id)
        .ok_or_else(|| unreachable_error(&rec))?;
    let params = json!(MaintenanceAttach { maintenance_id: id });
    let outcome = tokio::time::timeout(
        SHORT_WAIT,
        control.request("maintenance.attach", None, params),
    )
    .await
    .map_err(|_| {
        ApiError::new(
            StatusCode::GATEWAY_TIMEOUT,
            "timeout",
            "the station did not answer in time",
        )
    })?
    .map_err(|_| unreachable_error(&rec))?;
    let attached: ShellAttached = match outcome {
        Outcome::Ok(v) => serde_json::from_value(v).map_err(|_| bad_answer())?,
        Outcome::Err(e) => return Err(wire_failure(&e)),
    };
    let stream = hub
        .open(&attached.stream_token)
        .await
        .map_err(|_| unreachable_error(&rec))?;
    Ok(ws
        .max_message_size(MAX_INPUT_MESSAGE)
        .max_frame_size(MAX_INPUT_MESSAGE)
        .on_upgrade(move |socket| {
            let fence = ticket.map(|Extension(t)| t);
            relay(board, principal, socket, stream, fence)
        })
        .into_response())
}

#[cfg(test)]
mod tests {
    use super::{claim_plan, remember_plan};

    #[test]
    fn a_plan_is_claimed_by_one_request_of_one_login_and_expires() {
        remember_plan("unit-plan-1", "fake", "login-a", 1_000);
        assert!(!claim_plan("unit-plan-2", "fake", "login-a", "r1", 1_001));
        assert!(!claim_plan("unit-plan-1", "other", "login-a", "r1", 1_001));
        assert!(!claim_plan("unit-plan-1", "fake", "login-b", "r1", 1_001));
        assert!(claim_plan("unit-plan-1", "fake", "login-a", "r1", 1_001));
        assert!(claim_plan("unit-plan-1", "fake", "login-a", "r1", 1_002));
        assert!(!claim_plan("unit-plan-1", "fake", "login-a", "r2", 1_002));
        assert!(!claim_plan("unit-plan-1", "fake", "login-a", "r1", 121_000));
    }
}
