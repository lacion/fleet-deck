//! `session.execution_check` and `session.execution_stop_plan` (moved out of
//! `intake.rs` for the module size rule).

use fleetdeck_link::control::Control;
use fleetdeck_proto::link::{Outcome, WireError};

use super::{StationLink, bad_params, internal, ok};

/// `session.execution_check` (DL-182 §4.2): what still runs of the previous
/// execution of a session under a shim. Error code `unknown_session`. A link
/// that negotiated `execution_stop.v1` also gets `last_stop` (BG-4).
pub(super) async fn check(
    ctx: &StationLink,
    control: &Control,
    params: serde_json::Value,
) -> Outcome {
    let query: fleetdeck_proto::execution::ExecutionCheckQuery =
        match serde_json::from_value(params) {
            Ok(q) => q,
            Err(e) => return bad_params(e),
        };
    match ctx.manager.store().summary(query.session_id.clone()).await {
        Ok(Some(_)) => {}
        Ok(None) => return Outcome::Err(WireError::new("unknown_session", "no such session")),
        Err(e) => return internal(e),
    }
    let with_stop = control
        .params()
        .negotiated
        .has(fleetdeck_proto::version::feature::EXECUTION_STOP);
    let check = if with_stop {
        ctx.manager
            .execution_check_with_stop(&query.session_id)
            .await
    } else {
        ctx.manager.execution_check(&query.session_id).await
    };
    match check {
        Ok(check) => ok(check),
        Err(e) => internal(e),
    }
}

/// `session.execution_stop_plan` (BG-4): the stop plan of a lost run. Error
/// codes `unknown_session`, `invalid_params` (an unknown shim or the shim of
/// another session) and `unsupported`. A refusal is an answer with a code.
pub(super) async fn stop_plan(ctx: &StationLink, params: serde_json::Value) -> Outcome {
    use crate::manager::StopPlanError;
    let query = match serde_json::from_value(params) {
        Ok(q) => q,
        Err(e) => return bad_params(e),
    };
    match ctx.manager.execution_stop_plan(&query).await {
        Ok(plan) => ok(plan),
        Err(StopPlanError::UnknownSession) => {
            Outcome::Err(WireError::new("unknown_session", "no such session"))
        }
        Err(StopPlanError::UnknownShim) => {
            Outcome::Err(WireError::new("invalid_params", "unknown shim"))
        }
        Err(StopPlanError::Unsupported) => Outcome::Err(WireError::new(
            "unsupported",
            "this station cannot stop processes through pidfds",
        )),
        Err(StopPlanError::Store(e)) => internal(e),
    }
}
