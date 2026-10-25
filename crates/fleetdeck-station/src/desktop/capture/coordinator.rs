//! The hooks of a coordinator screenshot (R13 P6 §4.6, §4.7, §6.3): the
//! fixed disclosure before `grim`, the authority check before the import and
//! the provenance of the caller. The owner screenshot runs none of them.

use fleetdeck_core::ChildEnv;
use fleetdeck_proto::capture::CaptureCaller;
use fleetdeck_proto::coordinator_desktop_wire::{AuthorityPhase, CoordinatorContext};
use fleetdeck_proto::desktop::Urgency;

use super::Allowed;
use super::op::{OFF, check_gates};
use crate::desktop::actions::send_notify;
use crate::desktop::authority::Start;
use crate::desktop::run::Runner;
use crate::desktop::{Fail, Gate, code};

/// The title of the fixed disclosure.
const TITLE: &str = "Fleet Deck";

/// The live capture gates: `allow_desktop` and `allow_desktop_capture`, and
/// the `capture_monitors` rule when `monitor` is given. The codes and texts
/// are those of the owner screenshot.
///
/// # Errors
/// `forbidden`.
pub(crate) fn capture_gate(allowed: &Allowed, monitor: Option<&str>) -> Result<(), Fail> {
    match monitor {
        Some(monitor) => check_gates(allowed, monitor),
        None if allowed.desktop && allowed.capture => Ok(()),
        None => Err(Fail::new(code::FORBIDDEN, OFF)),
    }
}

/// The text of the disclosure: both values come from validated records, the
/// monitor of the live monitor list and the label of the checked context.
pub(super) fn disclosure(monitor: &str, ctx: &CoordinatorContext) -> String {
    format!(
        "Fleet Deck is taking a screenshot of {monitor} for {}.",
        ctx.caller_label
    )
}

/// Before `grim`: a coordinator capture sends the fixed disclosure with the
/// notify path (fixed argv, urgency `normal`, no action). It counts toward no
/// rate. A failure of the notification is the failure of the capture, and no
/// capture starts. The start check of the authority follows the disclosure.
///
/// # Errors
/// The failure of the notify path, unchanged.
pub(super) async fn before_grim(
    start: &Start,
    env: &ChildEnv,
    gate: Gate<'_>,
    monitor: &str,
) -> Result<(), Fail> {
    let Some(ctx) = start.coordinator() else {
        return Ok(());
    };
    let runner = Runner {
        env,
        gate: Some(gate),
        start: None,
    };
    send_notify(&runner, TITLE, &disclosure(monitor, ctx), Urgency::Normal).await
}

/// Before the import: a coordinator capture checks the authority again, in
/// the `import` phase. A refusal discards the staged output.
///
/// # Errors
/// `authority_lost`, `authority_unknown` or a failure of a live gate.
pub(super) async fn before_import(start: &Start, env: &ChildEnv) -> Result<(), Fail> {
    if start.coordinator().is_none() {
        return Ok(());
    }
    start.check_phase(AuthorityPhase::Import, env).await
}

/// The caller fields of the provenance, from the authenticated context: the
/// owner has none; a coordinator has its station, session and grant.
pub(super) fn caller(
    ctx: Option<&CoordinatorContext>,
) -> (
    CaptureCaller,
    Option<String>,
    Option<String>,
    Option<String>,
) {
    match ctx {
        None => (CaptureCaller::Owner, None, None, None),
        Some(c) => (
            CaptureCaller::Coordinator,
            Some(c.caller_station.to_string()),
            Some(c.caller_session.to_string()),
            Some(c.grant_id.clone()),
        ),
    }
}
