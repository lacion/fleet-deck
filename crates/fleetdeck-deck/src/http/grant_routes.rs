//! The owner's coordinator grant controls (M3 6-5, ADR-006 §4, §7):
//! `GET /api/coordinator/grants` lists the grants without any credential;
//! `POST /api/coordinator/grants/{grant_id}/revoke` commits `revoked` on the
//! deck first, so every later decision for the grant is `forbidden`.

use std::sync::Arc;

use axum::Json;
use axum::extract::{Extension, Path, State};
use axum::http::StatusCode;
use fleetdeck_proto::StationId;
use fleetdeck_proto::coordinator_desktop_text::{code as desktop_code, text as desktop_text};
use fleetdeck_proto::orchestration::GrantView;
use fleetdeck_proto::version::feature;

use super::Board;
use super::fleet_routes::{LIST_PAGE, audited};
use super::guard::ApiError;
use super::settings_routes::is_elevated;
use crate::store::auth::LoginSession;
use crate::store::{
    AuditResult, Grant, GrantClass, GrantState, LinkFeature, LinkV2, NewAudit, Principal,
};

/// The most stations that one grant names (its own station included).
pub const MAX_GRANT_STATIONS: usize = 16;

fn view(g: Grant) -> GrantView {
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
        // A used counter is shown only with its own cap (R13 P6 §8.1).
        desktop_cap: g.desktop_cap,
        desktop_used: g.desktop_cap.map(|_| g.desktop_used),
        capture_cap: g.capture_cap,
        capture_used: g.capture_cap.map(|_| g.capture_used),
        image_byte_cap: g.image_byte_cap,
        image_byte_used: g.image_byte_cap.map(|_| g.image_byte_used),
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
pub const CONTROL_SKILL_VERSION: &str = "2";
/// The deck-served control skill: the MCP `instructions` of a coordinator
/// session. It holds no secret and no grant data.
pub const CONTROL_SKILL: &str = include_str!("../control_skill.md");

/// The owner's coordinator start (ADR-006 §4, §7.1; DL-217): the board's
/// `session.start` with a `coordinator` request creates the grant on the
/// deck in this call, and the deck fills in the grant id, the expiry and
/// its control skill. Whatever grant id or skill the board sent is
/// replaced. A resend of the same `command_id` finds the same grant.
///
/// The grant's stations are the coordinator's own station and the target
/// stations of `spec.stations`; the deck takes that list out of the spec, so
/// the start command that goes to a station never carries it. A fleet-control
/// grant (the fleet scope, or the class `inspect`, `manage` or `answer`) or a
/// grant with a station other than its own needs the owner's elevation in the
/// same request, and every station of the grant must have negotiated
/// `coordinator.v2` on their current link (ADR-008 section 3). The classes
/// `desktop` and `capture` are fleet control: besides that, the caller's link
/// and every other station of the grant need the features of R13 P6 §5.1
/// (the caller's own desktop is checked when a call names it). The three desktop
/// caps are read from the spec and never go on to a station.
///
/// # Errors
/// 400 `invalid_grant` for an unknown class, no class, an unknown station, more
/// than 16 stations, an answer cap or a desktop cap out of range or without
/// its class, or a limit out of range; 400 `elevation_required`; 409
/// `unsupported`; 500 for a database error.
pub(super) async fn owner_start(
    board: &Board,
    station: &fleetdeck_proto::StationId,
    command_id: &fleetdeck_proto::CommandId,
    spec: &mut fleetdeck_proto::command::CoordinatorSpec,
    (session, elevation): (&LoginSession, Option<&str>),
) -> Result<(), ApiError> {
    let invalid = |m: String| ApiError::new(StatusCode::BAD_REQUEST, "invalid_grant", m);
    // The board-to-deck list never goes on to a station.
    let targets = std::mem::take(&mut spec.stations);
    // The desktop caps are read here and never go on to a station either
    // (R13 P6 item 18 of decision O12-4).
    let (desktop_cap, capture_cap, image_byte_cap) = (
        spec.desktop_cap.take(),
        spec.capture_cap.take(),
        spec.image_byte_cap.take(),
    );
    let mut classes = std::collections::BTreeSet::new();
    for name in &spec.classes {
        let class: GrantClass = serde_json::from_value(serde_json::Value::String(name.clone()))
            .map_err(|_| invalid(format!("unknown grant class `{name}`")))?;
        classes.insert(class);
    }
    if classes.is_empty() {
        return Err(invalid("a coordinator grant needs a class".to_owned()));
    }
    for target in &targets {
        let paired = board
            .store
            .station(target.clone())
            .await
            .map_err(ApiError::internal)?;
        if paired.is_none() {
            return Err(invalid(format!("Unknown station {target}.")));
        }
    }
    let mut grant = crate::store::NewGrant::new(station.clone(), "".into());
    grant.stations.extend(targets);
    if grant.stations.len() > MAX_GRANT_STATIONS {
        return Err(invalid(format!(
            "A grant has at most {MAX_GRANT_STATIONS} stations."
        )));
    }
    let widened = grant.stations.len() > 1;
    grant.classes = classes;
    grant.scope = spec.scope;
    grant.answer_cap = spec.answer_cap;
    grant.desktop_cap = desktop_cap;
    grant.capture_cap = capture_cap;
    grant.image_byte_cap = image_byte_cap;
    grant.start_command_id = Some(command_id.to_string());
    if spec.idle_ttl_ms > 0 {
        grant.idle_ttl_ms = i64::try_from(spec.idle_ttl_ms).unwrap_or(i64::MAX);
    }
    if spec.expires_at > 0 {
        grant.ttl_ms = spec.expires_at.saturating_sub(board.store.now_ms());
    }
    let fleet = spec.scope == fleetdeck_proto::command::GrantScope::Fleet
        || grant.classes.iter().any(|c| c.is_fleet_control());
    if fleet || widened {
        // The answer cap is checked first: a bad request is a bad request
        // with or without elevation.
        let answers = grant.classes.contains(&GrantClass::Answer);
        match grant.answer_cap {
            Some(_) if !answers => {
                return Err(invalid("answer_cap needs the answer class".to_owned()));
            }
            Some(cap) if cap > crate::store::grants::MAX_ANSWER_CAP => {
                return Err(invalid("answer_cap is 0 to 5000".to_owned()));
            }
            _ => {}
        }
        // The desktop caps, before the elevation, like the answer cap.
        crate::store::grants::check_desktop_caps(
            &grant.classes,
            grant.desktop_cap,
            grant.capture_cap,
            grant.image_byte_cap,
        )
        .map_err(|m| invalid(m.to_owned()))?;
        if !is_elevated(board, session, elevation) {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "elevation_required",
                "a fleet-control grant needs the owner's elevation".to_owned(),
            ));
        }
        // Every station of the grant: the coordinator's own and the targets.
        for st in &grant.stations {
            if board.store.link_v2(st) != LinkV2::Negotiated {
                return Err(ApiError::new(
                    StatusCode::CONFLICT,
                    "unsupported",
                    format!("station `{st}` has no link that negotiated coordinator.v2"),
                ));
            }
        }
        desktop_features(board, station, &grant.classes, &grant.stations)?;
    }
    let g = board.store.create_grant(grant).await.map_err(|e| match e {
        crate::store::DeckError::Invalid(m) => invalid(m),
        e => ApiError::internal(e),
    })?;
    // Each station of the grant relays coordinator calls with the link scope
    // `coordinate` that the grant added; the link's grant cache follows
    // (DL-211).
    board
        .link
        .peers
        .refresh_changed()
        .await
        .map_err(ApiError::internal)?;
    let named = g
        .stations
        .iter()
        .map(StationId::as_str)
        .collect::<Vec<_>>()
        .join(",");
    board
        .store
        .audit_or_log(
            NewAudit::new("owner", "coordinator.grant_created", AuditResult::Ok)
                .target(g.grant_id.clone())
                .command(command_id.clone())
                .detail(format!("stations={named}")),
        )
        .await;
    spec.grant_id = g.grant_id;
    spec.idle_ttl_ms = u64::try_from(g.idle_ttl_ms).unwrap_or(0);
    spec.expires_at = g.expires_at;
    spec.classes = view_classes(&g.classes);
    spec.scope = g.scope;
    spec.answer_cap = g.answer_cap;
    spec.control_skill = fleetdeck_proto::command::ControlSkill {
        version: CONTROL_SKILL_VERSION.to_owned(),
        text: CONTROL_SKILL.to_owned(),
    };
    Ok(())
}

/// The features that a `desktop` or `capture` grant needs now (R13 P6 §5.1,
/// §10): the caller's link has the caller features, and every other station
/// of the grant has the target features. The caller's own station is a target
/// only when a call names it, and that call is checked at call time, so a
/// headless coordinator station stays supported (§2.3). A missing feature or
/// link is 409 `unsupported`.
fn desktop_features(
    board: &Board,
    caller: &StationId,
    classes: &std::collections::BTreeSet<GrantClass>,
    stations: &std::collections::BTreeSet<StationId>,
) -> Result<(), ApiError> {
    let capture = classes.contains(&GrantClass::Capture);
    if !capture && !classes.contains(&GrantClass::Desktop) {
        return Ok(());
    }
    let mut on_caller = vec![
        feature::COORDINATOR,
        feature::COORDINATOR_V2,
        feature::DESKTOP_MCP,
    ];
    let mut on_target = vec![
        feature::COORDINATOR_V2,
        feature::DESKTOP,
        feature::DESKTOP_AUTHORITY,
        feature::DESKTOP_MCP,
    ];
    if capture {
        on_caller.push(feature::CAPTURE_MCP);
        on_target.extend([
            feature::DESKTOP_CAPTURE,
            feature::ARTIFACTS_DELETE,
            feature::CAPTURE_MCP,
        ]);
    }
    let has_all = |station: &StationId, wanted: &[&str]| {
        wanted
            .iter()
            .all(|f| board.store.link_feature(station, f) == LinkFeature::Negotiated)
    };
    let ready = has_all(caller, &on_caller)
        && stations
            .iter()
            .filter(|s| *s != caller)
            .all(|s| has_all(s, &on_target));
    if ready {
        Ok(())
    } else {
        Err(ApiError::new(
            StatusCode::CONFLICT,
            desktop_code::UNSUPPORTED,
            desktop_text(desktop_code::UNSUPPORTED)
                .unwrap_or(desktop_code::UNSUPPORTED)
                .to_owned(),
        ))
    }
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
