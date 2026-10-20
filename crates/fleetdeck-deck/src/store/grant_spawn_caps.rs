//! The spawn cap and the mode ceilings of a cap change (MCP P4a §8): the rules
//! of the values and the widening test, compared with the stored grant.

use std::collections::BTreeMap;

use fleetdeck_proto::command::GrantKind;

use super::coordinator_ops::{mode_within, valid_ceiling};
use super::grants::{Grant, GrantClass, MAX_SPAWN_CAP};
use super::worker_grant::MAX_WORKER_SPAWN_CAP;

/// The spawn cap and the mode ceilings after a cap change (MCP P4a §8).
pub(crate) struct SpawnCaps {
    pub spawn_cap: u32,
    pub max_modes: BTreeMap<String, String>,
    /// True when the change widens the spawn cap or a ceiling.
    pub widens: bool,
}

/// Checks the new spawn cap and the new mode ceilings of `grant` (the rules a
/// to f of MCP P4a §8, in order) and says whether the change widens. A `None`
/// field keeps the stored value; a given map replaces the stored one.
///
/// # Errors
/// The fixed text of the first rule that the fields break.
pub(crate) fn spawn_caps(
    grant: &Grant,
    spawn_cap: Option<u32>,
    max_modes: Option<&BTreeMap<String, String>>,
) -> Result<SpawnCaps, &'static str> {
    let spawns = grant.classes.contains(&GrantClass::Spawn);
    if spawn_cap.is_some() && !spawns {
        return Err("spawn_cap needs the spawn class");
    }
    if spawn_cap.is_some_and(|cap| cap > MAX_SPAWN_CAP) {
        return Err("spawn_cap is 0 to 50");
    }
    if grant.kind == GrantKind::Worker && spawn_cap.is_some_and(|cap| cap > MAX_WORKER_SPAWN_CAP) {
        return Err("a worker grant has a spawn cap of at most 10");
    }
    if max_modes.is_some() && !spawns && !grant.classes.contains(&GrantClass::Manage) {
        return Err("max_modes needs the spawn or manage class");
    }
    if let Some(modes) = max_modes {
        if modes.keys().any(|f| f != "claude" && f != "codex") {
            return Err("max_modes has only the families claude and codex");
        }
        if modes.iter().any(|(f, m)| !valid_ceiling(f, m)) {
            return Err("a mode ceiling is not valid for its family");
        }
    }
    let new_cap = spawn_cap.unwrap_or(grant.spawn_cap);
    let new_modes = max_modes
        .cloned()
        .unwrap_or_else(|| grant.max_modes.clone());
    let widens = new_cap > grant.spawn_cap
        || new_modes.iter().any(|(family, mode)| {
            grant
                .max_modes
                .get(family)
                .is_none_or(|old| !mode_within(family, Some(mode), old))
        });
    Ok(SpawnCaps {
        spawn_cap: new_cap,
        max_modes: new_modes,
        widens,
    })
}

#[cfg(test)]
#[path = "grant_spawn_caps_tests.rs"]
mod tests;
