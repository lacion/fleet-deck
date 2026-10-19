//! The setup report routes (BG-3, design `docs/v2/design/board-gaps.md`
//! section 4.4). Every route is owner only (`Principal::Owner`, also with the
//! `admin` scope: a token or a member gets 403 `owner_only`), and every
//! mutation has the CSRF check of the guard.
//!
//! - `GET /api/provision-reports`: the reporting window and the reports,
//!   newest first.
//! - `POST /api/provision-reports/window { open, elevation? }`: opens the
//!   window for one hour (needs the elevation) or closes it.
//! - `POST /api/provision-reports/{fingerprint}/retry`: allows one retry for
//!   the exact report that the owner saw (needs the elevation).
//! - `DELETE /api/provision-reports/{fingerprint}/retry`: cancels a queued
//!   retry.
//! - `DELETE /api/provision-reports/{fingerprint}`: dismisses a report.
//!
//! A report is self-reported by an unpaired key. The routes never create a
//! station, a pairing, a grant or an invitation. The audit rows hold the
//! fingerprint prefix and an action word, never the reported name.

use std::sync::Arc;

use axum::Json;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Extension, Path, State};
use axum::http::StatusCode;
use fleetdeck_core::identity::Fingerprint;
use fleetdeck_link::pair_hub::NoTailnetListener;
use fleetdeck_link::report_accept::is_lower_hex;
use serde::{Deserialize, Serialize};

use super::Board;
use super::guard::ApiError;
use super::settings_routes::{is_elevated, owner};
use crate::provision_reports::{ProvisionReports, WindowView, new_retry_id, retry_allowed};
use crate::store::provision_reports::{AllowRetry, Allowed, Cancelled, RetryState, StoredReport};
use crate::store::{AuditResult, NewAudit, Principal};

fn reports(board: &Board) -> Result<&Arc<ProvisionReports>, ApiError> {
    board
        .pairing
        .as_ref()
        .map(|node| node.provision_reports())
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "pairing_unavailable",
                "this deck process cannot pair stations",
            )
        })
}

fn invalid() -> ApiError {
    ApiError::new(
        StatusCode::BAD_REQUEST,
        "invalid_request",
        "The request is not valid.",
    )
}

fn elevation_required(what: &str) -> ApiError {
    ApiError::new(
        StatusCode::BAD_REQUEST,
        "elevation_required",
        format!("{what} needs the owner's elevation"),
    )
}

fn conflict(code: &'static str, message: &'static str) -> ApiError {
    ApiError::new(StatusCode::CONFLICT, code, message)
}

fn not_found(code: &'static str, message: &'static str) -> ApiError {
    ApiError::new(StatusCode::NOT_FOUND, code, message)
}

/// The path key: the full fingerprint, 64 lowercase hex digits.
fn key_of(text: &str) -> Result<Fingerprint, ApiError> {
    text.parse().map_err(|_| invalid())
}

/// The audit row of a report action: the fingerprint prefix and an action
/// word, never the reported name. A failure adds the error code.
async fn audit<T>(
    board: &Board,
    action: &'static str,
    target: &str,
    word: &str,
    result: &Result<T, ApiError>,
) {
    let row = match result {
        Ok(_) => NewAudit::new("owner", action, AuditResult::Ok).detail(word),
        Err(e) => NewAudit::new("owner", action, AuditResult::Failed)
            .detail(format!("{}: {word}", e.code)),
    };
    board.store.audit_or_log(row.target(target)).await;
}

/// The reporting window as the routes show it.
#[derive(Serialize)]
pub struct WindowBody {
    open: bool,
    /// Unix milliseconds; `null` while the window is closed.
    deadline_ms: Option<i64>,
}

impl From<WindowView> for WindowBody {
    fn from(w: WindowView) -> Self {
        Self {
            open: w.open,
            deadline_ms: w.deadline_ms,
        }
    }
}

/// The retry permission of a report.
#[derive(Serialize)]
pub struct RetryBody {
    state: &'static str,
    expires_at: i64,
}

impl From<RetryState> for RetryBody {
    fn from(s: RetryState) -> Self {
        Self {
            state: if s.delivered { "delivered" } else { "queued" },
            expires_at: s.expires_at,
        }
    }
}

/// One report of `GET /api/provision-reports`. The name is self-reported.
#[derive(Serialize)]
pub struct ReportBody {
    fingerprint: String,
    terminal_id: String,
    state_digest: String,
    name: String,
    code: String,
    phase: String,
    received_at: i64,
    last_contact: i64,
    retry: Option<RetryBody>,
}

impl From<StoredReport> for ReportBody {
    fn from(r: StoredReport) -> Self {
        Self {
            fingerprint: r.fingerprint,
            terminal_id: r.terminal_id,
            state_digest: r.state_digest,
            name: r.name,
            code: r.code,
            phase: r.phase,
            received_at: r.received_at,
            last_contact: r.last_contact,
            retry: r.retry.map(RetryBody::from),
        }
    }
}

/// The answer of `GET /api/provision-reports`.
#[derive(Serialize)]
pub struct ListBody {
    window: WindowBody,
    reports: Vec<ReportBody>,
}

/// `GET /api/provision-reports`.
///
/// # Errors
/// 403 `owner_only`; 503 `pairing_unavailable`.
pub async fn list(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
) -> Result<Json<ListBody>, ApiError> {
    owner(
        &board,
        &principal,
        "provision_reports.list",
        "",
        "reads the setup reports",
    )
    .await?;
    let reports = reports(&board)?;
    let rows = board
        .store
        .provision_reports_list(board.store.now_ms())
        .await
        .map_err(ApiError::internal)?;
    Ok(Json(ListBody {
        window: reports.window().into(),
        reports: rows.into_iter().map(ReportBody::from).collect(),
    }))
}

/// The body of `POST /api/provision-reports/window`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WindowRequest {
    open: bool,
    #[serde(default)]
    elevation: Option<String>,
}

/// `POST /api/provision-reports/window { open, elevation? }`.
///
/// # Errors
/// 400 `invalid_request`, `elevation_required` (to open); 403 `owner_only`;
/// 409 `no_tailnet_listener`; 503 `pairing_unavailable`.
pub async fn window_set(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    body: Result<Json<WindowRequest>, JsonRejection>,
) -> Result<Json<WindowBody>, ApiError> {
    let session = owner(
        &board,
        &principal,
        "provision_reports.window",
        "",
        "opens or closes the setup reports",
    )
    .await?;
    let reports = reports(&board)?;
    let Json(req) = body.map_err(|_| invalid())?;
    let word = if req.open { "open" } else { "close" };
    let result = if req.open {
        open_window(&board, reports, &session, req.elevation.as_deref())
    } else {
        Ok(reports.close_window())
    };
    audit(&board, "provision_reports.window", "", word, &result).await;
    result.map(|w| Json(w.into()))
}

fn open_window(
    board: &Board,
    reports: &ProvisionReports,
    session: &crate::store::auth::LoginSession,
    elevation: Option<&str>,
) -> Result<WindowView, ApiError> {
    if !is_elevated(board, session, elevation) {
        return Err(elevation_required("opening the setup reports"));
    }
    let tailnet = board
        .pairing
        .as_ref()
        .map_or(0, |node| node.pair_requests().tailnet_listeners());
    if tailnet == 0 {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "no_tailnet_listener",
            NoTailnetListener.to_string(),
        ));
    }
    Ok(reports.open_window())
}

/// The body of `POST /api/provision-reports/{fingerprint}/retry`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetryRequest {
    terminal_id: String,
    state_digest: String,
    request_id: String,
    #[serde(default)]
    elevation: Option<String>,
}

impl RetryRequest {
    /// True when each field has its form.
    fn is_valid(&self) -> bool {
        is_lower_hex(&self.terminal_id, 32)
            && is_lower_hex(&self.state_digest, 64)
            && (1..=64).contains(&self.request_id.len())
            && self
                .request_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    }
}

/// The answer of `POST /api/provision-reports/{fingerprint}/retry`.
#[derive(Serialize)]
pub struct RetryAnswer {
    retry: RetryBody,
}

/// `POST /api/provision-reports/{fingerprint}/retry`.
///
/// # Errors
/// 400 `invalid_request`, `elevation_required`; 403 `owner_only`; 404
/// `report_not_found`; 409 `report_changed`, `retry_not_allowed`,
/// `retry_queued`; 503 `pairing_unavailable`.
pub async fn retry_allow(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    Path(key): Path<String>,
    body: Result<Json<RetryRequest>, JsonRejection>,
) -> Result<Json<RetryAnswer>, ApiError> {
    let session = owner(
        &board,
        &principal,
        "provision_reports.retry",
        &key,
        "allows a setup retry",
    )
    .await?;
    reports(&board)?;
    let fingerprint = key_of(&key)?;
    let Json(req) = body.map_err(|_| invalid())?;
    if !req.is_valid() {
        return Err(invalid());
    }
    let result = allow(&board, &session, &fingerprint, req).await;
    audit(
        &board,
        "provision_reports.retry",
        &fingerprint.short(),
        "allowed",
        &result,
    )
    .await;
    result.map(Json)
}

async fn allow(
    board: &Board,
    session: &crate::store::auth::LoginSession,
    fingerprint: &Fingerprint,
    req: RetryRequest,
) -> Result<RetryAnswer, ApiError> {
    if !is_elevated(board, session, req.elevation.as_deref()) {
        return Err(elevation_required("allowing a setup retry"));
    }
    let retry_id = new_retry_id().ok_or_else(|| ApiError::internal("random generator"))?;
    let allowed = board
        .store
        .allow_provision_retry(AllowRetry {
            fingerprint: fingerprint.to_string(),
            terminal_id: req.terminal_id,
            state_digest: req.state_digest,
            request_id: req.request_id,
            retry_id,
            now: board.store.now_ms(),
            code_allowed: retry_allowed,
        })
        .await
        .map_err(ApiError::internal)?;
    match allowed {
        Allowed::NoReport => Err(no_report()),
        Allowed::Changed => Err(conflict(
            "report_changed",
            "The reported setup condition changed. Review the latest report.",
        )),
        Allowed::NotAllowed => Err(conflict(
            "retry_not_allowed",
            "Retrying cannot repair this condition. Fleet Deck has no board repair for it yet.",
        )),
        Allowed::Queued => Err(conflict(
            "retry_queued",
            "A retry is already allowed for this key.",
        )),
        Allowed::Granted(state) => Ok(RetryAnswer {
            retry: state.into(),
        }),
    }
}

fn no_report() -> ApiError {
    not_found("report_not_found", "No setup report from this key.")
}

/// `DELETE /api/provision-reports/{fingerprint}/retry`.
///
/// # Errors
/// 400 `invalid_request`; 403 `owner_only`; 404 `report_not_found`,
/// `retry_not_found`; 409 `retry_delivered`; 503 `pairing_unavailable`.
pub async fn retry_cancel(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    Path(key): Path<String>,
) -> Result<StatusCode, ApiError> {
    owner(
        &board,
        &principal,
        "provision_reports.retry_cancel",
        &key,
        "cancels a setup retry",
    )
    .await?;
    reports(&board)?;
    let fingerprint = key_of(&key)?;
    let result = async {
        let cancelled = board
            .store
            .cancel_provision_retry(fingerprint.to_string(), board.store.now_ms())
            .await
            .map_err(ApiError::internal)?;
        match cancelled {
            Cancelled::NoReport => Err(no_report()),
            Cancelled::NoRetry => Err(not_found(
                "retry_not_found",
                "No retry is waiting for this key.",
            )),
            Cancelled::Delivered => Err(conflict(
                "retry_delivered",
                "The retry was already delivered to the station.",
            )),
            Cancelled::Done => Ok(StatusCode::NO_CONTENT),
        }
    }
    .await;
    audit(
        &board,
        "provision_reports.retry",
        &fingerprint.short(),
        "cancelled",
        &result,
    )
    .await;
    result
}

/// `DELETE /api/provision-reports/{fingerprint}`.
///
/// # Errors
/// 400 `invalid_request`; 403 `owner_only`; 404 `report_not_found`; 503
/// `pairing_unavailable`.
pub async fn dismiss(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    Path(key): Path<String>,
) -> Result<StatusCode, ApiError> {
    owner(
        &board,
        &principal,
        "provision_reports.dismiss",
        &key,
        "dismisses a setup report",
    )
    .await?;
    reports(&board)?;
    let fingerprint = key_of(&key)?;
    let result = async {
        let removed = board
            .store
            .dismiss_provision_report(fingerprint.to_string())
            .await
            .map_err(ApiError::internal)?;
        if removed {
            Ok(StatusCode::NO_CONTENT)
        } else {
            Err(no_report())
        }
    }
    .await;
    audit(
        &board,
        "provision_reports.dismiss",
        &fingerprint.short(),
        "dismissed",
        &result,
    )
    .await;
    result
}
