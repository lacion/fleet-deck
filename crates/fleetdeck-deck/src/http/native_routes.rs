//! Browse the native sessions of a station (B7, `docs/v2/design/native-sessions.md`
//! 2.5).
//!
//! - `GET /api/stations/{station}/native-sessions?instance=&limit=` forwards
//!   `station.native_sessions` and returns a `NativeSessionList`.
//! - `POST /api/stations/{station}/native-sessions/preview` forwards
//!   `station.native_preview` and returns a `NativePreview`.
//!
//! Only the owner's board login reads them: a member and a bearer token get
//! 403 `owner_only`. The deck stores and caches nothing, and it never logs a
//! field of the answer. The text of the station never goes to the answer.

use std::sync::Arc;
use std::time::Duration;

use axum::Json;
use axum::extract::rejection::{JsonRejection, QueryRejection};
use axum::extract::{Extension, Path, Query, State};
use axum::http::StatusCode;
use fleetdeck_proto::history::code;
use fleetdeck_proto::link::Outcome;
use fleetdeck_proto::native::{
    NativePreview, NativePreviewQuery, NativeSessionList, NativeSessionsQuery,
};
use fleetdeck_proto::version::feature;
use fleetdeck_proto::{ProviderInstanceId, command::check_native_session_id};
use serde::Deserialize;
use serde::de::DeserializeOwned;

use super::Board;
use super::guard::{ApiError, owner_only};
use crate::store::{AuditResult, LinkFeature, NewAudit, Principal};

/// The longest the deck waits for the station's answer. The station's own
/// budget is 10 s.
const NATIVE_WAIT: Duration = Duration::from_secs(15);
/// The longest station name that goes into an audit target, in characters.
const MAX_STATION_CHARS: usize = 128;
/// The longest `home` of a pin, in bytes.
const MAX_HOME_BYTES: usize = 4096;
/// The text of a station that does not offer the feature.
const UNSUPPORTED_TEXT: &str = "This station does not list its sessions yet. Update it.";

/// What differs between the two routes.
struct Route {
    /// The audit action.
    action: &'static str,
    /// The link method.
    method: &'static str,
    /// What the owner's login does, for the `owner_only` text.
    what: &'static str,
}

const LIST: Route = Route {
    action: "stations.native_sessions",
    method: "station.native_sessions",
    what: "lists the sessions of a station",
};

const PREVIEW: Route = Route {
    action: "stations.native_preview",
    method: "station.native_preview",
    what: "previews a session of a station",
};

/// The query string of the list route.
#[derive(Debug, Deserialize)]
pub struct ListParams {
    #[serde(default)]
    instance: Option<String>,
    #[serde(default)]
    limit: Option<u32>,
}

fn bad(message: &'static str) -> ApiError {
    ApiError::new(StatusCode::BAD_REQUEST, "invalid_params", message)
}

fn unreachable_station(station: &str) -> ApiError {
    ApiError::new(
        StatusCode::CONFLICT,
        "station_unreachable",
        format!("station `{station}` has no live link to this deck"),
    )
}

/// True for an instance id: the rule of the station configuration (1 to 32 of
/// `a-z0-9_-`, starting with a letter or a digit).
fn instance_ok(id: &str) -> bool {
    let b = id.as_bytes();
    (1..=32).contains(&b.len())
        && (b[0].is_ascii_lowercase() || b[0].is_ascii_digit())
        && b.iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, b'-' | b'_'))
}

/// True when `name` is short and holds no control character.
fn plain_name(name: &str) -> bool {
    name.chars().count() <= MAX_STATION_CHARS && !name.chars().any(char::is_control)
}

/// Maps a station error code to a fixed HTTP error. The text of the station
/// never goes to the answer.
fn station_error(code_text: &str) -> ApiError {
    match code_text {
        code::BUSY => ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "busy",
            "The station is busy. Try again in a moment.",
        ),
        code::INVALID_PARAMS => ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_params",
            "The station refused the request.",
        ),
        _ => ApiError::new(
            StatusCode::BAD_GATEWAY,
            "station_error",
            "The station could not answer.",
        ),
    }
}

/// The checked forward: the station, the feature, then the call.
async fn read<T: DeserializeOwned>(
    board: &Board,
    station: &str,
    route: &Route,
    params: serde_json::Value,
) -> Result<T, ApiError> {
    let rec = board
        .store
        .find_station(station.to_owned())
        .await
        .map_err(ApiError::internal)?
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::NOT_FOUND,
                "unknown_station",
                format!("no station is named `{station}`"),
            )
        })?;
    match board
        .store
        .link_feature(&rec.station_id, feature::NATIVE_SESSIONS)
    {
        LinkFeature::Down => return Err(unreachable_station(station)),
        LinkFeature::Without => {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "native_sessions_unsupported",
                UNSUPPORTED_TEXT,
            ));
        }
        LinkFeature::Negotiated => {}
    }
    let control = board
        .link
        .control_for(&rec.station_id)
        .ok_or_else(|| unreachable_station(station))?;
    let outcome = tokio::time::timeout(NATIVE_WAIT, control.request(route.method, None, params))
        .await
        .map_err(|_| {
            ApiError::new(
                StatusCode::GATEWAY_TIMEOUT,
                "timeout",
                "the station did not answer in time",
            )
        })?
        .map_err(|_| unreachable_station(station))?;
    match outcome {
        Outcome::Ok(v) => serde_json::from_value(v).map_err(|_| {
            // The decode error can quote the answer: it is not logged.
            tracing::warn!(
                method = route.method,
                "the station sent an answer that does not parse"
            );
            ApiError::new(
                StatusCode::BAD_GATEWAY,
                "station_error",
                "the station sent an answer that the deck cannot read",
            )
        }),
        Outcome::Err(e) => {
            if !matches!(e.code.as_str(), code::INVALID_PARAMS | code::BUSY) {
                tracing::warn!(code = %e.code, method = route.method, "the station failed a read");
            }
            Err(station_error(&e.code))
        }
    }
}

/// Writes the audit row of a failure. The owner's successful read writes no
/// row. The detail is the error code, never a text of the station.
async fn audit(
    board: &Board,
    principal: &Principal,
    station: &str,
    route: &Route,
    error: Option<&ApiError>,
) {
    let Some(e) = error else { return };
    let kind = if matches!(
        e.status,
        StatusCode::BAD_REQUEST | StatusCode::FORBIDDEN | StatusCode::NOT_FOUND
    ) {
        AuditResult::Refused
    } else {
        AuditResult::Failed
    };
    let row = NewAudit::new(principal.actor(), route.action, kind).detail(e.code);
    // The name comes from the path: keep it out of the log when it is not
    // plain.
    let target = if plain_name(station) {
        station.to_owned()
    } else {
        String::new()
    };
    board.store.audit_or_log(row.target(target)).await;
}

/// `GET /api/stations/{station}/native-sessions`.
///
/// # Errors
/// 400 `invalid_params`; 403 `owner_only`; 404 `unknown_station`; 409
/// `station_unreachable`, `native_sessions_unsupported`; 502 `station_error`;
/// 503 `busy`; 504 `timeout`.
pub async fn list(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    Path(station): Path<String>,
    query: Result<Query<ListParams>, QueryRejection>,
) -> Result<Json<NativeSessionList>, ApiError> {
    let result = async {
        owner_only(&principal, LIST.what)?;
        let Query(params) = query.map_err(|_| bad("the query is not valid"))?;
        if let Some(instance) = &params.instance
            && !instance_ok(instance)
        {
            return Err(bad("the instance id is not valid"));
        }
        if let Some(limit) = params.limit
            && !(1..=200).contains(&limit)
        {
            return Err(bad("the limit must be 1 to 200"));
        }
        let query = NativeSessionsQuery {
            provider_instance: params.instance.map(ProviderInstanceId::from),
            limit: params.limit,
        };
        read(
            &board,
            &station,
            &LIST,
            serde_json::to_value(query).map_err(ApiError::internal)?,
        )
        .await
    }
    .await;
    audit(&board, &principal, &station, &LIST, result.as_ref().err()).await;
    result.map(Json)
}

/// `POST /api/stations/{station}/native-sessions/preview`.
///
/// # Errors
/// 400 `invalid_body`, `invalid_params`; otherwise as [`list`].
pub async fn preview(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    Path(station): Path<String>,
    body: Result<Json<NativePreviewQuery>, JsonRejection>,
) -> Result<Json<NativePreview>, ApiError> {
    let result = async {
        owner_only(&principal, PREVIEW.what)?;
        let Json(query) = body.map_err(|_| {
            ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_body",
                "The request body is not valid.",
            )
        })?;
        if !instance_ok(&query.provider_instance.0) {
            return Err(bad("the instance id is not valid"));
        }
        if check_native_session_id(&query.native_id).is_err() {
            return Err(bad("the session id is not valid"));
        }
        let home = &query.pin.home;
        if !home.starts_with('/') || home.len() > MAX_HOME_BYTES || home.contains('\0') {
            return Err(bad("the pin is not valid"));
        }
        read(
            &board,
            &station,
            &PREVIEW,
            serde_json::to_value(query).map_err(ApiError::internal)?,
        )
        .await
    }
    .await;
    audit(
        &board,
        &principal,
        &station,
        &PREVIEW,
        result.as_ref().err(),
    )
    .await;
    result.map(Json)
}
