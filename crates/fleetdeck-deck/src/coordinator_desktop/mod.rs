//! The deck handlers of the P6 coordinator desktop methods (feature
//! `desktop_mcp.v1`; `docs/v2/design/omarchy-p6.md` §5, §6; decisions
//! O12-4, O12-9).
//!
//! - [`desktop_call`] serves `notify`, `preview_open`, `window_close`,
//!   `screenshot` and `record_monitor` ([`calls`], [`capture`], [`record`]); [`desktop_query`] serves
//!   `desktop_info` and `desktop_action_status` ([`queries`]),
//!   `capture_status` ([`capture_status`]) and `read_capture_image`
//!   ([`image`]); [`authority_check`] answers the dispatch authority check of
//!   the target station ([`check`]).
//! - [`envelope`] is step 1, [`authorize`] steps 2 to 6, [`policy`] the live
//!   policy round trips, [`relay`] the one send of a mutation, [`permits`] and
//!   [`reads`] the protected read, [`audit`] the audit rows and [`limits`] the
//!   in-flight slots.
//! - `wall_show` stores a coordinator directive ([`wall`]); [`directive`]
//!   holds the live delivery checks of that directive for the displays.
//!
//! Every handler answers `Outcome::Ok(DesktopReply)`. A malformed envelope, a
//! frame command ID that differs from the envelope's, and an unknown method
//! form are the only wire errors (`invalid_params`).

mod audit;
mod authorize;
mod calls;
mod capture;
mod capture_status;
mod check;
mod directive;
mod envelope;
mod image;
mod limits;
mod permits;
mod policy;
mod queries;
mod reads;
mod record;
mod relay;
mod wall;

use std::sync::Arc;
use std::time::Instant;

use fleetdeck_link::control::{Control, InboundRequest};
use fleetdeck_link::methods::code as link_code;
use fleetdeck_proto::StationId;
use fleetdeck_proto::coordinator_desktop::DesktopQuery;
use fleetdeck_proto::coordinator_desktop_text::{code, text};
use fleetdeck_proto::coordinator_desktop_wire::{CoordinatorDesktopQueryEnvelope, DesktopReply};
use fleetdeck_proto::link::{Outcome, WireError};
use serde_json::Value;

// The shared pieces that tasks T3 and T5 use.
pub(crate) use audit::{RecordingStage, recording_stage};
#[allow(unused_imports)]
pub(crate) use authorize::{Authorized, Tool, authorize, recording_pass};
#[allow(unused_imports)]
pub(crate) use policy::{caller_policy, target_policy};
#[allow(unused_imports)]
pub(crate) use reads::coordinator_read;

pub(crate) use directive::{effective_directive, effective_scope};

use crate::link::DeckLink;
use envelope::Stop;
use limits::Slots;
use permits::Permits;

/// What the deck link shares for the P6 tools: the in-flight slots and the
/// read permits (§8.2, §5.7).
#[derive(Debug)]
pub(crate) struct Shared {
    slots: Arc<Slots>,
    permits: Permits,
}

impl Default for Shared {
    fn default() -> Self {
        let slots = Arc::new(Slots::default());
        Self {
            permits: Permits::new(Arc::clone(&slots)),
            slots,
        }
    }
}

impl Shared {
    pub(super) fn slots(&self) -> &Arc<Slots> {
        &self.slots
    }

    pub(super) fn permits(&self) -> &Permits {
        &self.permits
    }
}

/// Every code that a P6 reply may carry (§11.1, §11.2).
const CODES: [&str; 36] = [
    code::NOT_BOUND,
    code::NOT_GRANTED,
    code::BINDING_STALE,
    code::ELEVATION_REQUIRED,
    code::UNSUPPORTED,
    code::INCARNATION_MISMATCH,
    code::EXPIRED,
    code::INVALID_PARAMS,
    code::REQUEST_ID_CONFLICT,
    code::DESKTOP_CAP_REACHED,
    code::CAPTURE_CAP_REACHED,
    code::IMAGE_BUDGET_REACHED,
    code::REQUEST_BUDGET_REACHED,
    code::RATE_LIMITED,
    code::BUSY,
    code::AUTHORITY_LOST,
    code::AUTHORITY_UNKNOWN,
    code::FLEET_CONTROL_OFF,
    code::DESKTOP_RESULT_TOO_LARGE,
    code::IMAGE_TOO_LARGE,
    code::IMAGE_INVALID,
    code::UNKNOWN_ARTIFACT,
    code::DESKTOP_WINDOW_NOT_FOUND,
    code::DESKTOP_DISPLAY_NOT_FOUND,
    code::DESKTOP_REFERENCE_NOT_GRANTED,
    code::DESKTOP_READ_TIMEOUT,
    code::DESKTOP_UNSUPPORTED,
    code::DESKTOP_FORBIDDEN,
    code::INVALID_DESKTOP_ACTION,
    code::DESKTOP_NOT_FOUND,
    code::DESKTOP_CONFLICT,
    code::DESKTOP_FAILED,
    code::DESKTOP_OVERLOADED,
    code::DESKTOP_TIMEOUT,
    code::STATION_UNREACHABLE,
    code::DESKTOP_CONTROL_OFF,
];

/// The constant of a code of §11, or `None` for any other text: a station code
/// passes through to an agent only when it is one of these.
pub(super) fn known_code(candidate: &str) -> Option<&'static str> {
    CODES.iter().copied().find(|c| *c == candidate)
}

/// The reply of a success.
pub(super) fn reply_ok(result: Value) -> Outcome {
    reply(&DesktopReply::Ok { result })
}

/// The reply of a refusal or a failure: the code, its fixed text and the
/// operation ID when there is one.
pub(super) fn reply_err(error: &str, operation_id: Option<&str>) -> Outcome {
    reply(&DesktopReply::Err {
        error: error.to_owned(),
        message: text(error).unwrap_or_default().to_owned(),
        operation_id: operation_id.map(str::to_owned),
        capture_id: None,
    })
}

/// The reply of a refusal with a fixed text other than the text of its code
/// (R13 P5b-2 §16: a recording that finds the capture allowance used).
pub(super) fn reply_err_text(error: &str, message: &str) -> Outcome {
    reply(&DesktopReply::Err {
        error: error.to_owned(),
        message: message.to_owned(),
        operation_id: None,
        capture_id: None,
    })
}

/// The reply of a refusal or a failure of a screenshot: the code, a fixed
/// text (a capture code keeps its own text, §11.3) and the capture ID.
pub(super) fn reply_err_capture(error: &str, message: &str, capture_id: &str) -> Outcome {
    reply(&DesktopReply::Err {
        error: error.to_owned(),
        message: message.to_owned(),
        operation_id: None,
        capture_id: Some(capture_id.to_owned()),
    })
}

fn reply(body: &DesktopReply) -> Outcome {
    match serde_json::to_value(body) {
        Ok(v) => Outcome::Ok(v),
        Err(e) => Outcome::Err(WireError::new(link_code::PROTOCOL_ERROR, e.to_string())),
    }
}

/// The wire error of a malformed envelope.
fn invalid_params() -> Outcome {
    Outcome::Err(WireError::new(
        code::INVALID_PARAMS,
        text(code::INVALID_PARAMS).unwrap_or(code::INVALID_PARAMS),
    ))
}

/// The refusal of every method form that a later task serves: the code and
/// the fixed text of §11.2.
fn unsupported() -> Outcome {
    Outcome::Err(WireError::new(
        code::DESKTOP_UNSUPPORTED,
        text(code::DESKTOP_UNSUPPORTED).unwrap_or(code::DESKTOP_UNSUPPORTED),
    ))
}

fn stopped(stop: Stop) -> Outcome {
    match stop {
        Stop::Wire => invalid_params(),
        Stop::Reply(refusal) => reply_err(refusal, None),
    }
}

/// `coordinator.desktop_call` (§5.3).
pub(crate) async fn desktop_call(
    ctx: &DeckLink,
    station: StationId,
    req: &InboundRequest,
) -> Outcome {
    let started = Instant::now();
    let env = match envelope::parse_call(req) {
        Ok(env) => env,
        Err(stop) => return stopped(stop),
    };
    if !calls::served(&env.call) {
        return unsupported();
    }
    if let Err(stop) = envelope::check_call(&env, ctx.store.now_ms(), ctx.store.incarnation()) {
        return stopped(stop);
    }
    calls::run(ctx, &station, env, started).await
}

/// `coordinator.desktop_query` (§5.3).
pub(crate) async fn desktop_query(
    ctx: &DeckLink,
    station: StationId,
    req: &InboundRequest,
) -> Outcome {
    let started = Instant::now();
    let env = match envelope::parse_query(req) {
        Ok(env) => env,
        Err(stop) => return stopped(stop),
    };
    if let Err(stop) = envelope::check_query(&env, ctx.store.now_ms(), ctx.store.incarnation()) {
        return stopped(stop);
    }
    let CoordinatorDesktopQueryEnvelope {
        caller,
        binding,
        query,
        ..
    } = env;
    let who = (caller, binding);
    match query {
        DesktopQuery::DesktopInfo(args) => queries::info(ctx, &station, who, args, started).await,
        DesktopQuery::DesktopActionStatus(args) => queries::status(ctx, &station, who, args).await,
        DesktopQuery::CaptureStatus(args) => {
            capture_status::status(ctx, &station, who, args, started).await
        }
        DesktopQuery::ReadCaptureImage(args) => {
            image::read(ctx, &station, who, args, started).await
        }
    }
}

/// `desktop.authority_check` (§5.6).
pub(crate) async fn authority_check(
    ctx: &DeckLink,
    control: &Control,
    station: StationId,
    req: &InboundRequest,
) -> Outcome {
    check::authority_check(ctx, control, station, req).await
}

#[cfg(test)]
#[path = "mod_tests.rs"]
mod tests;
