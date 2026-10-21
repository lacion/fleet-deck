//! The station form of a stored owner grant (MCP P4b §3, §6): the
//! `CoordinatorSpec` that a `session.coordinator_set` carries. It holds what
//! the rewrite at the end of `http/grant_owner_start.rs` puts into the start
//! command of a grant, read from the stored grant instead of the request.
//! The caps and the target stations of the deck never go on to a station.

use std::collections::{BTreeMap, BTreeSet};

use fleetdeck_proto::command::{ControlSkill, CoordinatorSpec};
use fleetdeck_proto::coordinator_spec::InputScope;

use super::{Grant, GrantClass};
use crate::http::grant_routes::{CONTROL_SKILL, CONTROL_SKILL_VERSION};

/// The features of the station's link that filter the allowances of a spec: a
/// station that lacks a feature never sees its flags.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SpecFeatures {
    /// `capture_recording_mcp.v1`.
    pub recording: bool,
    /// `desktop_input_mcp.v1`.
    pub input: bool,
    /// `desktop_click_mcp.v1`.
    pub click: bool,
    /// `desktop_input_any.v1`.
    pub input_any: bool,
}

/// The names of `classes`, as the board and the station read them.
fn view_classes(classes: &BTreeSet<GrantClass>) -> Vec<String> {
    classes
        .iter()
        .filter_map(|c| serde_json::to_value(c).ok())
        .filter_map(|v| v.as_str().map(str::to_owned))
        .collect()
}

/// The station spec of `grant` for a station link with `features`.
#[must_use]
pub fn station_spec(grant: &Grant, features: SpecFeatures) -> CoordinatorSpec {
    let input_link = grant.allow_input && features.input;
    CoordinatorSpec {
        grant_id: grant.grant_id.clone(),
        classes: view_classes(&grant.classes),
        idle_ttl_ms: u64::try_from(grant.idle_ttl_ms).unwrap_or(0),
        expires_at: grant.expires_at,
        control_skill: ControlSkill {
            version: CONTROL_SKILL_VERSION.to_owned(),
            text: CONTROL_SKILL.to_owned(),
        },
        scope: grant.scope,
        answer_cap: grant.answer_cap,
        // The caps of the deck never go on to a station.
        desktop_cap: None,
        capture_cap: None,
        image_byte_cap: None,
        allow_recording: (grant.recording_cap != 0 || grant.recording_second_cap != 0)
            && features.recording,
        recording_cap: None,
        recording_second_cap: None,
        allow_input: input_link,
        input_scope: grant
            .input_scope
            .filter(|scope| input_link && (*scope != InputScope::AnyWindow || features.input_any)),
        allow_click: grant.allow_click && input_link && features.click,
        input_cap: None,
        input_character_cap: None,
        stations: Vec::new(),
        spawn_cap: None,
        max_modes: BTreeMap::new(),
        kind: grant.kind,
    }
}
