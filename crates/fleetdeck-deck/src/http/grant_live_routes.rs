//! The owner's live grant routes (MCP P4b §6.1, §6.2): give a grant to a
//! session that already runs, and change the classes of an owner grant that
//! has a `session_owner_grant` row. Revoke keeps its route
//! (`grant_routes::revoke`).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use axum::Json;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Extension, Path, State};
use axum::http::{HeaderMap, StatusCode};
use fleetdeck_proto::command::{CoordinatorSpec, GrantKind};
use fleetdeck_proto::orchestration::GrantView;
use fleetdeck_proto::version::feature;
use fleetdeck_proto::{SessionId, StationId};

use super::Board;
use super::artifact_delete::ELEVATION_HEADER;
use super::grant_check::check_live_grant;
use super::grant_routes::{audit_detail, current_grant, unknown_grant, view};
use super::guard::ApiError;
use super::settings_routes::{is_elevated, owner};
use crate::store::grants::owner_grants::{Change, ChangeError, GiveError};
use crate::store::{
    AuditResult, DeckError, Grant, GrantClass, LinkFeature, LinkV2, NewAudit, Principal,
};

/// The body of `POST /api/coordinator/grants/{grant_id}/classes`. An absent
/// cap keeps its value.
#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClassesBody {
    classes: Vec<String>,
    spawn_cap: Option<u32>,
    max_modes: Option<BTreeMap<String, String>>,
    answer_cap: Option<u32>,
}

fn invalid_body() -> ApiError {
    ApiError::new(
        StatusCode::BAD_REQUEST,
        "invalid_body",
        "the request body is not valid",
    )
}

fn refused_row(
    principal: &Principal,
    action: &'static str,
    target: &str,
    e: &ApiError,
) -> NewAudit {
    NewAudit::new(principal.actor(), action, AuditResult::Refused)
        .target(target.to_owned())
        .detail(e.code)
}

/// `POST /api/coordinator/sessions/{station_id}/{session_id}/grant` (owner
/// only, MCP P4b §6.1): gives an owner grant to a session that is running or
/// hibernated. The body is the `CoordinatorSpec` that the board sends with a
/// start; the deck fills in the grant id, the expiry and the skill.
///
/// # Errors
/// 400 `invalid_body`; 400 `invalid_grant` (a start-only field or class, or
/// the creation rules of `owner_start`); 400 `elevation_required`; 403
/// `owner_only`; 404 `unknown_session`; 409 `not_supported`, `unsupported`
/// or `has_grant`.
pub async fn give(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    headers: HeaderMap,
    Path((station_id, session_id)): Path<(String, String)>,
    body: Result<Json<CoordinatorSpec>, JsonRejection>,
) -> Result<Json<GrantView>, ApiError> {
    const ACTION: &str = "coordinator.grant_give";
    let login = owner(
        &board,
        &principal,
        ACTION,
        &session_id,
        "gives a coordinator grant",
    )
    .await?;
    let token = headers.get(ELEVATION_HEADER).and_then(|v| v.to_str().ok());
    let result = async {
        let Json(mut spec) = body.map_err(|_| invalid_body())?;
        let station = StationId::from(station_id.clone());
        let session = SessionId::from(session_id.clone());
        let live = |name: &str| board.store.link_feature(&station, name) == LinkFeature::Negotiated;
        if !(live(feature::COORDINATOR) && live(feature::COORDINATOR_LIVE)) {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "not_supported",
                "this station cannot change grants live",
            ));
        }
        let known = board
            .store
            .session_known(station_id.clone(), session_id.clone())
            .await
            .map_err(ApiError::internal)?;
        if !known {
            return Err(super::access::unknown_session(&station_id, &session_id));
        }
        if spec.kind != GrantKind::Coordinator {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_grant",
                "the grant kind is set by the deck",
            ));
        }
        let grant =
            check_live_grant(&board, &station, &session, &mut spec, (&login, token)).await?;
        let g = board
            .store
            .give(station, session, grant)
            .await
            .map_err(|e| match e {
                DeckError::Invalid(m) => ApiError::new(StatusCode::BAD_REQUEST, "invalid_grant", m),
                e => ApiError::internal(e),
            })?
            .map_err(|GiveError::HasGrant| {
                ApiError::new(
                    StatusCode::CONFLICT,
                    "has_grant",
                    "the session already has a grant; renew or revoke it first",
                )
            })?;
        board
            .link
            .peers
            .refresh_changed()
            .await
            .map_err(ApiError::internal)?;
        Ok(g)
    }
    .await;
    match result {
        Ok(g) => {
            let named = g
                .stations
                .iter()
                .map(StationId::as_str)
                .collect::<Vec<_>>()
                .join(",");
            board
                .store
                .audit_or_log(
                    NewAudit::new(
                        principal.actor(),
                        "coordinator.grant_created",
                        AuditResult::Ok,
                    )
                    .target(g.grant_id.clone())
                    .detail(format!("{};live=true", audit_detail(&g, &named))),
                )
                .await;
            Ok(Json(view(g, true)))
        }
        Err(e) => {
            board
                .store
                .audit_or_log(refused_row(&principal, ACTION, &session_id, &e))
                .await;
            Err(e)
        }
    }
}

/// The audit detail of a change: the classes before and after, and the caps
/// of the newly added classes.
fn change_detail(before: &Grant, after: &Grant) -> String {
    let added = |c: GrantClass| after.classes.contains(&c) && !before.classes.contains(&c);
    let mut new = serde_json::json!({"classes": after.classes});
    if added(GrantClass::Spawn) {
        new["spawn_cap"] = after.spawn_cap.into();
    }
    if added(GrantClass::Spawn) || added(GrantClass::Manage) {
        new["max_modes"] = serde_json::json!(after.max_modes);
    }
    if added(GrantClass::Answer) {
        new["answer_cap"] = serde_json::json!(after.answer_cap);
    }
    serde_json::json!({"old": {"classes": before.classes}, "new": new}).to_string()
}

/// `POST /api/coordinator/grants/{grant_id}/classes` (owner only, MCP P4b
/// §6.2): changes the classes of an owner grant that has a record. Adding
/// access needs the elevation of the header `x-fleetdeck-elevation`;
/// removing access needs none.
///
/// # Errors
/// 400 `invalid_body`; 400 `invalid_grant`; 400 `elevation_required`; 403
/// `owner_only`; 404 `unknown_grant`; 409 `worker_grant`, `not_supported`,
/// `fleet_control_off` or `unsupported`.
pub async fn classes(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    headers: HeaderMap,
    Path(grant_id): Path<String>,
    body: Result<Json<ClassesBody>, JsonRejection>,
) -> Result<Json<GrantView>, ApiError> {
    const ACTION: &str = "coordinator.grant_classes";
    let login = owner(
        &board,
        &principal,
        ACTION,
        &grant_id,
        "changes the classes of a coordinator grant",
    )
    .await?;
    let token = headers.get(ELEVATION_HEADER).and_then(|v| v.to_str().ok());
    let result: Result<(Grant, Grant), ApiError> = async {
        let Json(req) = body.map_err(|_| invalid_body())?;
        let mut classes = BTreeSet::new();
        for name in &req.classes {
            let class: GrantClass = serde_json::from_value(serde_json::Value::String(name.clone()))
                .map_err(|_| {
                    ApiError::new(
                        StatusCode::BAD_REQUEST,
                        "invalid_grant",
                        format!("unknown grant class `{name}`"),
                    )
                })?;
            classes.insert(class);
        }
        let before = current_grant(&board, &grant_id).await?;
        let adds_fleet_control = classes
            .iter()
            .any(|c| c.is_fleet_control() && !before.classes.contains(c));
        if adds_fleet_control {
            if !board.store.fleet_control() {
                return Err(ApiError::new(
                    StatusCode::CONFLICT,
                    "fleet_control_off",
                    "fleet control is off",
                ));
            }
            for st in &before.stations {
                if board.store.link_v2(st) != LinkV2::Negotiated {
                    return Err(ApiError::new(
                        StatusCode::CONFLICT,
                        "unsupported",
                        format!("station `{st}` has no link that negotiated coordinator.v2"),
                    ));
                }
            }
        }
        let change = Change {
            classes,
            spawn_cap: req.spawn_cap,
            max_modes: req.max_modes,
            answer_cap: req.answer_cap,
            allow_widening: is_elevated(&board, &login, token),
            now: 0,
        };
        let (before, after) = board
            .store
            .change_access(grant_id.clone(), change)
            .await
            .map_err(ApiError::internal)?
            .map_err(|e| match e {
                ChangeError::NotFound => unknown_grant(&grant_id),
                ChangeError::Worker => ApiError::new(
                    StatusCode::CONFLICT,
                    "worker_grant",
                    "a worker grant has fixed classes",
                ),
                ChangeError::NoRow => ApiError::new(
                    StatusCode::CONFLICT,
                    "not_supported",
                    "this grant cannot change live",
                ),
                ChangeError::Invalid(text) => {
                    ApiError::new(StatusCode::BAD_REQUEST, "invalid_grant", text)
                }
                ChangeError::ElevationRequired => ApiError::new(
                    StatusCode::BAD_REQUEST,
                    "elevation_required",
                    "adding access needs the owner's elevation",
                ),
            })?;
        board
            .link
            .peers
            .refresh_changed()
            .await
            .map_err(ApiError::internal)?;
        Ok((before, after))
    }
    .await;
    let row = match &result {
        Ok((before, after)) => NewAudit::new(principal.actor(), ACTION, AuditResult::Ok)
            .target(grant_id.clone())
            .detail(change_detail(before, after)),
        Err(e) => refused_row(&principal, ACTION, &grant_id, e),
    };
    board.store.audit_or_log(row).await;
    result.map(|(_, after)| Json(view(after, true)))
}
