//! The plan of an owner grant change (design §6.2): the rules that do not need
//! the elevation and the values that the change writes. The omitted caps are
//! resolved against `[grant_limits]` before the checks (B27 §3.2).

use std::collections::BTreeMap;

use fleetdeck_core::config_grant_limits::GrantLimits;

use super::{Change, START_ONLY};
use crate::store::grant_spawn_caps::spawn_caps;
use crate::store::grants::grant_rules::{check_answer_cap, default_answer_cap, default_spawn_cap};
use crate::store::grants::{Grant, GrantClass};

/// What a change writes besides the classes.
pub(super) struct Plan {
    pub(super) spawn_cap: u32,
    pub(super) max_modes: BTreeMap<String, String>,
    pub(super) answer_cap: Option<u32>,
}

/// The rules of a change that do not need the elevation (design §6.2) and the
/// values that it writes. Only the fields of newly added classes change.
pub(super) fn plan(
    before: &Grant,
    ch: &Change,
    limits: &GrantLimits,
) -> std::result::Result<Plan, &'static str> {
    if START_ONLY
        .iter()
        .any(|c| before.classes.contains(c) != ch.classes.contains(c))
    {
        return Err("desktop, screenshot and input access is given only at start");
    }
    if ch.classes.is_empty() {
        return Err("a coordinator grant needs a class");
    }
    let added = |c: GrantClass| ch.classes.contains(&c) && !before.classes.contains(&c);
    let (spawn, manage, answer) = (
        added(GrantClass::Spawn),
        added(GrantClass::Manage),
        added(GrantClass::Answer),
    );
    if (ch.spawn_cap.is_some() && !spawn)
        || (ch.max_modes.is_some() && !spawn && !manage)
        || (ch.answer_cap.is_some() && !answer)
    {
        return Err("a cap is changed with the caps route");
    }
    let mut out = Plan {
        spawn_cap: before.spawn_cap,
        max_modes: before.max_modes.clone(),
        answer_cap: before.answer_cap,
    };
    if spawn {
        out.spawn_cap = ch.spawn_cap.unwrap_or_else(|| default_spawn_cap(limits));
        out.max_modes = ch.max_modes.clone().unwrap_or_default();
    } else if manage && let Some(modes) = &ch.max_modes {
        out.max_modes = modes.clone();
    }
    if spawn || (manage && ch.max_modes.is_some()) {
        // The creation rules, on the grant as it will be.
        let after = Grant {
            classes: ch.classes.clone(),
            spawn_cap: out.spawn_cap,
            max_modes: out.max_modes.clone(),
            ..before.clone()
        };
        spawn_caps(
            &after,
            spawn.then_some(out.spawn_cap),
            Some(&out.max_modes),
            limits,
        )?;
    }
    if answer {
        let cap = ch.answer_cap.unwrap_or_else(|| default_answer_cap(limits));
        check_answer_cap(cap, limits)?;
        out.answer_cap = Some(cap);
    }
    Ok(out)
}
