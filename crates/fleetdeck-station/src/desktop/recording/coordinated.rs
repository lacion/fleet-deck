//! The hooks of a coordinator recording (R13 P5b-2 §13.3, decision O12-22;
//! P6 §4.6): the fixed disclosure before the recorder, the authority check
//! before the import, and the provenance of the caller. An owner recording
//! runs none of them.

use fleetdeck_core::ChildEnv;
use fleetdeck_proto::capture::CaptureCaller;
use fleetdeck_proto::coordinator_desktop_wire::{AuthorityPhase, CoordinatorContext};
use fleetdeck_proto::desktop::Urgency;

use super::op::Run;
use crate::desktop::Fail;
use crate::desktop::actions::send_notify;
use crate::desktop::authority::Start;
use crate::desktop::run::Runner;

/// The title of the fixed disclosure.
const TITLE: &str = "Fleet Deck";

/// The text of the disclosure. The monitor is one of the live monitor list
/// (the admission found it), the label comes from the checked context and
/// the seconds are the validated integer.
pub(super) fn disclosure(monitor: &str, seconds: u32, ctx: &CoordinatorContext) -> String {
    format!(
        "Fleet Deck is recording {monitor} for {} for up to {seconds} seconds.",
        ctx.caller_label
    )
}

impl Run<'_> {
    /// Before the recorder and before the start check: a coordinator
    /// recording sends the fixed disclosure with the notify path (fixed argv,
    /// urgency `normal`, no action). A failure of the notification is the
    /// failure of the recording, and no recorder starts. An owner recording
    /// sends nothing.
    ///
    /// # Errors
    /// The failure of the notify path, unchanged.
    pub(super) async fn disclose(&self) -> Result<(), Fail> {
        let req = self.req;
        let Some(ctx) = req.coordinator.as_ref() else {
            return Ok(());
        };
        let allowed = std::sync::Arc::clone(&req.allowed);
        let gate = move || allowed().desktop;
        let runner = Runner {
            env: &self.ctx.env,
            gate: Some(&gate),
            start: None,
        };
        let body = disclosure(&req.monitor, req.seconds, ctx);
        send_notify(&runner, TITLE, &body, Urgency::Normal).await
    }
}

/// Before the import: a coordinator recording checks the authority again, in
/// the `import` phase, in addition to the import lease. `Ok` for an owner
/// recording.
///
/// # Errors
/// `authority_lost`, `authority_unknown` or a failure of a live gate.
pub(super) async fn before_import(
    coordinator: Option<&CoordinatorContext>,
    start: &Start,
    env: &ChildEnv,
) -> Result<(), Fail> {
    if coordinator.is_none() {
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
