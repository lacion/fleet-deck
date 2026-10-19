//! The gate of a fleet-control call (ADR-008 §3, §4.1), moved out of
//! `coordinator.rs` for the module size rule. Behavior is unchanged.

use fleetdeck_proto::RejectCode;
use fleetdeck_proto::StationId;
use fleetdeck_proto::mutation::CoordinatorOp;

use super::super::LinkV2;
use super::super::coordinator_ops::{Context, Decision};
use super::super::grants::Grant;

/// True when the call is a fleet-control op (ADR-008 §2): an op that
/// ADR-008 adds, or any op of a fleet-control grant.
fn is_fleet_op(grant: &Grant, op: &CoordinatorOp) -> bool {
    grant.is_fleet_control()
        || matches!(
            op,
            CoordinatorOp::Resume { .. }
                | CoordinatorOp::Rename { .. }
                | CoordinatorOp::SetMode { .. }
                | CoordinatorOp::ProjectAssign { .. }
                | CoordinatorOp::ProjectStart { .. }
                | CoordinatorOp::Answer { .. }
                | CoordinatorOp::Land { .. }
                | CoordinatorOp::Move { .. }
        )
}

/// The station that an op names, for the `coordinator.v2` gate.
fn op_station(op: &CoordinatorOp) -> Option<&StationId> {
    match op {
        CoordinatorOp::Send { to, .. } | CoordinatorOp::Assign { to, .. } => Some(&to.station),
        CoordinatorOp::Interrupt { station, .. }
        | CoordinatorOp::Stop { station, .. }
        | CoordinatorOp::Spawn { station, .. }
        | CoordinatorOp::Resume { station, .. }
        | CoordinatorOp::Rename { station, .. }
        | CoordinatorOp::SetMode { station, .. }
        | CoordinatorOp::ProjectAssign { station, .. }
        | CoordinatorOp::Answer { station, .. }
        | CoordinatorOp::Land { station, .. } => Some(station),
        _ => None,
    }
}

/// The gate of a fleet-control op (ADR-008 §3, §4.1): the deck setting
/// `fleet_control` is on (`fleet_control_off:`, stored `forbidden`), and the
/// caller's station and the target station did not negotiate less than
/// `coordinator.v2` on a live link (stored `unsupported`). A station with no
/// live link passes here: the send waits for it and checks again.
pub(in crate::store) fn fleet_gate(
    grant: &Grant,
    op: &CoordinatorOp,
    ctx: &Context,
) -> Option<Decision> {
    if !is_fleet_op(grant, op) {
        return None;
    }
    if !ctx.env.fleet_control {
        return Some(Decision::Rejected(
            RejectCode::Forbidden,
            "fleet_control_off: fleet control is off on this deck".to_owned(),
        ));
    }
    if grant.authority().is_none() {
        return Some(Decision::Rejected(
            RejectCode::Forbidden,
            "the grant is not bound".to_owned(),
        ));
    }
    let stations = std::iter::once(&grant.station_id).chain(op_station(op));
    stations.into_iter().find_map(|s| v2_refusal(grant, ctx, s))
}

/// The refusal for a station whose live link did not negotiate
/// `coordinator.v2`, for a call under a fleet-control grant (ADR-008 §3).
pub(in crate::store) fn v2_refusal(
    grant: &Grant,
    ctx: &Context,
    station: &StationId,
) -> Option<Decision> {
    (grant.is_fleet_control() && ctx.env.link(station) == LinkV2::Without).then(|| {
        Decision::Rejected(
            RejectCode::Unsupported,
            format!("the link of station `{station}` did not negotiate coordinator.v2"),
        )
    })
}
