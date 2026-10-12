//! The owner's coordinator grant controls (M3 6-5, ADR-006 §4, §7):
//! `GET /api/coordinator/grants` lists the grants without any credential;
//! `POST /api/coordinator/grants/{grant_id}/revoke` commits `revoked` on the
//! deck first, so every later decision for the grant is `forbidden`.

use std::sync::Arc;

use axum::Json;
use axum::extract::{Extension, Path, State};
use axum::http::StatusCode;
use fleetdeck_proto::orchestration::GrantView;

use super::Board;
use super::fleet_routes::{LIST_PAGE, audited};
use super::guard::ApiError;
use crate::store::{Grant, GrantClass, GrantState, Principal};

fn view(g: Grant) -> GrantView {
    let class = |c: &GrantClass| {
        match c {
            GrantClass::Read => "read",
            GrantClass::Mail => "mail",
            GrantClass::Drive => "drive",
            GrantClass::Spawn => "spawn",
        }
        .to_owned()
    };
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
                .saturating_add(super::fleet_routes::DELIVERY_TTL_MS),
        })
        .await
        .map_err(ApiError::internal)?;
    Ok(())
}

/// The version of the deck-served control skill (ADR-006 §7.1).
pub const CONTROL_SKILL_VERSION: &str = "1";
/// The deck-served control skill: the MCP `instructions` of a coordinator
/// session. It holds no secret and no grant data.
pub const CONTROL_SKILL: &str = include_str!("../control_skill.md");

/// The owner's coordinator start (ADR-006 §4, §7.1; DL-217): the board's
/// `session.start` with a `coordinator` request creates the grant on the
/// deck in this call, and the deck fills in the grant id, the expiry and
/// its control skill. Whatever grant id or skill the board sent is
/// replaced. A resend of the same `command_id` finds the same grant.
///
/// # Errors
/// 400 `invalid_grant` for an unknown class, no class or a limit out of
/// range; 500 for a database error.
pub(super) async fn owner_start(
    board: &Board,
    station: &fleetdeck_proto::StationId,
    command_id: &fleetdeck_proto::CommandId,
    spec: &mut fleetdeck_proto::command::CoordinatorSpec,
) -> Result<(), ApiError> {
    let invalid = |m: String| ApiError::new(StatusCode::BAD_REQUEST, "invalid_grant", m);
    let mut classes = std::collections::BTreeSet::new();
    for name in &spec.classes {
        let class: GrantClass = serde_json::from_value(serde_json::Value::String(name.clone()))
            .map_err(|_| invalid(format!("unknown grant class `{name}`")))?;
        classes.insert(class);
    }
    if classes.is_empty() {
        return Err(invalid("a coordinator grant needs a class".to_owned()));
    }
    let mut grant = crate::store::NewGrant::new(station.clone(), "".into());
    grant.classes = classes;
    grant.start_command_id = Some(command_id.to_string());
    if spec.idle_ttl_ms > 0 {
        grant.idle_ttl_ms = i64::try_from(spec.idle_ttl_ms).unwrap_or(i64::MAX);
    }
    if spec.expires_at > 0 {
        grant.ttl_ms = spec.expires_at.saturating_sub(board.store.now_ms());
    }
    let g = board.store.create_grant(grant).await.map_err(|e| match e {
        crate::store::DeckError::Invalid(m) => invalid(m),
        e => ApiError::internal(e),
    })?;
    // The station relays coordinator calls with the link scope `coordinate`
    // that the grant added; the link's grant cache follows (DL-211).
    board
        .link
        .peers
        .refresh(station.clone())
        .await
        .map_err(ApiError::internal)?;
    spec.grant_id = g.grant_id;
    spec.idle_ttl_ms = u64::try_from(g.idle_ttl_ms).unwrap_or(0);
    spec.expires_at = g.expires_at;
    spec.classes = view_classes(&g.classes);
    spec.control_skill = fleetdeck_proto::command::ControlSkill {
        version: CONTROL_SKILL_VERSION.to_owned(),
        text: CONTROL_SKILL.to_owned(),
    };
    Ok(())
}

fn view_classes(classes: &std::collections::BTreeSet<GrantClass>) -> Vec<String> {
    classes
        .iter()
        .filter_map(|c| serde_json::to_value(c).ok())
        .filter_map(|v| v.as_str().map(str::to_owned))
        .collect()
}

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
