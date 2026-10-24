//! The MCP App routes of the board API (B15, design `mcp-apps.md` §4.7).
//!
//! - `GET /api/sessions/{station}/{session}/apps/{app_id}` (`app_views`)
//! - `POST /api/sessions/{station}/{session}/apps/{app_id}/call` (here)
//! - `GET /api/sessions/{station}/{session}/app-calls` (`app_views`)
//! - `POST /api/sessions/{station}/{session}/app-calls/{call_id}/release`
//!   (`app_views`)
//!
//! Only the owner's board login uses them. This file holds what the routes
//! share: the forward to the station, the error table and the audit rows.

use std::sync::Arc;
use std::time::Duration;

use axum::Json;
use axum::body::Bytes;
use axum::extract::rejection::BytesRejection;
use axum::extract::{Extension, Path, State};
use axum::http::StatusCode;
use fleetdeck_proto::SessionId;
use fleetdeck_proto::StationId;
use fleetdeck_proto::apps::{AppCallBody, AppCallParams, AppCallReply, AppRequest};
use fleetdeck_proto::link::Outcome;
use fleetdeck_proto::version::feature;
use serde::de::DeserializeOwned;

use super::Board;
use super::access::session_of;
use super::app_public::AppTokens;
use super::guard::{ApiError, owner_only};
use crate::store::{AuditResult, LinkFeature, MemberAction, NewAudit, Principal};

/// The longest wait for `session.app_call`.
const CALL_WAIT: Duration = Duration::from_secs(70);
/// The longest body of the call route.
pub(super) const MAX_CALL_BODY: usize = 128 * 1024;
/// The longest session or app id of a request, in characters.
const MAX_ID_CHARS: usize = 128;
/// The longest tool name in an audit row, in characters.
const MAX_TOOL_CHARS: usize = 128;
/// The longest station text that goes to the answer, in characters.
const MAX_TEXT_CHARS: usize = 1000;
/// The text of a station that does not offer the feature.
const OLDER_STATION: &str = "This station runs an older Fleet Deck. On that computer, install the new version, then run `fleetdeck service start`.";
/// The characters of a ULID.
const ULID_ALPHABET: &[u8] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// The station error codes that map to their own HTTP status and keep the
/// station's text (design §4.7): code, status, the code of the answer.
const ERROR_TABLE: &[(&str, u16)] = &[
    ("unknown_session", 404),
    ("unknown_app", 404),
    ("unknown_call", 404),
    ("session_stopped", 409),
    ("call_running", 409),
    ("app_gone", 410),
    ("invalid_params", 400),
    ("tool_not_callable", 403),
    ("too_large", 413),
    ("busy", 429),
    ("provider_error", 502),
    ("timeout", 504),
];

/// The answer code for a station code of [`ERROR_TABLE`].
fn static_code(code: &str) -> &'static str {
    match code {
        "unknown_session" => "unknown_session",
        "unknown_app" => "unknown_app",
        "unknown_call" => "unknown_call",
        "session_stopped" => "session_stopped",
        "call_running" => "call_running",
        "app_gone" => "app_gone",
        "invalid_params" => "invalid_params",
        "tool_not_callable" => "tool_not_callable",
        "too_large" => "too_large",
        "busy" => "busy",
        "provider_error" => "provider_error",
        "timeout" => "timeout",
        _ => "station_error",
    }
}

/// `text` without control characters, cut to `max` characters.
fn clean(text: &str, max: usize) -> String {
    text.chars().filter(|c| !c.is_control()).take(max).collect()
}

/// Maps a station error to an HTTP error. A code of [`ERROR_TABLE`] keeps its
/// status and the station's text (without control characters, at most
/// 1000 characters), because the board sends that text to the app. Every
/// other code gives 502 `station_error` with a fixed text.
pub(super) fn map_station_error(code: &str, message: &str) -> ApiError {
    match ERROR_TABLE.iter().find(|(c, _)| *c == code) {
        Some((c, status)) => ApiError::new(
            StatusCode::from_u16(*status).unwrap_or(StatusCode::BAD_GATEWAY),
            static_code(c),
            clean(message, MAX_TEXT_CHARS),
        ),
        None => station_failed("the station could not do the app request"),
    }
}

pub(super) fn station_failed(text: &'static str) -> ApiError {
    ApiError::new(StatusCode::BAD_GATEWAY, "station_error", text)
}

fn unreachable_station(station: &str) -> ApiError {
    ApiError::new(
        StatusCode::CONFLICT,
        "station_unreachable",
        format!("station `{station}` has no live link to this deck"),
    )
}

pub(super) fn bad(message: &'static str) -> ApiError {
    ApiError::new(StatusCode::BAD_REQUEST, "invalid_params", message)
}

/// True when `id` is short and holds no control character.
pub(super) fn plain(id: &str) -> bool {
    id.chars().count() <= MAX_ID_CHARS && !id.chars().any(char::is_control)
}

/// True for 26 characters of the ULID alphabet.
pub(super) fn is_ulid(id: &str) -> bool {
    id.len() == 26 && id.bytes().all(|b| ULID_ALPHABET.contains(&b))
}

/// The checks of a route before its forward: the owner's login, plain ids,
/// the station and the session, and the feature. Returns the station id.
pub(super) async fn checked(
    board: &Board,
    principal: &Principal,
    station: &str,
    session: &str,
    what: &str,
) -> Result<StationId, ApiError> {
    owner_only(principal, what)?;
    if !plain(session) {
        return Err(bad("the session id is not valid"));
    }
    ready(board, principal, station, session, what).await
}

/// The station and session lookup and the feature check (after the owner
/// and id checks).
pub(super) async fn ready(
    board: &Board,
    principal: &Principal,
    station: &str,
    session: &str,
    what: &str,
) -> Result<StationId, ApiError> {
    let station_id =
        session_of(board, principal, station, session, MemberAction::Read, what).await?;
    match board.store.link_feature(&station_id, feature::MCP_APPS) {
        LinkFeature::Down => Err(unreachable_station(station)),
        LinkFeature::Without => Err(ApiError::new(
            StatusCode::CONFLICT,
            "apps_unsupported",
            OLDER_STATION,
        )),
        LinkFeature::Negotiated => Ok(station_id),
    }
}

/// Sends `method` to the station and reads its answer. A link failure and an
/// answer that does not parse are 502 `station_error`.
pub(super) async fn forward<T: DeserializeOwned>(
    board: &Board,
    station: &str,
    station_id: &StationId,
    method: &'static str,
    params: serde_json::Value,
    wait: Duration,
) -> Result<T, ApiError> {
    let control = board
        .link
        .control_for(station_id)
        .ok_or_else(|| unreachable_station(station))?;
    let outcome = tokio::time::timeout(wait, control.request(method, None, params))
        .await
        .map_err(|_| {
            ApiError::new(
                StatusCode::GATEWAY_TIMEOUT,
                "timeout",
                "the station did not answer in time",
            )
        })?
        .map_err(|_| station_failed("the station could not be reached"))?;
    match outcome {
        Outcome::Ok(v) => serde_json::from_value(v).map_err(|_| {
            // The decode error can quote the answer: it is not logged.
            tracing::warn!(method, "the station sent an answer that does not parse");
            station_failed("the station sent an answer that the deck cannot read")
        }),
        Outcome::Err(e) => Err(map_station_error(&e.code, &e.message)),
    }
}

/// The audit result of an error: `Refused` for 400, 403 and 404, `Failed`
/// for every other status.
pub(super) fn audit_result_of(status: StatusCode) -> AuditResult {
    if matches!(
        status,
        StatusCode::BAD_REQUEST | StatusCode::FORBIDDEN | StatusCode::NOT_FOUND
    ) {
        AuditResult::Refused
    } else {
        AuditResult::Failed
    }
}

/// The audit detail of a call: `{app_id}/{tool} {word}`. `app_id` is `-`
/// unless it is a ULID; `tool` has no control characters and at most 128
/// characters.
pub(super) fn call_detail(app_id: &str, tool: &str, word: &str) -> String {
    let app = if is_ulid(app_id) { app_id } else { "-" };
    format!("{app}/{} {word}", clean(tool, MAX_TOOL_CHARS))
}

/// The audit detail of a release: `{call_id} {word}`; `- invalid_params` for
/// a call id that is not a ULID.
pub(super) fn release_detail(call_id: &str, word: &str) -> String {
    if is_ulid(call_id) {
        format!("{call_id} {word}")
    } else {
        "- invalid_params".to_owned()
    }
}

/// Writes one audit row of an app route. `result` is the outcome of the
/// route; `word` is the word of the detail.
pub(super) async fn write_audit(
    board: &Board,
    principal: &Principal,
    action: &'static str,
    station: &str,
    session: &str,
    result: AuditResult,
    detail: String,
) {
    // The names come from the path: keep them out of the log when they are
    // not plain.
    let target = if plain(station) && plain(session) {
        format!("{station}/{session}")
    } else {
        String::new()
    };
    let row = NewAudit::new(principal.actor(), action, result)
        .detail(detail)
        .target(target);
    board.store.audit_or_log(row).await;
}

/// The login hash of the owner, for the call rate limit.
fn login_hash(principal: &Principal) -> &str {
    match principal {
        Principal::Owner(s) => &s.session_hash,
        _ => "",
    }
}

/// The word of the audit detail of a call answer.
fn call_word(reply: &Result<AppCallReply, ApiError>) -> &'static str {
    match reply {
        Ok(AppCallReply::Done { .. }) => "ok",
        Ok(AppCallReply::Confirm { .. }) => "confirm",
        Err(e) => e.code,
    }
}

/// `POST /api/sessions/{station}/{session}/apps/{app_id}/call`: one
/// `tools/call` or `resources/read` of the app.
///
/// # Errors
/// 400 `invalid_params`; 403 `owner_only`, `tool_not_callable`; 404
/// `unknown_station`, `unknown_session`, `unknown_app`; 409
/// `apps_unsupported`, `station_unreachable`, `session_stopped`,
/// `call_running`; 410 `app_gone`; 413 `too_large`; 429 `busy`; 502
/// `provider_error`, `station_error`; 504 `timeout`.
pub async fn call(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    Extension(tokens): Extension<Arc<AppTokens>>,
    Path((station, session, app_id)): Path<(String, String, String)>,
    body: Result<Bytes, BytesRejection>,
) -> Result<Json<AppCallReply>, ApiError> {
    const WHAT: &str = "uses an app";
    owner_only(&principal, WHAT)?;
    if !plain(&session) || !plain(&app_id) {
        return Err(bad("the session id or the app id is not valid"));
    }
    let bytes = body.map_err(|e| {
        if e.status() == StatusCode::PAYLOAD_TOO_LARGE {
            ApiError::new(
                StatusCode::PAYLOAD_TOO_LARGE,
                "too_large",
                "the request of the app is too large",
            )
        } else {
            bad("the request body could not be read")
        }
    })?;
    if bytes.len() > MAX_CALL_BODY {
        return Err(ApiError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "too_large",
            "the request of the app is too large",
        ));
    }
    let parsed: AppCallBody =
        serde_json::from_slice(&bytes).map_err(|_| bad("the request of the app is not valid"))?;
    let tool = match &parsed.request {
        AppRequest::ToolsCall { name, .. } => Some(name.clone()),
        AppRequest::ResourcesRead { .. } => None,
    };
    let result: Result<AppCallReply, ApiError> = async {
        if !tokens.allow_call(login_hash(&principal), board.store.now_ms()) {
            return Err(ApiError::new(
                StatusCode::TOO_MANY_REQUESTS,
                "busy",
                "The app sends too many requests. Wait a moment.",
            ));
        }
        let station_id = ready(&board, &principal, &station, &session, WHAT).await?;
        let params = serde_json::to_value(AppCallParams {
            session_id: SessionId::from(session.as_str()),
            app_id: app_id.clone(),
            request: parsed.request,
            confirmed: parsed.confirmed,
        })
        .map_err(ApiError::internal)?;
        forward(
            &board,
            &station,
            &station_id,
            "session.app_call",
            params,
            CALL_WAIT,
        )
        .await
    }
    .await;
    if let Some(tool) = tool {
        let kind = match &result {
            Ok(_) => AuditResult::Ok,
            Err(e) => audit_result_of(e.status),
        };
        let detail = call_detail(&app_id, &tool, call_word(&result));
        write_audit(
            &board,
            &principal,
            "session.app_call",
            &station,
            &session,
            kind,
            detail,
        )
        .await;
    }
    result.map(Json)
}

#[cfg(test)]
#[path = "app_routes_tests.rs"]
mod tests;
