//! Provider CLI updates of a station (B28, design `cli-update.md` §3.11).
//!
//! - `GET /api/stations/{station}/cli-update` forwards `cli_update.status`
//!   and returns a `CliUpdateReport`.
//! - `POST /api/stations/{station}/cli-update { provider, check_only, force,
//!   elevation }` forwards `cli_update.run` and returns a
//!   `CliUpdateRunAnswer`. A check needs no elevation; an update does.
//!
//! Only the owner's board login calls them. The deck never sends the text
//! of a station to the board: the error texts are fixed. The POST is audited
//! with the actor `owner`.

use std::sync::Arc;
use std::time::Duration;

use axum::Json;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Extension, Path, State};
use axum::http::StatusCode;
use fleetdeck_proto::cli_update::{CliUpdateReport, CliUpdateRun, CliUpdateRunAnswer};
use fleetdeck_proto::event::ProviderKind;
use fleetdeck_proto::link::{Outcome, WireError};
use fleetdeck_proto::version::feature;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::json;

use super::Board;
use super::guard::ApiError;
use super::settings_routes::{is_elevated, owner, station_call_wait};
use crate::store::{AuditResult, LinkFeature, NewAudit, Principal};

/// The longest the deck waits for `cli_update.status`.
const STATUS_WAIT: Duration = Duration::from_secs(10);
/// The longest the deck waits for `cli_update.run`.
const RUN_WAIT: Duration = Duration::from_secs(90);
/// The audit action of a check.
pub const ACTION_CHECK: &str = "cli_update.check";
/// The audit action of an update.
pub const ACTION_UPDATE: &str = "cli_update.update";
/// The text of a station that cannot update.
const UNSUPPORTED_TEXT: &str = "This station cannot update its provider CLIs yet. Update it.";

/// The body of the POST.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunBody {
    provider: ProviderKind,
    #[serde(default)]
    check_only: bool,
    #[serde(default)]
    force: bool,
    #[serde(default)]
    elevation: Option<String>,
}

fn unsupported() -> ApiError {
    ApiError::new(
        StatusCode::NOT_IMPLEMENTED,
        "cli_update_unsupported",
        UNSUPPORTED_TEXT,
    )
}

fn invalid_body() -> ApiError {
    ApiError::new(
        StatusCode::BAD_REQUEST,
        "invalid_body",
        "the body must be {\"provider\": \"claude\" | \"codex\", \"check_only\": bool, \"force\": bool, \"elevation\": string | null}",
    )
}

/// 400 `elevation_required` with `keys: ["update"]`.
fn elevation_required() -> ApiError {
    let mut keys = serde_json::Map::new();
    keys.insert("keys".to_owned(), json!(["update"]));
    ApiError::new(
        StatusCode::BAD_REQUEST,
        "elevation_required",
        "a recent sign-in is needed to update a provider",
    )
    .with_details(keys)
}

/// The HTTP form of a station error. The text of the station never goes to
/// the answer.
#[must_use]
pub fn station_error(e: &WireError) -> ApiError {
    match e.code.as_str() {
        "unsupported" | "unknown_method" => unsupported(),
        _ => ApiError::new(
            StatusCode::BAD_GATEWAY,
            "station_error",
            "the station could not answer",
        ),
    }
}

/// The audit row of a finished POST (§3.11): the actor `owner`, the action
/// of the request, the target `<station>/<provider>`, and the result.
#[must_use]
pub fn run_audit(
    station: &str,
    body: &RunBody,
    result: &Result<CliUpdateRunAnswer, ApiError>,
) -> NewAudit {
    let action = if body.check_only {
        ACTION_CHECK
    } else {
        ACTION_UPDATE
    };
    let provider = serde_json::to_value(body.provider)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_else(|| "unknown".to_owned());
    let row = match result {
        Ok(a) => match &a.refused {
            None => NewAudit::new("owner", action, AuditResult::Ok),
            Some(code) => {
                NewAudit::new("owner", action, AuditResult::Refused).detail(detail(code, body))
            }
        },
        Err(e) => {
            let kind = if matches!(
                e.status,
                StatusCode::BAD_REQUEST | StatusCode::FORBIDDEN | StatusCode::NOT_FOUND
            ) {
                AuditResult::Refused
            } else {
                AuditResult::Failed
            };
            NewAudit::new("owner", action, kind).detail(detail(e.code, body))
        }
    };
    let row = match (&row.detail, result) {
        (None, Ok(_)) if body.force => row.detail("force"),
        _ => row,
    };
    row.target(format!("{station}/{provider}"))
}

/// The detail of a refusal: its code, and `force` when the body had it.
fn detail(code: &str, body: &RunBody) -> String {
    if body.force {
        format!("{code} force")
    } else {
        code.to_owned()
    }
}

/// The checked forward: the station, the feature, then the call.
async fn forward<T: DeserializeOwned>(
    board: &Board,
    station: &str,
    method: &'static str,
    params: serde_json::Value,
    wait: Duration,
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
        .link_feature(&rec.station_id, feature::CLI_UPDATE)
    {
        LinkFeature::Down => {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "station_unreachable",
                format!("station `{}` has no live link to this deck", rec.name),
            ));
        }
        LinkFeature::Without => return Err(unsupported()),
        LinkFeature::Negotiated => {}
    }
    let (_, outcome) = station_call_wait(board, &rec.name, method, params, wait).await?;
    match outcome {
        Outcome::Ok(v) => serde_json::from_value(v).map_err(|_| {
            // The decode error can quote the answer: it is not logged.
            tracing::warn!(method, "the station sent an answer that does not parse");
            ApiError::new(
                StatusCode::BAD_GATEWAY,
                "station_error",
                "the station sent an answer that the deck cannot read",
            )
        }),
        Outcome::Err(e) => {
            tracing::warn!(code = %e.code, method, "the station refused a provider update call");
            Err(station_error(&e))
        }
    }
}

/// `GET /api/stations/{station}/cli-update`.
///
/// # Errors
/// 403 `owner_only`; 404 `unknown_station`; 409 `station_unreachable`; 501
/// `cli_update_unsupported`; 502 `station_error`; 504 `timeout`.
pub async fn status(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    Path(station): Path<String>,
) -> Result<Json<CliUpdateReport>, ApiError> {
    owner(
        &board,
        &principal,
        "station.cli_update_status",
        &station,
        "reads the provider updates of a station",
    )
    .await?;
    forward(
        &board,
        &station,
        "cli_update.status",
        json!({}),
        STATUS_WAIT,
    )
    .await
    .map(Json)
}

/// `POST /api/stations/{station}/cli-update`.
///
/// # Errors
/// 400 `invalid_body`, `elevation_required` (`keys: ["update"]`); 403
/// `owner_only`; 404 `unknown_station`; 409 `station_unreachable`; 501
/// `cli_update_unsupported`; 502 `station_error`; 504 `timeout`.
pub async fn run(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    Path(station): Path<String>,
    body: Result<Json<RunBody>, JsonRejection>,
) -> Result<Json<CliUpdateRunAnswer>, ApiError> {
    let session = owner(
        &board,
        &principal,
        "station.cli_update_run",
        &station,
        "updates the provider CLIs of a station",
    )
    .await?;
    // A rejection text could quote the body: the answer is fixed.
    let Ok(Json(body)) = body else {
        let e = invalid_body();
        let row = NewAudit::new("owner", ACTION_UPDATE, AuditResult::Refused)
            .target(station)
            .detail(e.code);
        board.store.audit_or_log(row).await;
        return Err(e);
    };
    let result = if body.check_only || is_elevated(&board, &session, body.elevation.as_deref()) {
        let params = json!(CliUpdateRun {
            provider: body.provider,
            check_only: body.check_only,
            force: body.force,
        });
        forward(&board, &station, "cli_update.run", params, RUN_WAIT).await
    } else {
        Err(elevation_required())
    };
    board
        .store
        .audit_or_log(run_audit(&station, &body, &result))
        .await;
    result.map(Json)
}

#[cfg(test)]
#[path = "cli_update_routes_tests.rs"]
mod tests;
