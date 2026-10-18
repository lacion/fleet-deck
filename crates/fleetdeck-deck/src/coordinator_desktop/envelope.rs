//! Step 1 of the deck decision (R13 P6 §6.2, decision O12-9 items 3, 9, 21):
//! the strict parse of the envelope, the frame command ID, the incarnation and
//! the deadline. A frame command ID that differs from the envelope's, and any
//! parse failure, are wire errors (the caller built the envelope wrongly);
//! every other refusal is a [`DesktopReply`](fleetdeck_proto::coordinator_desktop_wire::DesktopReply).

use fleetdeck_link::control::InboundRequest;
use fleetdeck_proto::IncarnationId;
use fleetdeck_proto::coordinator_desktop::{DesktopCall, DesktopQuery};
use fleetdeck_proto::coordinator_desktop_text::{code, deadline_ms};
use fleetdeck_proto::coordinator_desktop_wire::{
    CoordinatorDesktopCallEnvelope, CoordinatorDesktopQueryEnvelope,
};
use fleetdeck_proto::desktop_authority::CLOCK_SKEW_MS;

/// How long a `coordinator.desktop_call` may still be accepted (§5.3).
pub(super) const CALL_RETRY_MS: i64 = 10_000;

/// Why step 1 stops.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Stop {
    /// The wire error `invalid_params`.
    Wire,
    /// A reply with this code.
    Reply(&'static str),
}

/// Parses a call envelope. The frame command ID must equal the envelope's.
pub(super) fn parse_call(req: &InboundRequest) -> Result<CoordinatorDesktopCallEnvelope, Stop> {
    let env = serde_json::from_value::<CoordinatorDesktopCallEnvelope>(req.params.clone())
        .map_err(|_| Stop::Wire)?;
    if req.command_id.as_ref() != Some(&env.command_id) {
        return Err(Stop::Wire);
    }
    Ok(env)
}

/// Parses a query envelope.
pub(super) fn parse_query(req: &InboundRequest) -> Result<CoordinatorDesktopQueryEnvelope, Stop> {
    serde_json::from_value::<CoordinatorDesktopQueryEnvelope>(req.params.clone())
        .map_err(|_| Stop::Wire)
}

/// The checks of a call envelope after the parse: the incarnation, the
/// deadline `retry_until` (not past, and at most 10 s plus the clock skew
/// ahead), and the argument rules.
pub(super) fn check_call(
    env: &CoordinatorDesktopCallEnvelope,
    now: i64,
    incarnation: &IncarnationId,
) -> Result<(), Stop> {
    if env.target_incarnation != *incarnation {
        return Err(Stop::Reply(code::INCARNATION_MISMATCH));
    }
    if now > env.retry_until {
        return Err(Stop::Reply(code::EXPIRED));
    }
    if env.retry_until > now.saturating_add(CALL_RETRY_MS + CLOCK_SKEW_MS) {
        return Err(Stop::Reply(code::INVALID_PARAMS));
    }
    check_call_args(&env.call)
}

fn check_call_args(call: &DesktopCall) -> Result<(), Stop> {
    call.validate()
        .map_err(|_| Stop::Reply(code::INVALID_PARAMS))
}

/// The deck deadline of a query tool, in milliseconds (§9).
fn deck_ms(query: &DesktopQuery) -> u64 {
    match query {
        DesktopQuery::DesktopInfo(_) => deadline_ms::INFO_DECK,
        DesktopQuery::DesktopActionStatus(_) => deadline_ms::STATUS_DECK,
        DesktopQuery::CaptureStatus(_) => deadline_ms::CAPTURE_LOOKUP_DECK,
        DesktopQuery::ReadCaptureImage(_) => deadline_ms::IMAGE_DECK,
    }
}

/// The checks of a query envelope after the parse: the incarnation, the
/// deadline (not past, and at most the deck deadline of the tool plus the
/// clock skew ahead), and the argument rules.
pub(super) fn check_query(
    env: &CoordinatorDesktopQueryEnvelope,
    now: i64,
    incarnation: &IncarnationId,
) -> Result<(), Stop> {
    if env.target_incarnation != *incarnation {
        return Err(Stop::Reply(code::INCARNATION_MISMATCH));
    }
    if env.deadline < now {
        return Err(Stop::Reply(code::EXPIRED));
    }
    let ahead = i64::try_from(deck_ms(&env.query))
        .unwrap_or(i64::MAX)
        .saturating_add(CLOCK_SKEW_MS);
    if env.deadline > now.saturating_add(ahead) {
        return Err(Stop::Reply(code::INVALID_PARAMS));
    }
    env.query
        .validate()
        .map_err(|_| Stop::Reply(code::INVALID_PARAMS))
}

#[cfg(test)]
#[path = "envelope_tests.rs"]
mod tests;
