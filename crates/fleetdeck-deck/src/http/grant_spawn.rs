//! The spawn rules of a coordinator grant at creation (MCP P0 §2.2, §2.4):
//! the spawn cap and the mode ceilings that the board sends in the
//! `CoordinatorSpec`, and the audit detail of the grant.

use std::collections::{BTreeMap, BTreeSet};

use axum::http::StatusCode;
use fleetdeck_proto::StationId;
use fleetdeck_proto::command::CoordinatorSpec;
use fleetdeck_proto::coordinator_desktop_text::{code as desktop_code, text as desktop_text};
use fleetdeck_proto::version::feature;

use super::Board;
use super::guard::ApiError;
use crate::store::coordinator_ops::valid_ceiling;
use crate::store::grants::MAX_SPAWN_CAP;
use crate::store::{Grant, GrantClass, LinkFeature};

/// Takes `spawn_cap` and `max_modes` out of the spec (they never go on to a
/// station) and checks the rules 1 to 5 of MCP P0 §2.2 in order. The `Err`
/// text is the message of the rule.
///
/// # Errors
/// The message of the first rule that the fields break.
pub(super) fn take_spawn_fields(
    spec: &mut CoordinatorSpec,
    classes: &BTreeSet<GrantClass>,
) -> Result<(Option<u32>, BTreeMap<String, String>), String> {
    let spawn_cap = spec.spawn_cap.take();
    let max_modes = std::mem::take(&mut spec.max_modes);
    let spawns = classes.contains(&GrantClass::Spawn);
    if spawn_cap.is_some() && !spawns {
        return Err("spawn_cap needs the spawn class".to_owned());
    }
    if spawn_cap.is_some_and(|cap| cap > MAX_SPAWN_CAP) {
        return Err("spawn_cap is 0 to 50".to_owned());
    }
    if !max_modes.is_empty() && !spawns && !classes.contains(&GrantClass::Manage) {
        return Err("max_modes needs the spawn or manage class".to_owned());
    }
    for family in max_modes.keys() {
        if family != "claude" && family != "codex" {
            return Err(format!("unknown provider family `{family}`"));
        }
    }
    for (family, ceiling) in &max_modes {
        if !valid_ceiling(family, ceiling) {
            return Err(format!(
                "the mode ceiling `{ceiling}` is not valid for `{family}`"
            ));
        }
    }
    Ok((spawn_cap, max_modes))
}

/// The detail of the audit row `coordinator.grant_created` (MCP P0 §2.4):
/// the stations, the spawn cap with the class `spawn`, and the mode
/// ceilings when there are any.
pub(crate) fn audit_detail(g: &Grant, named: &str) -> String {
    let mut detail = format!("stations={named}");
    if g.classes.contains(&GrantClass::Spawn) {
        detail.push_str(&format!(";spawn_cap={}", g.spawn_cap));
    }
    if !g.max_modes.is_empty() {
        let modes = g
            .max_modes
            .iter()
            .map(|(family, ceiling)| format!("{family}:{ceiling}"))
            .collect::<Vec<_>>()
            .join(",");
        detail.push_str(&format!(";max_modes={modes}"));
    }
    detail
}

/// The features that a `desktop` or `capture` grant needs now (R13 P6 §5.1,
/// §10): the caller's link has the caller features, and every other station
/// of the grant has the target features. The caller's own station is a target
/// only when a call names it, and that call is checked at call time, so a
/// headless coordinator station stays supported (§2.3). A missing feature or
/// link is 409 `unsupported`.
pub(super) fn desktop_features(
    board: &Board,
    caller: &StationId,
    classes: &std::collections::BTreeSet<GrantClass>,
    stations: &std::collections::BTreeSet<StationId>,
    (recording, input, click, any): (bool, bool, bool, bool),
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
    // R13 P5b-2: a recording allowance also needs `capture_recording_mcp.v1`
    // on the caller's link and on every other station of the grant.
    if recording {
        on_caller.push(feature::CAPTURE_RECORDING_MCP);
        on_target.push(feature::CAPTURE_RECORDING_MCP);
    }
    // R13 P7: an input allowance also needs `desktop_input_mcp.v1` on the
    // caller's link and on every other station of the grant.
    if input {
        on_caller.push(feature::DESKTOP_INPUT_MCP);
        on_target.push(feature::DESKTOP_INPUT_MCP);
    }
    // R13 P7b §4: a click allowance also needs `desktop_click_mcp.v1`, and
    // the `any_window` scope `desktop_input_any.v1`, on the caller's link and
    // on every other station of the grant.
    if click {
        on_caller.push(feature::DESKTOP_CLICK_MCP);
        on_target.push(feature::DESKTOP_CLICK_MCP);
    }
    if any {
        on_caller.push(feature::DESKTOP_INPUT_ANY);
        on_target.push(feature::DESKTOP_INPUT_ANY);
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
