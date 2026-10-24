//! The bypass start of a worker grant and the default mode of a coordinator
//! start (B27 §3.4, §3.5).

use fleetdeck_proto::ProviderInstanceId;
use fleetdeck_proto::StationId;
use fleetdeck_proto::command::{GrantKind, SessionCommand};
use rusqlite::{Connection, params};

use super::Result;
use super::coordinator_fleet::{mode_refusal, session_family};
use super::coordinator_ops::modes::{CLAUDE_MODES, CODEX_DEFAULT, CODEX_POLICIES, CODEX_SANDBOXES};
use super::coordinator_ops::{Context, is_bypass, mode_within};
use super::grants::Grant;

/// Rule 2 of §3.4: the deck setting is off.
const SETTING_OFF: &str =
    "a worker can start a bypass session only while the deck allows bypass mode for workers";
/// Rule 4 of §3.4: the station does not offer the mode.
const STATION_NO_BYPASS: &str = "the station does not allow bypass permissions";
/// Rule 5 of §3.4: the caller does not run in a bypass mode.
const CALLER_NOT_BYPASS: &str =
    "a worker can start a bypass session only while it runs in a bypass mode itself";

/// Why a start of `grant` in the bypass `mode` of `family` on `station` is
/// refused, or `None` when it may run (rules 1-5 of §3.4, in order).
pub(super) fn bypass_refusal(
    grant: &Grant,
    ctx: &Context,
    (station, instance): (&StationId, &ProviderInstanceId),
    (family, mode): (&str, &str),
) -> Option<String> {
    // Rule 1: owner grants keep the absolute rule.
    if grant.kind != GrantKind::Worker {
        return Some("a coordinator never starts a session in a bypass mode".to_owned());
    }
    // Rule 2: the deck setting, read in the turn of the decision.
    if !ctx.env.knobs.worker_bypass {
        return Some(SETTING_OFF.to_owned());
    }
    // Rule 3: within the grant ceiling.
    let ceiling = grant.max_modes.get(family).map_or("", String::as_str);
    if !mode_within(family, Some(mode), ceiling) {
        return Some(format!("the mode is above the grant's ceiling `{ceiling}`"));
    }
    // Rule 4: the station offers the mode.
    let offered = provider_of(ctx, station, instance)
        .and_then(|p| p.capabilities.as_ref())
        .is_some_and(|c| c.modes.iter().any(|m| m == mode));
    if !offered {
        return Some(STATION_NO_BYPASS.to_owned());
    }
    // Rule 5: the caller runs in a bypass mode itself.
    let caller_bypass = session_family(ctx, &grant.station_id, &grant.session_id)
        .is_some_and(|(f, m)| is_bypass(&f, m.as_deref()));
    if !caller_bypass {
        return Some(CALLER_NOT_BYPASS.to_owned());
    }
    None
}

fn provider_of<'a>(
    ctx: &'a Context,
    station: &StationId,
    instance: &ProviderInstanceId,
) -> Option<&'a fleetdeck_proto::link::StationProvider> {
    ctx.infos
        .get(station)?
        .providers
        .iter()
        .find(|p| &p.instance == instance)
}

/// The audit row of an accepted bypass start, in the caller's transaction.
pub(super) fn audit_bypass_start(
    tx: &Connection,
    grant: &Grant,
    station: &StationId,
    (family, mode): (&str, &str),
    (command_id, now): (&str, i64),
) -> Result<()> {
    let detail = format!(
        "grant={};caller={}/{};mode={family}:{mode}",
        grant.grant_id, grant.station_id, grant.session_id.0
    );
    tx.execute(
        "INSERT INTO audit (at, actor, action, target, command_id, result, detail)
         VALUES (?1, 'coordinator', 'coordinator.worker_bypass_start', ?2, ?3, 'ok', ?4)",
        params![now, station.as_str(), command_id, detail],
    )?;
    Ok(())
}

/// The lower of two modes of `family` (Claude: the earlier in the mode
/// order; Codex: the lower policy and the lower sandbox). `None` for an
/// unknown family or mode.
#[must_use]
pub fn meet(family: &str, a: &str, b: &str) -> Option<String> {
    let at = |list: &[&str], m: &str| list.iter().position(|x| *x == m);
    match family {
        "claude" => {
            let (x, y) = (at(CLAUDE_MODES, a)?, at(CLAUDE_MODES, b)?);
            Some(CLAUDE_MODES[x.min(y)].to_owned())
        }
        "codex" => {
            let parse = |s: &str| {
                let (p, b) = s.split_once('/')?;
                Some((at(CODEX_POLICIES, p)?, at(CODEX_SANDBOXES, b)?))
            };
            let ((ap, asb), (bp, bsb)) = (parse(a)?, parse(b)?);
            Some(format!(
                "{}/{}",
                CODEX_POLICIES[ap.min(bp)],
                CODEX_SANDBOXES[asb.min(bsb)]
            ))
        }
        _ => None,
    }
}

/// The concrete mode of a coordinator start that gives none (§3.5,
/// revision 2): the station default of the instance lowered to the grant
/// ceiling (`meet`); the provider default (`default`,
/// `untrusted/workspace-write`) when the station reports no default (an
/// older station). `None` only for an unknown family or mode (the start is
/// then refused with the ceiling text).
pub(super) fn resolved_mode(
    _grant: &Grant,
    ctx: &Context,
    (station, instance): (&StationId, &ProviderInstanceId),
    (family, ceiling): (&str, &str),
) -> Option<String> {
    let reported = provider_of(ctx, station, instance).and_then(|p| p.default_mode.as_deref());
    match reported {
        Some(default) => meet(family, default, ceiling),
        None => match family {
            "claude" => Some(CLAUDE_MODES[0].to_owned()),
            "codex" => Some(CODEX_DEFAULT.to_owned()),
            _ => None,
        },
    }
}

/// The dispatch rule of a launch mode (revision 2, "The dispatch check"):
/// `true` when `authority_check` must answer `mode_not_allowed`.
pub(super) fn launch_refusal(
    grant: &Grant,
    command: &SessionCommand,
    (family, launch_mode): (Option<&str>, Option<&str>),
    worker_bypass: bool,
) -> bool {
    let bypass = family.is_some_and(|f| is_bypass(f, launch_mode));
    if !bypass {
        return mode_refusal(grant, family, launch_mode).is_some();
    }
    let (Some(family), Some(mode)) = (family, launch_mode) else {
        return true;
    };
    // Rule 1: the stored start has exactly this mode.
    let SessionCommand::Start { spec, .. } = command else {
        return true;
    };
    if spec.mode.as_deref() != Some(mode) {
        return true;
    }
    // Rules 2-4.
    if grant.kind != GrantKind::Worker || !worker_bypass {
        return true;
    }
    let Some(ceiling) = grant.max_modes.get(family) else {
        return true;
    };
    !mode_within(family, Some(mode), ceiling)
}

#[cfg(test)]
#[path = "worker_bypass_tests.rs"]
mod tests;
