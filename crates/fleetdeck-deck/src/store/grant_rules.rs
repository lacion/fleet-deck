//! The creation rules of a grant (B27 §3.2): the limits of `[grant_limits]`,
//! the defaults that are resolved before the checks, and the rules of a
//! worker grant. Every creator of a grant passes [`prepare_grant`].

use std::collections::BTreeSet;

use fleetdeck_core::config_grant_limits::{GrantLimits, Limit, default_within, within};
use fleetdeck_proto::command::{GrantKind, GrantScope};
use fleetdeck_proto::coordinator_desktop_text::limits as desktop_limits;
use fleetdeck_proto::coordinator_spec::InputScope;
use fleetdeck_proto::grant_limit::NO_LIMIT;

use crate::store::coordinator_ops::{is_bypass, valid_ceiling};
use crate::store::grants::{
    DEFAULT_ANSWER_CAP, DEFAULT_SPAWN_CAP, GrantClass, MAX_TTL_MS, NewGrant,
};
use crate::store::grants_input::check_input_grant;
use crate::store::worker_grant::{check_desktop_caps, check_recording_caps};
use crate::store::{DeckError, Result};

/// A spawn cap of an owner grant is above `grant_limits.spawn_cap`.
pub(crate) const OVER_SPAWN_LIMIT: &str =
    "spawn_cap is above the grant limit of the deck (Settings, Grant limits)";
/// A spawn cap of a worker grant is above `grant_limits.worker_spawn_cap`.
pub(crate) const OVER_WORKER_SPAWN_LIMIT: &str =
    "a worker grant's spawn cap is above the grant limit of the deck (Settings, Grant limits)";
/// An answer cap is above `grant_limits.answer_cap`.
pub(crate) const OVER_ANSWER_LIMIT: &str =
    "answer_cap is above the grant limit of the deck (Settings, Grant limits)";
/// An idle time is above `grant_limits.idle_hours`.
pub(crate) const OVER_IDLE_LIMIT: &str =
    "the idle time is above the grant limit of the deck (Settings, Grant limits)";
/// `NO_LIMIT` while the matching limit is a number.
pub(crate) const NO_LIMIT_REFUSED: &str =
    "no limit needs the grant limit of the deck set to no limit (Settings, Grant limits)";
/// A lifetime, an idle time or the stations of a grant are out of range.
pub(crate) const BAD_LIFETIME: &str =
    "a grant needs a lifetime of at most 7 days and at least one station";

const HOUR_MS: i64 = 60 * 60 * 1000;

/// The spawn ceiling of a grant of `kind`: `limits.spawn_cap` for an owner
/// (coordinator) grant, `limits.worker_spawn_cap` for a worker grant. The two
/// limits are independent: a worker grant is never checked against
/// `limits.spawn_cap`.
pub(crate) fn spawn_limit(kind: GrantKind, limits: &GrantLimits) -> Limit {
    match kind {
        GrantKind::Coordinator => limits.spawn_cap,
        GrantKind::Worker => limits.worker_spawn_cap,
    }
}

/// `cap` against `limit`: `NO_LIMIT` needs no limit; a number must be within.
fn check_cap(cap: u32, limit: Limit, over: &'static str) -> std::result::Result<(), &'static str> {
    if cap == NO_LIMIT {
        return if limit.is_none() {
            Ok(())
        } else {
            Err(NO_LIMIT_REFUSED)
        };
    }
    if within(cap, limit) {
        Ok(())
    } else {
        Err(over)
    }
}

/// Checks a spawn cap of a grant of `kind` against the spawn limit of that
/// kind.
///
/// # Errors
/// The fixed text of the limit that the cap breaks.
pub(crate) fn check_spawn_cap(
    cap: u32,
    kind: GrantKind,
    limits: &GrantLimits,
) -> std::result::Result<(), &'static str> {
    let over = match kind {
        GrantKind::Coordinator => OVER_SPAWN_LIMIT,
        GrantKind::Worker => OVER_WORKER_SPAWN_LIMIT,
    };
    check_cap(cap, spawn_limit(kind, limits), over)
}

/// Checks an answer cap against `limits.answer_cap`.
///
/// # Errors
/// The fixed text of the limit that the cap breaks.
pub(crate) fn check_answer_cap(
    cap: u32,
    limits: &GrantLimits,
) -> std::result::Result<(), &'static str> {
    check_cap(cap, limits.answer_cap, OVER_ANSWER_LIMIT)
}

/// The omitted spawn cap of an owner grant: the default lowered to the limit.
#[must_use]
pub(crate) fn default_spawn_cap(limits: &GrantLimits) -> u32 {
    default_within(DEFAULT_SPAWN_CAP, limits.spawn_cap)
}

/// The omitted answer cap: the default lowered to the limit.
#[must_use]
pub(crate) fn default_answer_cap(limits: &GrantLimits) -> u32 {
    default_within(DEFAULT_ANSWER_CAP, limits.answer_cap)
}

/// The idle time that a grant stores (§3.3 item 3): `NO_LIMIT` (only with no
/// idle limit) and a number above the lifetime both give the lifetime.
fn resolve_idle(idle_ms: i64, ttl_ms: i64, limits: &GrantLimits) -> Result<i64> {
    let refuse = |m: &str| Err(DeckError::Invalid(m.to_owned()));
    if idle_ms == i64::from(NO_LIMIT) {
        return if limits.idle_hours.is_some() {
            refuse(NO_LIMIT_REFUSED)
        } else {
            Ok(ttl_ms)
        };
    }
    if limits
        .idle_hours
        .is_some_and(|h| idle_ms > i64::from(h) * HOUR_MS)
    {
        return refuse(OVER_IDLE_LIMIT);
    }
    Ok(idle_ms.min(ttl_ms))
}

/// The rules and the defaults of a new grant, the same for every creator.
/// `allow_bypass` is `worker_grant.allow_bypass`: a worker grant may have a
/// bypass mode ceiling only then (§3.4).
///
/// # Errors
/// [`DeckError::Invalid`] for a value out of range or above a limit.
pub(crate) fn prepare_grant(
    mut grant: NewGrant,
    limits: &GrantLimits,
    allow_bypass: bool,
) -> Result<NewGrant> {
    let invalid = |m: &str| DeckError::Invalid(m.to_owned());
    if grant.idle_ttl_ms < 1
        || !(1..=MAX_TTL_MS).contains(&grant.ttl_ms)
        || grant.stations.is_empty()
    {
        return Err(invalid(BAD_LIFETIME));
    }
    // The spawn cap of a grant that has no spawn class is never used.
    if grant.classes.contains(&GrantClass::Spawn) {
        check_spawn_cap(grant.spawn_cap, grant.kind, limits).map_err(invalid)?;
    }
    grant.idle_ttl_ms = resolve_idle(grant.idle_ttl_ms, grant.ttl_ms, limits)?;
    // The answer cap (ADR-008 §3): only with the `answer` class. An omitted
    // cap is resolved before the check (revision 2).
    let answers = grant.classes.contains(&GrantClass::Answer);
    if answers && grant.answer_cap.is_none() {
        grant.answer_cap = Some(default_answer_cap(limits));
    }
    match grant.answer_cap {
        Some(_) if !answers => return Err(invalid("answer_cap needs the answer class")),
        Some(cap) => check_answer_cap(cap, limits).map_err(invalid)?,
        None => {}
    }
    // The desktop caps (R13 P6 §8.1).
    check_desktop_caps(
        &grant.classes,
        grant.desktop_cap,
        grant.capture_cap,
        grant.image_byte_cap,
    )
    .map_err(|m| DeckError::Invalid(m.to_owned()))?;
    check_recording_caps(
        &grant.classes,
        grant.recording_cap,
        grant.recording_second_cap,
    )
    .map_err(|m| DeckError::Invalid(m.to_owned()))?;
    let input = (grant.input_cap, grant.input_character_cap);
    check_input_grant(
        &grant.classes,
        (grant.allow_input, grant.allow_click),
        grant.input_scope,
        input,
    )
    .map_err(|m| DeckError::Invalid(m.to_owned()))?;
    if grant.allow_input {
        grant.input_scope.get_or_insert(InputScope::OwnWindows);
    }
    if grant.classes.contains(&GrantClass::Desktop) || grant.classes.contains(&GrantClass::Capture)
    {
        grant
            .desktop_cap
            .get_or_insert(desktop_limits::DESKTOP_CAP_DEFAULT);
    }
    if grant.classes.contains(&GrantClass::Capture) {
        grant
            .capture_cap
            .get_or_insert(desktop_limits::CAPTURE_CAP_DEFAULT);
        grant
            .image_byte_cap
            .get_or_insert(desktop_limits::IMAGE_BYTE_CAP_DEFAULT);
    }
    if grant.kind == GrantKind::Worker {
        check_worker_grant(&grant, allow_bypass)?;
    }
    Ok(grant)
}

/// A mode ceiling of a worker grant: a valid ceiling, or a bypass mode when
/// `allow_bypass` is on (§3.4).
fn valid_worker_ceiling(family: &str, mode: &str, allow_bypass: bool) -> bool {
    matches!(family, "claude" | "codex")
        && (valid_ceiling(family, mode) || (allow_bypass && is_bypass(family, Some(mode))))
}

/// A worker grant has exactly the classes read, mail, drive and spawn, the
/// lineage scope, valid ceilings and no other allowance (§4). Its spawn cap
/// is checked in [`prepare_grant`] with [`spawn_limit`].
fn check_worker_grant(grant: &NewGrant, allow_bypass: bool) -> Result<()> {
    let classes = BTreeSet::from([
        GrantClass::Read,
        GrantClass::Mail,
        GrantClass::Drive,
        GrantClass::Spawn,
    ]);
    let bad = |m: &str| Err(DeckError::Invalid(m.to_owned()));
    if grant.classes != classes || grant.scope != GrantScope::Lineage {
        return bad(
            "a worker grant has the classes read, mail, drive and spawn and the lineage scope",
        );
    }
    let ceilings_ok = grant
        .max_modes
        .iter()
        .all(|(family, mode)| valid_worker_ceiling(family, mode, allow_bypass));
    if !ceilings_ok {
        return bad("a worker grant needs valid mode ceilings");
    }
    if grant.answer_cap.is_some()
        || grant.desktop_cap.is_some()
        || grant.allow_input
        || grant.allow_click
        || grant.recording_cap != 0
    {
        return bad("a worker grant has no answer, desktop or input allowance");
    }
    Ok(())
}

#[cfg(test)]
#[path = "grant_limits_tests.rs"]
mod tests;
