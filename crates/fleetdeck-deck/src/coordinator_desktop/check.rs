//! `desktop.authority_check` (R13 P6 §5.6, §5.7, decision O12-9 item 12, Q2,
//! Q3): the target station asks right before the effect of an action, before
//! the import of a screenshot or a recording, or before it returns the data of a protected
//! read, whether the authority still holds. The deck answers from its own
//! state and from the live policy of the caller station, in the order of
//! ADR-008 section 6.2 (local, policy, local), and fails closed.
//!
//! Every answer is `valid: true`, or `valid: false` with `authority_lost` or
//! `authority_unknown`. A malformed request, a store error and a timeout are
//! `authority_unknown`.

use std::time::{Duration, Instant};

use fleetdeck_link::control::{Control, InboundRequest};
use fleetdeck_link::methods::code as link_code;
use fleetdeck_proto::StationId;
use fleetdeck_proto::coordinator_desktop_text::{code, deadline_ms};
use fleetdeck_proto::coordinator_desktop_wire::{
    AuthorityPhase, AuthorityReason, DesktopAuthorityAnswer, DesktopAuthorityCheck, ProtectedRead,
};
use fleetdeck_proto::link::{Outcome, WireError};
use fleetdeck_proto::mutation::CoordinatorBinding;

use super::authorize::{Held, Tool, local_pass};
use super::permits::Arrival;
use super::policy::caller_policy;
use crate::link::DeckLink;
use crate::store::coordinator_desktop::{OpState, OpTool};

type Verdict = Result<(), AuthorityReason>;

fn answer(verdict: Verdict) -> Outcome {
    let body = match verdict {
        Ok(()) => DesktopAuthorityAnswer {
            valid: true,
            reason: None,
        },
        Err(reason) => DesktopAuthorityAnswer {
            valid: false,
            reason: Some(reason),
        },
    };
    match serde_json::to_value(body) {
        Ok(v) => Outcome::Ok(v),
        Err(e) => Outcome::Err(WireError::new(link_code::PROTOCOL_ERROR, e.to_string())),
    }
}

/// The reason of a refusal code of a local pass or a policy round trip:
/// `authority_unknown` stays unknown; everything else is lost.
fn reason(refusal: &str) -> AuthorityReason {
    if refusal == code::AUTHORITY_UNKNOWN {
        AuthorityReason::AuthorityUnknown
    } else {
        AuthorityReason::AuthorityLost
    }
}

/// `desktop.authority_check`.
pub(crate) async fn authority_check(
    ctx: &DeckLink,
    control: &Control,
    station: StationId,
    req: &InboundRequest,
) -> Outcome {
    let deadline = Instant::now() + Duration::from_millis(deadline_ms::AUTHORITY_CHECK);
    let Ok(check) = serde_json::from_value::<DesktopAuthorityCheck>(req.params.clone()) else {
        return answer(Err(AuthorityReason::AuthorityUnknown));
    };
    let run = decide(ctx, control, &station, check, deadline);
    match tokio::time::timeout(deadline.saturating_duration_since(Instant::now()), run).await {
        Ok(verdict) => answer(verdict),
        Err(_) => answer(Err(AuthorityReason::AuthorityUnknown)),
    }
}

async fn decide(
    ctx: &DeckLink,
    control: &Control,
    station: &StationId,
    check: DesktopAuthorityCheck,
    deadline: Instant,
) -> Verdict {
    match (check.phase, check.read.is_some()) {
        (AuthorityPhase::Read, true) => read(ctx, control, station, check, deadline).await,
        (AuthorityPhase::Execute | AuthorityPhase::Import, false) => {
            action(ctx, station, &check, deadline).await
        }
        // A `read` field with another phase, or a missing one with phase
        // `read`, is malformed.
        _ => Err(AuthorityReason::AuthorityUnknown),
    }
}

/// The local pass, the caller policy for the current binding of the grant,
/// and the local pass again.
async fn pass(
    ctx: &DeckLink,
    held: &Held<'_>,
    (caller_session, deadline): (&fleetdeck_proto::SessionId, Instant),
) -> Verdict {
    let first = local_pass(ctx, held).await.map_err(reason)?;
    let binding: CoordinatorBinding = first.binding.ok_or(AuthorityReason::AuthorityLost)?;
    caller_policy(ctx, held.caller, caller_session, &binding, deadline)
        .await
        .map_err(reason)?;
    // The caller policy can wait up to 3 s: the deck checks again.
    local_pass(ctx, held).await.map_err(reason)?;
    Ok(())
}

/// Phases `execute` and `import`: the operation that the deck sent to this
/// station with this hash, in state `attempted`.
async fn action(
    ctx: &DeckLink,
    station: &StationId,
    check: &DesktopAuthorityCheck,
    deadline: Instant,
) -> Verdict {
    let op = match ctx.store.desktop_op(&check.operation_id).await {
        Ok(Some(op)) => op,
        Ok(None) => return Err(AuthorityReason::AuthorityLost),
        Err(e) => {
            tracing::warn!(error = %e, "desktop authority check");
            return Err(AuthorityReason::AuthorityUnknown);
        }
    };
    if op.target_station != *station
        || op.action_hash.as_deref() != Some(check.action_hash.as_str())
        || op.state != OpState::Attempted
        // Only a capture imports: a screenshot, or a recording (R13 P5b-2
        // §7.6: the station asks for import authority before its import). An
        // input operation never imports (R13 P7 §6.4); it has the `execute`
        // phase only, with the same rule as every other mutating tool.
        || (check.phase == AuthorityPhase::Import
            && !matches!(op.tool, OpTool::Screenshot | OpTool::RecordMonitor))
    {
        return Err(AuthorityReason::AuthorityLost);
    }
    let held = Held {
        grant_id: &op.grant_id,
        binding_id: &op.binding_id,
        generation: op.generation,
        caller: &op.caller_station,
        target: &op.target_station,
        class: op.class,
        tool: Some(Tool::of_op(op.tool)),
    };
    pass(ctx, &held, (&op.caller_session, deadline)).await
}

/// Phase `read`: the five checks of §5.7 against the permit, then the same
/// passes. The permit leaves the registry at the first check, so every answer
/// consumes it.
async fn read(
    ctx: &DeckLink,
    control: &Control,
    station: &StationId,
    check: DesktopAuthorityCheck,
    deadline: Instant,
) -> Verdict {
    let Some(permit) = ctx.coordinator_desktop.permits().take(&check.operation_id) else {
        return Err(AuthorityReason::AuthorityLost);
    };
    let params = control.params();
    let node = params.peer.to_string();
    let Some(read) = check.read.as_ref() else {
        return Err(AuthorityReason::AuthorityUnknown);
    };
    let arrival = Arrival {
        station,
        station_node: &node,
        epoch: params.peer_desktop_epoch.as_deref(),
        read,
        hash: &check.action_hash,
    };
    if !permit.matches(&arrival, &check.operation_id, Instant::now()) {
        return Err(AuthorityReason::AuthorityLost);
    }
    let held = Held {
        grant_id: &permit.ctx.grant_id,
        binding_id: &permit.ctx.binding_id,
        generation: permit.ctx.generation,
        caller: &permit.ctx.caller_station,
        target: &permit.target,
        class: permit.class,
        // R13 P7b §6.1: the permit of `input_windows` has the input class,
        // and its read phase runs the rules of `window_list`.
        tool: matches!(permit.read, ProtectedRead::InputWindows).then_some(Tool::WindowList),
    };
    pass(ctx, &held, (&permit.ctx.caller_session, deadline)).await
}
