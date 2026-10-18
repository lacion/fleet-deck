//! The Omarchy routes of a station (design `omarchy-admin.md` revision 5
//! section 4): `GET` reads the status and `POST` runs one action, through
//! the link methods `omarchy.status` and `omarchy.act`.
//!
//! Both routes are owner only. The deck checks the owner's elevation for
//! every action (header `x-fleetdeck-elevation`) and tells the station with
//! `elevated: true`. The deck answers with its own fixed text for each
//! station error, never with the text of the station. Each `POST` writes one
//! audit row (`station.omarchy`) that holds the action, the outcome and the
//! code, never a message.

use std::sync::Arc;
use std::time::Duration;

use axum::Json;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Extension, Path, State};
use axum::http::{HeaderMap, StatusCode};
use fleetdeck_proto::link::{Outcome, WireError};
use fleetdeck_proto::omarchy_admin::{
    ActOutcome, OmarchyActParams, OmarchyActResult, OmarchyAction, OmarchyStatus, code,
};
use fleetdeck_proto::version::feature;
use serde::Deserialize;
use serde_json::{Value, json};

use super::Board;
use super::artifact_delete::ELEVATION_HEADER;
use super::guard::ApiError;
use super::settings_routes::{SETTINGS_WAIT, is_elevated, owner, station_call_wait};
use crate::store::{AuditResult, LinkFeature, NewAudit, Principal};

/// The longest wait for an action that runs a child on the station.
const ACT_WAIT: Duration = Duration::from_secs(75);

const UNSUPPORTED_TEXT: &str = "This station's build cannot manage Omarchy from the board.";
const ELEVATION_TEXT: &str = "This action needs the owner's elevation.";

/// The `POST` body.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActBody {
    action: OmarchyAction,
}

/// The longest wait for the answer of `action`: a provisioning retry
/// answers at once.
pub const fn wait_for(action: OmarchyAction) -> Duration {
    match action {
        OmarchyAction::ProvisionRetry => SETTINGS_WAIT,
        _ => ACT_WAIT,
    }
}

fn unsupported() -> ApiError {
    ApiError::new(StatusCode::NOT_IMPLEMENTED, "unsupported", UNSUPPORTED_TEXT)
}

fn bad_answer() -> ApiError {
    ApiError::new(
        StatusCode::BAD_GATEWAY,
        "station_error",
        "the station answered with data that the deck does not know",
    )
}

/// The error for a station answer: a fixed status, code and text for each
/// wire code; never the message of the station.
fn wire_failure(e: &WireError) -> ApiError {
    let (status, name, text) = match e.code.as_str() {
        code::ELEVATION_REQUIRED => (
            StatusCode::BAD_REQUEST,
            "elevation_required",
            ELEVATION_TEXT,
        ),
        code::BUSY => (
            StatusCode::CONFLICT,
            "busy",
            "Another Omarchy action is running on this station.",
        ),
        code::OMARCHY_MISSING => (
            StatusCode::CONFLICT,
            "omarchy_missing",
            "Omarchy was not found on this station.",
        ),
        code::NOT_PROVISIONED => (
            StatusCode::CONFLICT,
            "not_provisioned",
            "Provisioning cannot be retried from the board in this state.",
        ),
        code::INVALID_REQUEST => (
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "The request is not valid.",
        ),
        "unavailable" | "unsupported" | "feature_not_negotiated" | "unknown_method" => {
            return unsupported();
        }
        _ => {
            return ApiError::new(
                StatusCode::BAD_GATEWAY,
                "station_error",
                "the station answered with an error",
            );
        }
    };
    ApiError::new(status, name, text)
}

/// Refuses a station whose live links did not negotiate `omarchy_admin.v1`.
/// An unknown station is a 404 here; a station without a live link goes on
/// to the call, which answers 409 `station_unreachable`.
async fn require_feature(board: &Board, name: &str) -> Result<(), ApiError> {
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
    if board
        .store
        .link_feature(&rec.station_id, feature::OMARCHY_ADMIN)
        == LinkFeature::Without
    {
        return Err(unsupported());
    }
    Ok(())
}

/// One call of an Omarchy method and its parsed answer.
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

/// `GET /api/stations/{station}/omarchy`: link method `omarchy.status`.
///
/// # Errors
/// 403 `owner_only`; 404 `unknown_station`; 409 `station_unreachable`,
/// `omarchy_missing`; 501 `unsupported`; 502 `station_error`; 504 `timeout`.
pub async fn status(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    Path(name): Path<String>,
) -> Result<Json<OmarchyStatus>, ApiError> {
    owner(
        &board,
        &principal,
        "station.omarchy_status",
        &name,
        "reads the Omarchy status of a station",
    )
    .await?;
    require_feature(&board, &name).await?;
    call(&board, &name, "omarchy.status", json!({}), SETTINGS_WAIT)
        .await
        .map(Json)
}

/// `POST /api/stations/{station}/omarchy { action }` with the header
/// `x-fleetdeck-elevation`: link method `omarchy.act`.
///
/// # Errors
/// 400 `invalid_body`, `elevation_required`, `invalid_request`; 403
/// `owner_only`; 404 `unknown_station`; 409 `station_unreachable`, `busy`,
/// `omarchy_missing`, `not_provisioned`; 501 `unsupported`; 502
/// `station_error`; 504 `timeout`.
pub async fn act(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    Path(name): Path<String>,
    headers: HeaderMap,
    body: Result<Json<ActBody>, JsonRejection>,
) -> Result<Json<OmarchyActResult>, ApiError> {
    const ACTION: &str = "station.omarchy_act";
    let session = owner(
        &board,
        &principal,
        ACTION,
        &name,
        "manages Omarchy on a station",
    )
    .await?;
    let actor = principal.actor();
    let action = match body {
        Ok(Json(b)) => b.action,
        Err(_) => {
            let result = Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_body",
                "the request body is not valid",
            ));
            audit(&board, &actor, &name, None, &result).await;
            return result.map(Json);
        }
    };
    let token = headers.get(ELEVATION_HEADER).and_then(|v| v.to_str().ok());
    let result = if is_elevated(&board, &session, token) {
        run(&board, &name, action).await
    } else {
        Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "elevation_required",
            ELEVATION_TEXT,
        ))
    };
    audit(&board, &actor, &name, Some(action), &result).await;
    result.map(Json)
}

async fn run(
    board: &Board,
    name: &str,
    action: OmarchyAction,
) -> Result<OmarchyActResult, ApiError> {
    require_feature(board, name).await?;
    let params = json!(OmarchyActParams {
        action,
        elevated: true,
    });
    call(board, name, "omarchy.act", params, wait_for(action)).await
}

/// The snake case name of an action, for the audit row.
fn action_name(action: OmarchyAction) -> &'static str {
    match action {
        OmarchyAction::Install => "install",
        OmarchyAction::Uninstall => "uninstall",
        OmarchyAction::ScreensaverOn => "screensaver_on",
        OmarchyAction::ScreensaverOff => "screensaver_off",
        OmarchyAction::ScreensaverRebaseline => "screensaver_rebaseline",
        OmarchyAction::ProvisionRetry => "provision_retry",
    }
}

fn outcome_name(outcome: ActOutcome) -> &'static str {
    match outcome {
        ActOutcome::Done => "done",
        ActOutcome::Failed => "failed",
        ActOutcome::Started => "started",
    }
}

/// The act code of a `failed` result for the audit row. A code that the
/// contract does not name becomes `failed`, so that the row never holds text
/// that the station chose.
fn audit_code(result: &OmarchyActResult) -> &'static str {
    let Some(text) = result.code.as_deref() else {
        return "-";
    };
    [
        code::INTEGRATION_OUTDATED,
        code::INSTALL_REFUSED,
        code::UNINSTALL_INCOMPLETE,
        code::SCREENSAVER_REFUSED,
        code::TIMEOUT,
        code::OUTPUT_TOO_LARGE,
        code::FAILED,
    ]
    .into_iter()
    .find(|known| *known == text)
    .unwrap_or(code::FAILED)
}

/// The audit row of a `POST`: `action=<action> outcome=<outcome or error
/// code> code=<act code or ->`. Never a message.
async fn audit(
    board: &Board,
    actor: &str,
    target: &str,
    action: Option<OmarchyAction>,
    result: &Result<OmarchyActResult, ApiError>,
) {
    let (audit_result, outcome, act_code) = match result {
        Ok(r) if r.outcome == ActOutcome::Failed => {
            (AuditResult::Failed, outcome_name(r.outcome), audit_code(r))
        }
        Ok(r) => (AuditResult::Ok, outcome_name(r.outcome), audit_code(r)),
        Err(e) => (AuditResult::Refused, e.code, "-"),
    };
    let detail = format!(
        "action={} outcome={outcome} code={act_code}",
        action.map_or("-", action_name)
    );
    board
        .store
        .audit_or_log(
            NewAudit::new(actor, "station.omarchy", audit_result)
                .target(target)
                .detail(detail),
        )
        .await;
}
