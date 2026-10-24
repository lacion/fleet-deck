//! The spawn cap and the mode ceilings of a cap change (MCP P4a §8): the rules
//! of the values and the widening test, compared with the stored grant.

use std::collections::BTreeMap;

use fleetdeck_core::config_grant_limits::GrantLimits;
use fleetdeck_proto::command::GrantKind;

use super::coordinator_ops::{is_bypass, mode_within, valid_ceiling};
use super::grants::grant_rules::check_spawn_cap;
use super::grants::{Grant, GrantClass};

/// The spawn cap and the mode ceilings after a cap change (MCP P4a §8).
pub(crate) struct SpawnCaps {
    pub spawn_cap: u32,
    pub max_modes: BTreeMap<String, String>,
    /// True when the change widens the spawn cap or a ceiling.
    pub widens: bool,
}

/// Checks the new spawn cap and the new mode ceilings of `grant` (the rules a
/// to f of MCP P4a §8, in order) and says whether the change widens. A `None`
/// field keeps the stored value; a given map replaces the stored one. The
/// spawn cap is checked against the spawn limit of the grant kind (B27); a
/// worker grant keeps a stored bypass ceiling that the request repeats, and
/// never gets a new one (§3.4).
///
/// # Errors
/// The fixed text of the first rule that the fields break.
pub(crate) fn spawn_caps(
    grant: &Grant,
    spawn_cap: Option<u32>,
    max_modes: Option<&BTreeMap<String, String>>,
    limits: &GrantLimits,
) -> Result<SpawnCaps, &'static str> {
    let spawns = grant.classes.contains(&GrantClass::Spawn);
    if spawn_cap.is_some() && !spawns {
        return Err("spawn_cap needs the spawn class");
    }
    if let Some(cap) = spawn_cap {
        check_spawn_cap(cap, grant.kind, limits)?;
    }
    if max_modes.is_some() && !spawns && !grant.classes.contains(&GrantClass::Manage) {
        return Err("max_modes needs the spawn or manage class");
    }
    if let Some(modes) = max_modes {
        if modes.keys().any(|f| f != "claude" && f != "codex") {
            return Err("max_modes has only the families claude and codex");
        }
        let kept = |f: &str, m: &str| {
            grant.kind == GrantKind::Worker
                && is_bypass(f, Some(m))
                && grant.max_modes.get(f).is_some_and(|old| old == m)
        };
        if modes
            .iter()
            .any(|(f, m)| !valid_ceiling(f, m) && !kept(f, m))
        {
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
