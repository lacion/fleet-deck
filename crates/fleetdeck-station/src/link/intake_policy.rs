//! `coordinator.policy_now` of the station link (ADR-008 §6.2). Moved from
//! `intake.rs` for the module size rule (R13 P6 F0).

use fleetdeck_proto::link::Outcome;

use super::{bad_params, ok};
use crate::link::StationLink;

/// `coordinator.policy_now` (ADR-008 §6.2): the live fleet-control policy of
/// this station for the deck: the live setting `allow_fleet_control`, and
/// whether the caller session still holds a binding of a grant here.
pub(super) fn policy_now(ctx: &StationLink, params: serde_json::Value) -> Outcome {
    let query: fleetdeck_proto::mutation::PolicyNow = match serde_json::from_value(params) {
        Ok(q) => q,
        Err(e) => return bad_params(e),
    };
    ok(crate::manager::fleet_control::policy_now_answer(
        &ctx.manager.policy(),
        ctx.manager.coordinator_bindings(),
        &query,
    ))
}
