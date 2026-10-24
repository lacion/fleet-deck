//! The route of a decided command (moved out of `SessionManager::submit`
//! for the module size rule; the arms are the same). Each arm calls the
//! decision that needs the facts that `submit` read outside the writer
//! transaction.

use std::sync::Arc;

use fleetdeck_proto::command::{CommandOrigin, CommandResult, SessionCommand};
use fleetdeck_proto::execution::ExecutionCheck;

use super::stop_decide::StopFacts;
use super::usage_recount::RecountFacts;
use super::{Policy, decide, land, shell_decide, shims, stop_decide, trash, usage_recount};
use crate::coordinator::Bindings;
use crate::shell::Shells;
use crate::store::{DecideError, Decision};

/// What `submit` read before the transaction.
pub(super) struct RouteCtx {
    pub origin: CommandOrigin,
    pub policy: Arc<Policy>,
    pub coordinator: Bindings,
    pub shells: Option<Arc<Shells>>,
    pub restarting: bool,
    pub root: Option<String>,
    /// The process check of a reconciliation.
    pub check: Option<ExecutionCheck>,
    /// The facts of a stop.
    pub stop: Option<StopFacts>,
    /// The facts of a usage recount (B20).
    pub recount: Option<RecountFacts>,
}

/// Decides `command` in the command transaction.
///
/// # Errors
/// A refusal (stored as the receipt), or a database error.
pub(super) fn route(
    d: &mut Decision<'_>,
    ctx: RouteCtx,
    command: SessionCommand,
) -> Result<CommandResult, DecideError> {
    let RouteCtx {
        origin,
        policy,
        coordinator,
        shells,
        restarting,
        root,
        check,
        stop,
        recount,
    } = ctx;
    match command {
        SessionCommand::ReconcileExecution {
            session_id,
            shim_id,
        } => shims::decide_reconcile(d, origin, &session_id, &shim_id, check.as_ref()),
        SessionCommand::StopExecution {
            session_id,
            shim_id,
            plan_id,
        } => stop_decide::decide(d, origin, (&session_id, &shim_id, &plan_id), stop.as_ref()),
        SessionCommand::UsageRecount { session_id } => {
            usage_recount::decide(d, &session_id, recount)
        }
        command @ (SessionCommand::WorktreeTrash { .. }
        | SessionCommand::WorktreeRestore { .. }
        | SessionCommand::WorktreePurge { .. }
        | SessionCommand::WorktreeRelease { .. }) => {
            trash::decide(d, &policy, origin, shells.as_deref(), command)
        }
        command @ (SessionCommand::Land { .. } | SessionCommand::MoveOut { .. }) => {
            land::decide(d, &policy, &coordinator, origin, shells.as_deref(), command)
        }
        command @ (SessionCommand::ShellCommand { .. } | SessionCommand::ReleaseShell { .. }) => {
            shell_decide::decide(d, &policy, restarting, root.as_deref(), command)
        }
        command => {
            shell_decide::check_root(d, root.as_deref(), &command)?;
            decide::decide(d, &policy, &coordinator, origin, restarting, command)
        }
    }
}
