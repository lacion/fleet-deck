//! The dispatch of `EffectPayload::StopExecution` (BG-4, design
//! `docs/v2/design/board-gaps.md` §5.3 rules 10 to 12).
//!
//! The decision stored the `authorized` row and queued the effect. The
//! dispatch runs the stop inline (at most fifteen seconds) and returns
//! [`Dispatch::Confirmed`] after it settled the row:
//!
//! 1. It holds the lock of the session.
//! 2. It sets the row `attempted` (durable) before the first signal. A row
//!    that is not `authorized` any more is not dispatched.
//! 3. It takes the plan; a plan that is gone or expired settles the row
//!    `failed` with `plan_changed`.
//! 4. It checks the shim row and the boot again; a change settles the row
//!    `failed` with `plan_changed`.
//! 5. [`super::stop_exec::execute`] signals the targets.

use std::time::Instant;

use fleetdeck_proto::SessionId;
use fleetdeck_proto::execution_stop::code;
use fleetdeck_providers::host::shim::identity::real_boot_id_reader;

use super::SessionTarget;
use super::{Dispatch, Effect, EffectPayload};
use crate::manager::stop_attribution::check_row;
use crate::manager::stop_exec;
use crate::store::execution_stops::StopResult;

impl SessionTarget {
    /// The session locks and the stop plans of this station.
    pub(in crate::manager) fn stops(&self) -> &crate::manager::stop_state::StopState {
        &self.stops
    }

    /// Runs the stop of one claimed `StopExecution` effect.
    pub(super) async fn dispatch_stop(&self, effect: &Effect) -> Dispatch {
        let EffectPayload::StopExecution {
            command_id,
            plan_id,
        } = &effect.payload
        else {
            return Dispatch::NotDispatched("not_a_stop".into());
        };
        let _serial = self.stops.locks.hold(&effect.session_id).await;
        match self.store.execution_stop_attempt(command_id.clone()).await {
            Ok(true) => {}
            Ok(false) => return Dispatch::NotDispatched("plan_changed".into()),
            Err(e) => return Dispatch::NotDispatched(format!("store: {e}")),
        }
        let result = self
            .stop_result(&effect.session_id, command_id, plan_id)
            .await;
        match self
            .store
            .execution_stop_settle(command_id.clone(), result)
            .await
        {
            Ok(_) => Dispatch::Confirmed,
            Err(e) => {
                tracing::error!(error = %e, "the outcome of a stop is not stored");
                Dispatch::Unknown(format!("store: {e}"))
            }
        }
    }

    /// The result of a stop whose row is `attempted`.
    async fn stop_result(
        &self,
        session: &SessionId,
        command_id: &str,
        plan_id: &str,
    ) -> StopResult {
        let Some(plan) = self.stops.plans.take(session, plan_id, Instant::now()) else {
            tracing::info!(command_id, "the stop plan is gone or expired");
            return StopResult::failed(code::PLAN_CHANGED);
        };
        let row = match self.store.shim(plan.shim_id.clone()).await {
            Ok(Some(row)) if &row.session_id == session => row,
            Ok(_) => return StopResult::failed(code::PLAN_CHANGED),
            Err(e) => {
                tracing::warn!(error = %e, "the shim row of a stop cannot be read");
                return StopResult::failed(code::PLAN_CHANGED);
            }
        };
        let reader = self.boot_reader().unwrap_or_else(real_boot_id_reader);
        let boot = reader().ok();
        let incarnation = self.store.incarnation().as_str().to_owned();
        match check_row(&row, &incarnation, boot.as_deref()) {
            Ok(identity) if identity.pid == plan.leader => {}
            Ok(_) => return StopResult::failed(code::PLAN_CHANGED),
            Err(refused) => {
                tracing::info!(why = %refused.why, "the run changed since the stop plan");
                return StopResult::failed(code::PLAN_CHANGED);
            }
        }
        stop_exec::execute(&plan, stop_exec::real_send(), stop_exec::real_uids()).await
    }
}
