//! The creation checks of a grant that the owner gives to a session that
//! already runs (MCP P4b §6.1): the checks 3 to 8 and 11 to 14 of
//! `owner_start`, with the same helpers, the same texts and the same order.

use axum::http::StatusCode;
use fleetdeck_proto::command::{CoordinatorSpec, GrantScope};
use fleetdeck_proto::{SessionId, StationId};

use super::Board;
use super::grant_routes::MAX_GRANT_STATIONS;
use super::guard::ApiError;
use super::settings_routes::is_elevated;
use crate::store::auth::LoginSession;
use crate::store::{GrantClass, LinkV2, NewGrant};

/// True when the spec holds a desktop, capture, recording, input or click
/// field, or one of the classes `desktop`, `capture` and `input`.
fn has_start_only_access(spec: &CoordinatorSpec) -> bool {
    spec.desktop_cap.is_some()
        || spec.capture_cap.is_some()
        || spec.image_byte_cap.is_some()
        || spec.allow_recording
        || spec.recording_cap.is_some()
        || spec.recording_second_cap.is_some()
        || spec.allow_input
        || spec.allow_click
        || spec.input_scope.is_some()
        || spec.input_cap.is_some()
        || spec.input_character_cap.is_some()
        || spec
            .classes
            .iter()
            .any(|c| matches!(c.as_str(), "desktop" | "capture" | "input"))
}

/// The checks of a live give: the `NewGrant` of `session_id` on `station`,
/// with `start_command_id` empty. Whatever the spec holds for the targets,
/// the spawn cap and the mode ceilings is taken out of it.
///
/// # Errors
/// 400 `invalid_grant` for a desktop, capture, recording, input or click
/// field or class, an unknown class, no class, a bad spawn field, an unknown
/// station, more than 16 stations, or an answer cap out of range or without
/// its class; 400 `elevation_required`; 409 `unsupported`; 500 for a database
/// error.
pub(super) async fn check_live_grant(
    board: &Board,
    station: &StationId,
    session_id: &SessionId,
    spec: &mut CoordinatorSpec,
    (session, elevation): (&LoginSession, Option<&str>),
) -> Result<NewGrant, ApiError> {
    let invalid = |m: String| ApiError::new(StatusCode::BAD_REQUEST, "invalid_grant", m);
    if has_start_only_access(spec) {
        return Err(invalid(
            "desktop, screenshot and input access is given only at start".to_owned(),
        ));
    }
    // The board-to-deck list never goes on to a station.
    let targets = std::mem::take(&mut spec.stations);
    let mut classes = std::collections::BTreeSet::new();
    for name in &spec.classes {
        let class: GrantClass = serde_json::from_value(serde_json::Value::String(name.clone()))
            .map_err(|_| invalid(format!("unknown grant class `{name}`")))?;
        classes.insert(class);
    }
    if classes.is_empty() {
        return Err(invalid("a coordinator grant needs a class".to_owned()));
    }
    // MCP P0 §2.2: the spawn cap and the mode ceilings, rules 1 to 5.
    let grant_limits = board.live.get().knobs.grant;
    let (spawn_cap, max_modes) =
        super::grant_spawn::take_spawn_fields(spec, &classes, &grant_limits).map_err(invalid)?;
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
    let mut grant = NewGrant::new(station.clone(), session_id.clone());
    grant.stations.extend(targets);
    if grant.stations.len() > MAX_GRANT_STATIONS {
        return Err(invalid(format!(
            "A grant has at most {MAX_GRANT_STATIONS} stations."
        )));
    }
    let widened = grant.stations.len() > 1;
    grant.classes = classes;
    if grant.classes.contains(&GrantClass::Spawn) {
        grant.spawn_cap = spawn_cap
            .unwrap_or_else(|| crate::store::grants::grant_rules::default_spawn_cap(&grant_limits));
    }
    grant.max_modes = max_modes;
    grant.scope = spec.scope;
    grant.answer_cap = spec.answer_cap;
    if spec.idle_ttl_ms > 0 {
        grant.idle_ttl_ms = i64::try_from(spec.idle_ttl_ms).unwrap_or(i64::MAX);
    }
    if spec.expires_at > 0 {
        grant.ttl_ms = spec.expires_at.saturating_sub(board.store.now_ms());
    }
    let fleet =
        spec.scope == GrantScope::Fleet || grant.classes.iter().any(|c| c.is_fleet_control());
    if grant.classes.contains(&GrantClass::Spawn)
        && !(fleet || widened)
        && !is_elevated(board, session, elevation)
    {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "elevation_required",
            "a grant that starts sessions needs the owner's elevation".to_owned(),
        ));
    }
    if fleet || widened {
        // The answer cap is checked first: a bad request is a bad request
        // with or without elevation.
        // An omitted cap is resolved to the limit before the check (B27).
        let answers = grant.classes.contains(&GrantClass::Answer);
        match grant.answer_cap {
            Some(_) if !answers => {
                return Err(invalid("answer_cap needs the answer class".to_owned()));
            }
            Some(cap) => crate::store::grants::grant_rules::check_answer_cap(cap, &grant_limits)
                .map_err(|m| invalid(m.to_owned()))?,
            None => {}
        }
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
    }
    Ok(grant)
}
