//! The owner's coordinator grant controls (M3 6-5, ADR-006 §4, §7):
//! `GET /api/coordinator/grants` lists the grants without any credential;
//! `POST /api/coordinator/grants/{grant_id}/revoke` commits `revoked` on the
//! deck first, so every later decision for the grant is `forbidden`.
//! `POST .../caps` changes the desktop caps (R13 P6 §2.2) and
//! `GET .../desktop-ops` lists the recent desktop operations of a grant.

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::Json;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Extension, Path, State};
use axum::http::{HeaderMap, StatusCode};
use fleetdeck_proto::orchestration::GrantView;

use super::Board;
use super::artifact_delete::ELEVATION_HEADER;
use super::fleet_routes::{LIST_PAGE, audited};
use super::guard::ApiError;
use super::settings_routes::{is_elevated, owner};
use crate::store::coordinator_desktop::{CapsChange, CapsError, RecentOp};
use crate::store::{AuditResult, Grant, GrantClass, GrantState, NewAudit, Principal};

/// The detail of the audit row of a created grant (the store writes the row
/// of a worker grant in its own transaction).
pub(crate) use super::grant_spawn::audit_detail;

/// The most stations that one grant names (its own station included).
pub const MAX_GRANT_STATIONS: usize = 16;

fn view(g: Grant) -> GrantView {
    let (kind, max_modes) = (g.kind, g.max_modes.clone());
    let class = |c: &GrantClass| {
        match c {
            GrantClass::Read => "read",
            GrantClass::Mail => "mail",
            GrantClass::Drive => "drive",
            GrantClass::Spawn => "spawn",
            GrantClass::Inspect => "inspect",
            GrantClass::Manage => "manage",
            GrantClass::Answer => "answer",
            GrantClass::Desktop => "desktop",
            GrantClass::Capture => "capture",
            GrantClass::Input => "input",
        }
        .to_owned()
    };
    let recording = g.recording_cap > 0;
    // The input fields show only for a grant with the input allowance (R13 P7
    // §3.1): a grant without it has none of them.
    let input = g.allow_input;
    GrantView {
        classes: g.classes.iter().map(class).collect(),
        stations: g.stations.into_iter().collect(),
        state: match g.state {
            GrantState::Active => "active",
            GrantState::Revoked => "revoked",
            GrantState::Expired => "expired",
        }
        .to_owned(),
        generation: g.binding.map(|b| b.generation),
        grant_id: g.grant_id,
        station_id: g.station_id,
        session_id: g.session_id,
        spawn_cap: g.spawn_cap,
        spawn_used: g.spawn_used,
        revoked_reason: g.revoked_reason,
        expires_at: g.expires_at,
        last_used_at: g.last_used_at,
        // A used counter is shown only with its own cap (R13 P6 §8.1).
        desktop_cap: g.desktop_cap,
        desktop_used: g.desktop_cap.map(|_| g.desktop_used),
        capture_cap: g.capture_cap,
        capture_used: g.capture_cap.map(|_| g.capture_used),
        image_byte_cap: g.image_byte_cap,
        image_byte_used: g.image_byte_cap.map(|_| g.image_byte_used),
        // The recording fields show only for a non-zero recording cap
        // (R13 P5b-2 §13.2): a screenshot-only grant has none.
        recording_cap: recording.then_some(g.recording_cap),
        recording_used: recording.then_some(g.recording_used),
        recording_second_cap: recording.then_some(g.recording_second_cap),
        recording_second_used: recording.then_some(g.recording_second_used),
        allow_input: input.then_some(true),
        input_scope: g.input_scope.filter(|_| input),
        input_cap: input.then_some(g.input_cap),
        input_used: input.then_some(g.input_used),
        input_character_cap: input.then_some(g.input_character_cap),
        input_character_used: input.then_some(g.input_character_used),
        allow_click: input.then_some(g.allow_click),
        kind,
        max_modes,
    }
}

/// `GET /api/coordinator/grants`: the newest grants.
///
/// # Errors
/// A store error.
pub async fn list(State(board): State<Arc<Board>>) -> Result<Json<Vec<GrantView>>, ApiError> {
    let grants = board
        .store
        .grants(LIST_PAGE)
        .await
        .map_err(ApiError::internal)?;
    Ok(Json(grants.into_iter().map(view).collect()))
}

/// `POST /api/coordinator/grants/{grant_id}/revoke`. A revoked grant stays
/// revoked; a second revoke returns it unchanged.
///
/// # Errors
/// 404 `unknown_grant`.
pub async fn revoke(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    Path(grant_id): Path<String>,
) -> Result<Json<GrantView>, ApiError> {
    let result = async {
        owner_only(&principal)?;
        let before = board
            .store
            .grant(grant_id.clone())
            .await
            .map_err(ApiError::internal)?;
        board
            .store
            .revoke_grant(grant_id.clone(), "owner".to_owned())
            .await
            .map_err(ApiError::internal)?;
        // The stations that lost `coordinate` leave the link cache at once.
        board
            .link
            .peers
            .refresh_changed()
            .await
            .map_err(ApiError::internal)?;
        // The deck refuses at once; the station learns it through
        // `session.coordinator_revoke` when its link is up (ADR-006 §7,
        // DL-212). A second revoke sends nothing.
        if let Some(g) = before.filter(|g| g.state != GrantState::Revoked) {
            let command = fleetdeck_proto::SessionCommand::CoordinatorRevoke {
                session_id: g.session_id.clone(),
                grant_id: g.grant_id.clone(),
            };
            queue(&board, &g, command).await?;
        }
        current(&board, &grant_id).await
    }
    .await;
    audited(
        &board,
        &principal,
        "coordinator.revoke",
        Some(&grant_id),
        result,
    )
    .await
    .map(Json)
}

/// `POST /api/coordinator/grants/{grant_id}/renew` (ADR-006 §5.5, test
/// 25b). Only an idle-expired grant inside its lifetime renews: the deck
/// records the pending generation and sends `session.coordinator_renew`;
/// the grant becomes active again only when the station acknowledges that
/// generation with `coordinator.bound`.
///
/// # Errors
/// 404 `unknown_grant`; 409 `not_renewable`.
pub async fn renew(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    Path(grant_id): Path<String>,
) -> Result<Json<GrantView>, ApiError> {
    let result = async {
        owner_only(&principal)?;
        let g = current_grant(&board, &grant_id).await?;
        let Some(generation) = board
            .store
            .renew_grant(grant_id.clone())
            .await
            .map_err(ApiError::internal)?
        else {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "not_renewable",
                "only an idle-expired grant inside its lifetime renews".to_owned(),
            ));
        };
        let command = fleetdeck_proto::SessionCommand::CoordinatorRenew {
            session_id: g.session_id.clone(),
            grant_id: g.grant_id.clone(),
            generation,
        };
        queue(&board, &g, command).await?;
        current(&board, &grant_id).await
    }
    .await;
    audited(
        &board,
        &principal,
        "coordinator.renew",
        Some(&grant_id),
        result,
    )
    .await
    .map(Json)
}

/// The body of `POST /api/coordinator/grants/{grant_id}/caps`: the caps to
/// change; an absent cap keeps its value.
#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapsBody {
    desktop_cap: Option<u32>,
    capture_cap: Option<u32>,
    image_byte_cap: Option<u64>,
    recording_cap: Option<u32>,
    recording_second_cap: Option<u32>,
    input_cap: Option<u32>,
    input_character_cap: Option<u32>,
    spawn_cap: Option<u32>,
    max_modes: Option<BTreeMap<String, String>>,
}

/// The caps of a grant as JSON, for the audit row. The two recording caps are
/// in it only for a grant that allows recordings, and the two input caps only
/// for a grant that allows input (R13 P7 §3.1); the spawn cap is in it only
/// for a grant with the class `spawn`, and the mode ceilings only for a grant
/// with `spawn` or `manage` (MCP P4a §8). The row of any other grant keeps its
/// exact JSON.
fn caps_json(g: &Grant) -> serde_json::Value {
    let mut caps = serde_json::json!({
        "desktop_cap": g.desktop_cap,
        "capture_cap": g.capture_cap,
        "image_byte_cap": g.image_byte_cap,
    });
    if g.recording_cap > 0 || g.recording_second_cap > 0 {
        caps["recording_cap"] = g.recording_cap.into();
        caps["recording_second_cap"] = g.recording_second_cap.into();
    }
    if g.allow_input {
        caps["input_cap"] = g.input_cap.into();
        caps["input_character_cap"] = g.input_character_cap.into();
    }
    let spawns = g.classes.contains(&GrantClass::Spawn);
    if spawns {
        caps["spawn_cap"] = g.spawn_cap.into();
    }
    if spawns || g.classes.contains(&GrantClass::Manage) {
        caps["max_modes"] = serde_json::json!(g.max_modes);
    }
    caps
}

fn no_grant() -> ApiError {
    ApiError::new(StatusCode::NOT_FOUND, "not_found", "No such grant.")
}

/// `POST /api/coordinator/grants/{grant_id}/caps` (owner only, R13 P6 §2.2,
/// decision O12-4 item 16): a decrease needs no elevation, an increase needs
/// the elevation of the header `x-fleetdeck-elevation`. The caps are the
/// three desktop limits, the two recording limits and the two input limits
/// (R13 P7 §9.1; a grant without `allow_input` refuses them), and the spawn
/// cap and the mode ceilings (MCP P4a §8). The counters
/// never change. The audit row `coordinator.grant_caps` holds the old and the new
/// caps.
///
/// # Errors
/// 400 `invalid_body`; 400 `invalid_grant` (the fixed text of the cap rule);
/// 400 `elevation_required`; 403 `owner_only`; 404 `not_found`.
pub async fn caps(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    headers: HeaderMap,
    Path(grant_id): Path<String>,
    body: Result<Json<CapsBody>, JsonRejection>,
) -> Result<Json<GrantView>, ApiError> {
    const ACTION: &str = "coordinator.grant_caps";
    let session = owner(
        &board,
        &principal,
        ACTION,
        &grant_id,
        "changes the caps of a coordinator grant",
    )
    .await?;
    let token = headers.get(ELEVATION_HEADER).and_then(|v| v.to_str().ok());
    let result: Result<(Grant, Grant), ApiError> = async {
        let Json(req) = body.map_err(|_| {
            ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_body",
                "the request body is not valid",
            )
        })?;
        let before = board
            .store
            .grant(grant_id.clone())
            .await
            .map_err(ApiError::internal)?
            .ok_or_else(no_grant)?;
        board
            .store
            .desktop_caps_update(
                &grant_id,
                CapsChange {
                    desktop_cap: req.desktop_cap,
                    capture_cap: req.capture_cap,
                    image_byte_cap: req.image_byte_cap,
                    recording_cap: req.recording_cap,
                    recording_second_cap: req.recording_second_cap,
                    input_cap: req.input_cap,
                    input_character_cap: req.input_character_cap,
                    spawn_cap: req.spawn_cap,
                    max_modes: req.max_modes,
                },
                is_elevated(&board, &session, token),
            )
            .await
            .map_err(ApiError::internal)?
            .map_err(|e| match e {
                CapsError::NotFound => no_grant(),
                CapsError::Invalid(text) => {
                    ApiError::new(StatusCode::BAD_REQUEST, "invalid_grant", text.to_owned())
                }
                CapsError::ElevationRequired => ApiError::new(
                    StatusCode::BAD_REQUEST,
                    "elevation_required",
                    "raising a limit needs the owner's elevation".to_owned(),
                ),
            })?;
        let after = current_grant(&board, &grant_id).await?;
        Ok((before, after))
    }
    .await;
    let row = match &result {
        Ok((before, after)) => NewAudit::new(principal.actor(), ACTION, AuditResult::Ok)
            .target(grant_id.clone())
            .detail(
                serde_json::json!({"old": caps_json(before), "new": caps_json(after)}).to_string(),
            ),
        Err(e) => NewAudit::new(principal.actor(), ACTION, AuditResult::Refused)
            .target(grant_id.clone())
            .detail(e.code),
    };
    board.store.audit_or_log(row).await;
    result.map(|(_, after)| Json(view(after)))
}

/// How many recent operations `GET .../desktop-ops` returns.
const RECENT_OPS: u32 = 50;

/// `GET /api/coordinator/grants/{grant_id}/desktop-ops` (owner only): the
/// newest 50 desktop operations of the grant. A row has no resource, result or
/// hash.
///
/// # Errors
/// 403 `owner_only`; 404 `unknown_grant`.
pub async fn desktop_ops(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    Path(grant_id): Path<String>,
) -> Result<Json<Vec<RecentOp>>, ApiError> {
    owner(
        &board,
        &principal,
        "coordinator.desktop_ops",
        &grant_id,
        "reads the desktop operations of a coordinator grant",
    )
    .await?;
    current_grant(&board, &grant_id).await?;
    let rows = board
        .store
        .desktop_ops_recent(&grant_id, RECENT_OPS)
        .await
        .map_err(ApiError::internal)?;
    Ok(Json(rows))
}

/// Queues a deck-only coordinator command for the grant's station.
async fn queue(
    board: &Board,
    g: &Grant,
    command: fleetdeck_proto::SessionCommand,
) -> Result<(), ApiError> {
    board
        .store
        .enqueue(crate::store::NewOutboxItem {
            station_id: g.station_id.clone(),
            origin: crate::link::router::origin_name(
                fleetdeck_proto::command::CommandOrigin::Board,
            )
            .to_owned(),
            payload: serde_json::to_value(crate::link::CommandKind::Session { command })
                .map_err(ApiError::internal)?,
            expires_at: board
                .store
                .now_ms()
                .saturating_add(board.store.knobs().delivery_ms),
        })
        .await
        .map_err(ApiError::internal)?;
    Ok(())
}

/// The version of the deck-served control skill (ADR-006 §7.1).
pub const CONTROL_SKILL_VERSION: &str = "3";
/// The deck-served control skill: the MCP `instructions` of a coordinator
/// session. It holds no secret and no grant data.
pub const CONTROL_SKILL: &str = include_str!("../control_skill.md");

pub(super) use super::grant_owner_start::owner_start;

/// Only the owner's board login revokes or renews a grant (ADR-006 §3,
/// DL-137): a bearer token is refused before anything is read or written,
/// whatever its scopes.
fn owner_only(principal: &Principal) -> Result<(), ApiError> {
    super::guard::owner_only(principal, "revokes or renews a coordinator grant")
}

async fn current_grant(board: &Board, grant_id: &str) -> Result<Grant, ApiError> {
    board
        .store
        .grant(grant_id.to_owned())
        .await
        .map_err(ApiError::internal)?
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::NOT_FOUND,
                "unknown_grant",
                format!("no grant `{grant_id}`"),
            )
        })
}

async fn current(board: &Board, grant_id: &str) -> Result<GrantView, ApiError> {
    current_grant(board, grant_id).await.map(view)
}
