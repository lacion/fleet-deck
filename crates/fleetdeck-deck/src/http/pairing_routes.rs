//! The pairing request routes (DL-178, tailnet design 5.1 and 5.4). Every
//! route is owner only (`Principal::Owner`, also with the `admin` scope: a
//! token or a member gets 403 `owner_only`), and every POST has the CSRF
//! check of the guard.
//!
//! - `GET /api/pairing/window`: the window of this deck process.
//! - `POST /api/pairing/window { open: true, elevation }`: opens it for
//!   10 minutes (new generation); `{ open: false }` closes it.
//! - `GET /api/pairing/requests`: the window and the waiting requests, with
//!   the informational Tailscale identity of each.
//! - `POST /api/pairing/requests/{id}/approve { fingerprint, digest,
//!   elevation }`: the one serialized decision of the link hub.
//! - `POST /api/pairing/requests/{id}/deny`.
//!
//! The approval never selects a request by name, address, Tailscale user or
//! list position: it needs the full link key fingerprint and the digest of
//! the record that the owner saw. The audit rows hold ids and fingerprints
//! only.

use std::net::IpAddr;
use std::sync::Arc;

use axum::Json;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Extension, Path, State};
use axum::http::StatusCode;
use fleetdeck_core::identity::{Fingerprint, PeerKey};
use fleetdeck_core::tailscale::TailscaleCli;
use fleetdeck_link::pair_hub::{ApproveError, PairRequests, PendingView, WindowView};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::Semaphore;

use super::Board;
use super::guard::ApiError;
use super::settings_routes::{is_elevated, owner};
use super::tailnet_routes::{cli_of, read_saved};
use crate::store::auth::LoginSession;
use crate::store::{AuditResult, NewAudit, Principal};

/// The Tailscale identity lookups that run at the same time.
const WHOIS_AT_ONCE: usize = 2;

static WHOIS: Semaphore = Semaphore::const_new(WHOIS_AT_ONCE);

fn hub(board: &Board) -> Result<&Arc<PairRequests>, ApiError> {
    board
        .pairing
        .as_ref()
        .map(|node| node.pair_requests())
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "pairing_unavailable",
                "this deck process cannot pair stations",
            )
        })
}

fn body_error(_: &JsonRejection) -> ApiError {
    ApiError::new(
        StatusCode::BAD_REQUEST,
        "invalid_body",
        "the request body is not valid",
    )
}

fn invalid(message: &str) -> ApiError {
    ApiError::new(StatusCode::BAD_REQUEST, "invalid_body", message)
}

fn elevation_required(what: &str) -> ApiError {
    ApiError::new(
        StatusCode::BAD_REQUEST,
        "elevation_required",
        format!("{what} needs the owner's elevation"),
    )
}

/// The audit row of a pairing action: ids and fingerprints only.
async fn audit<T>(
    board: &Board,
    action: &'static str,
    target: &str,
    detail: &str,
    result: &Result<T, ApiError>,
) {
    let row = match result {
        Ok(_) => NewAudit::new("owner", action, AuditResult::Ok).detail(detail),
        Err(e) => {
            NewAudit::new("owner", action, AuditResult::Failed).detail(if detail.is_empty() {
                e.code.to_owned()
            } else {
                format!("{}: {detail}", e.code)
            })
        }
    };
    board.store.audit_or_log(row.target(target)).await;
}

/// The window of the deck as the routes show it.
#[derive(Serialize)]
pub struct WindowBody {
    open: bool,
    generation: u64,
    /// Unix milliseconds; `null` while the window is closed.
    deadline_ms: Option<i64>,
}

impl From<WindowView> for WindowBody {
    fn from(w: WindowView) -> Self {
        Self {
            open: w.open,
            generation: w.generation,
            deadline_ms: w.deadline_ms,
        }
    }
}

/// `GET /api/pairing/window`.
///
/// # Errors
/// 403 `owner_only`; 503 `pairing_unavailable`.
pub async fn window_get(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
) -> Result<Json<WindowBody>, ApiError> {
    owner(
        &board,
        &principal,
        "pairing.window_get",
        "",
        "reads the pairing window",
    )
    .await?;
    Ok(Json(hub(&board)?.window().into()))
}

/// The body of `POST /api/pairing/window`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WindowRequest {
    open: bool,
    #[serde(default)]
    elevation: Option<String>,
}

/// `POST /api/pairing/window { open, elevation? }`.
///
/// # Errors
/// 400 `invalid_body`, `elevation_required` (to open); 403 `owner_only`; 409
/// `no_tailnet_listener`; 503 `pairing_unavailable`.
pub async fn window_set(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    body: Result<Json<WindowRequest>, JsonRejection>,
) -> Result<Json<WindowBody>, ApiError> {
    let session = owner(
        &board,
        &principal,
        "pairing.window_set",
        "",
        "opens or closes the pairing window",
    )
    .await?;
    let Json(req) = body.map_err(|e| body_error(&e))?;
    let action = if req.open {
        "pairing.window_open"
    } else {
        "pairing.window_close"
    };
    let result = set_window(&board, &session, req).await;
    let detail = result.as_ref().map_or_else(
        |_| String::new(),
        |w| format!("generation {}", w.generation),
    );
    audit(&board, action, "", &detail, &result).await;
    result.map(|w| Json(w.into()))
}

async fn set_window(
    board: &Board,
    session: &LoginSession,
    req: WindowRequest,
) -> Result<WindowView, ApiError> {
    let hub = hub(board)?;
    if !req.open {
        hub.close_window().await;
        return Ok(hub.window());
    }
    if !is_elevated(board, session, req.elevation.as_deref()) {
        return Err(elevation_required("opening the pairing window"));
    }
    hub.open_window()
        .await
        .map_err(|e| ApiError::new(StatusCode::CONFLICT, "no_tailnet_listener", e.to_string()))
}

/// The Tailscale identity of a request, for display only.
fn identity_view(identity: Option<&Value>) -> Value {
    match identity {
        None | Some(Value::Null) => json!({ "tags": [], "error": "identity pending" }),
        Some(v) => v.clone(),
    }
}

/// One waiting request of `GET /api/pairing/requests`.
#[derive(Serialize)]
struct RequestBody {
    request_id: String,
    requested_name: String,
    station_fingerprint: String,
    offered_scopes: Vec<String>,
    source_ip: String,
    admitted_at: i64,
    expires_at: i64,
    digest: String,
    tailscale: Value,
}

impl From<&PendingView> for RequestBody {
    fn from(p: &PendingView) -> Self {
        Self {
            request_id: p.request_id.clone(),
            requested_name: p.requested_name.clone(),
            station_fingerprint: p.station_fingerprint.clone(),
            offered_scopes: p
                .offered_scopes
                .iter()
                .map(|s| s.name().to_owned())
                .collect(),
            source_ip: p.source_ip.to_string(),
            admitted_at: p.admitted_at,
            expires_at: p.expires_at,
            digest: p.digest.clone(),
            tailscale: identity_view(p.identity.as_ref()),
        }
    }
}

/// The answer of `GET /api/pairing/requests`.
#[derive(Serialize)]
pub struct RequestsBody {
    window: WindowBody,
    requests: Vec<RequestBody>,
}

/// Looks up the Tailscale identity of `ip` (display only) and keeps it with
/// the request. A failure keeps "identity unavailable".
fn spawn_identity(board: Arc<Board>, id: String, ip: IpAddr) {
    tokio::spawn(async move {
        let Ok(_place) = WHOIS.acquire().await else {
            return;
        };
        let saved = read_saved(&board).await.ok().flatten();
        let cli: TailscaleCli = cli_of(saved.as_ref());
        let identity = match cli.whois(ip).await {
            Ok(w) => {
                let mut v = json!({
                    "host_name": w.name,
                    "tags": w.tags,
                });
                if let Some(login) = w.login {
                    v["login"] = json!(login);
                }
                if let Some(shared) = w.shared {
                    v["shared"] = json!(shared);
                }
                v
            }
            Err(e) => {
                tracing::debug!(error = %e.reason(), "tailscale whois of a pairing request");
                json!({ "tags": [], "error": "identity unavailable" })
            }
        };
        if let Ok(hub) = hub(&board) {
            hub.set_identity(&id, identity).await;
        }
    });
}

/// `GET /api/pairing/requests`.
///
/// # Errors
/// 403 `owner_only`; 503 `pairing_unavailable`.
pub async fn requests(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
) -> Result<Json<RequestsBody>, ApiError> {
    owner(
        &board,
        &principal,
        "pairing.requests",
        "",
        "lists the pairing requests",
    )
    .await?;
    let hub = hub(&board)?;
    let mut list = hub.list().await;
    // A request without an identity gets one lookup; its answer comes with
    // the next call of the board.
    for p in list.iter_mut().filter(|p| p.identity.is_none()) {
        hub.set_identity(&p.request_id, Value::Null).await;
        spawn_identity(Arc::clone(&board), p.request_id.clone(), p.source_ip);
        p.identity = Some(Value::Null);
    }
    Ok(Json(RequestsBody {
        window: hub.window().into(),
        requests: list.iter().map(RequestBody::from).collect(),
    }))
}

/// The body of `POST /api/pairing/requests/{id}/approve`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApproveRequest {
    /// The full link key fingerprint (64 hex) of the station.
    fingerprint: String,
    /// The digest (64 hex) of the record that the owner saw.
    digest: String,
    #[serde(default)]
    elevation: Option<String>,
}

/// The answer of an approval.
#[derive(Serialize)]
pub struct Approved {
    approved: bool,
    station_id: String,
}

fn approve_error(e: ApproveError) -> ApiError {
    let (status, code) = match &e {
        ApproveError::UnknownRequest => (StatusCode::NOT_FOUND, "unknown_request"),
        ApproveError::WindowClosed => (StatusCode::CONFLICT, "window_closed"),
        ApproveError::DigestMismatch => (StatusCode::CONFLICT, "digest_mismatch"),
        ApproveError::FingerprintMismatch => (StatusCode::CONFLICT, "fingerprint_mismatch"),
        ApproveError::AlreadyPaired => (StatusCode::CONFLICT, "already_paired"),
        ApproveError::NameTaken(_) => (StatusCode::CONFLICT, "name_taken"),
        ApproveError::Store(_) => return ApiError::internal(e),
    };
    ApiError::new(status, code, e.to_string())
}

fn digest_of(text: &str) -> Option<[u8; 32]> {
    if text.len() != 64
        || !text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(text.get(2 * i..2 * i + 2)?, 16).ok()?;
    }
    Some(out)
}

/// `POST /api/pairing/requests/{id}/approve`.
///
/// # Errors
/// 400 `invalid_body`, `elevation_required`; 403 `owner_only`; 404
/// `unknown_request`; 409 `window_closed`, `digest_mismatch`,
/// `fingerprint_mismatch`, `already_paired`, `name_taken`; 503
/// `pairing_unavailable`.
pub async fn approve(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    Path(id): Path<String>,
    body: Result<Json<ApproveRequest>, JsonRejection>,
) -> Result<Json<Approved>, ApiError> {
    let session = owner(
        &board,
        &principal,
        "pairing.approve",
        &id,
        "approves a pairing request",
    )
    .await?;
    let Json(req) = body.map_err(|e| body_error(&e))?;
    let detail = req.fingerprint.clone();
    let result = approve_request(&board, &session, &id, req).await;
    audit(&board, "pairing.approve", &id, &detail, &result).await;
    result.map(Json)
}

async fn approve_request(
    board: &Board,
    session: &LoginSession,
    id: &str,
    req: ApproveRequest,
) -> Result<Approved, ApiError> {
    let hub = hub(board)?;
    let fingerprint: Fingerprint = req
        .fingerprint
        .parse()
        .map_err(|_| invalid("fingerprint is the full link key fingerprint: 64 hex digits"))?;
    let digest = digest_of(&req.digest).ok_or_else(|| invalid("digest is 64 hex digits"))?;
    if !is_elevated(board, session, req.elevation.as_deref()) {
        return Err(elevation_required("approving a pairing request"));
    }
    let approved = hub
        .approve(id, &fingerprint, &digest)
        .await
        .map_err(approve_error)?;
    Ok(Approved {
        approved: true,
        station_id: station_id(board, approved.station).await?,
    })
}

async fn station_id(board: &Board, key: PeerKey) -> Result<String, ApiError> {
    let record = board
        .store
        .station_by_key(key)
        .await
        .map_err(ApiError::internal)?;
    Ok(record.map_or_else(
        || key.fingerprint().to_string(),
        |st| st.station_id.as_str().to_owned(),
    ))
}

/// The answer of a denial.
#[derive(Serialize)]
pub struct Denied {
    denied: bool,
}

/// `POST /api/pairing/requests/{id}/deny`.
///
/// # Errors
/// 403 `owner_only`; 404 `unknown_request`; 503 `pairing_unavailable`.
pub async fn deny(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    Path(id): Path<String>,
) -> Result<Json<Denied>, ApiError> {
    owner(
        &board,
        &principal,
        "pairing.deny",
        &id,
        "denies a pairing request",
    )
    .await?;
    let result = async {
        hub(&board)?
            .deny(&id)
            .await
            .map(|key| key.fingerprint().to_string())
            .map_err(approve_error)
    }
    .await;
    let detail = result
        .as_ref()
        .map(String::as_str)
        .unwrap_or_default()
        .to_owned();
    audit(&board, "pairing.deny", &id, &detail, &result).await;
    result.map(|_| Json(Denied { denied: true }))
}
