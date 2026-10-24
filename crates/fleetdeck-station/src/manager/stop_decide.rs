//! The decision of `session.stop_execution` (BG-4, design
//! `docs/v2/design/board-gaps.md` §5.2 and §5.3 rules 1, 2 and 9).
//!
//! The manager holds the lock of the session and reads the facts that need
//! the plan store and the boot id (`SessionManager::stop_facts`); the
//! decision runs in the command transaction. Refusals:
//!
//! - `forbidden`: the origin is not the board;
//! - `unknown_session`, `invalid_command` (an unknown shim or a shim of
//!   another session);
//! - `unsupported`: the platform has no pidfd;
//! - `invalid_state` with `identity_unproven: <text>`: the shim row is not
//!   the exact unresolved row of this incarnation and boot;
//! - `invalid_state` with `plan_changed: <text>`: the plan is gone,
//!   expired, replaced, or belongs to another session or shim.
//!
//! Accepted, the transaction holds the `authorized` row and the effect. The
//! receipt only says that the stop was admitted; the row holds the outcome.

use fleetdeck_proto::command::{CommandOrigin, CommandResult};
use fleetdeck_proto::execution_stop::code;
use fleetdeck_proto::{RejectCode, SessionId};

use super::decide::{reject, result};
use super::stop_attribution::check_row;
use crate::store::execution_stops::insert_authorized;
use crate::store::{DecideError, Decision, EffectPayload};

/// What the manager reads before the transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StopFacts {
    /// The stop plan of the command is alive for this session and shim.
    pub plan_valid: bool,
    /// The current boot id; `None` when it cannot be read.
    pub boot: Option<String>,
    /// The database incarnation of the station.
    pub incarnation: String,
}

fn refuse(text_code: &str) -> DecideError {
    let text = code::text(text_code).unwrap_or("");
    reject(RejectCode::InvalidState, format!("{text_code}: {text}"))
}

/// A direct `decide` call has no plan store: the stop is refused.
#[must_use]
pub fn refuse_direct() -> DecideError {
    refuse(code::PLAN_CHANGED)
}

/// Decides `session.stop_execution`.
///
/// # Errors
/// A stored rejection, or a database error.
pub fn decide(
    d: &mut Decision<'_>,
    origin: CommandOrigin,
    (session_id, shim_id, plan_id): (&SessionId, &str, &str),
    facts: Option<&StopFacts>,
) -> Result<CommandResult, DecideError> {
    if origin != CommandOrigin::Board {
        return Err(reject(
            RejectCode::Forbidden,
            "only the board can stop the processes of a previous run",
        ));
    }
    if d.summary(session_id)?.is_none() {
        return Err(reject(RejectCode::UnknownSession, "no such session"));
    }
    let row = d
        .shim_row(shim_id)?
        .filter(|row| &row.session_id == session_id)
        .ok_or_else(|| reject(RejectCode::InvalidCommand, "unknown shim"))?;
    if !cfg!(target_os = "linux") {
        return Err(reject(
            RejectCode::Unsupported,
            "this platform cannot stop processes through pidfds",
        ));
    }
    let Some(facts) = facts else {
        return Err(refuse_direct());
    };
    if let Err(refused) = check_row(&row, &facts.incarnation, facts.boot.as_deref()) {
        tracing::info!(why = %refused.why, "a stop is refused");
        return Err(refuse(code::IDENTITY_UNPROVEN));
    }
    if !facts.plan_valid {
        return Err(refuse(code::PLAN_CHANGED));
    }
    let command_id = d.command_id().as_str().to_owned();
    insert_authorized(
        d.connection(),
        &command_id,
        session_id,
        shim_id,
        plan_id,
        d.now(),
    )?;
    d.enqueue(
        session_id,
        EffectPayload::StopExecution {
            command_id,
            plan_id: plan_id.to_owned(),
        },
    )?;
    Ok(result(session_id, None))
}
