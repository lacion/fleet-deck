//! `desktop.capture_active`, `desktop.capture_stop` and
//! `desktop.capture_policy_now` (R13 P5b, feature `desktop_recording.v1`,
//! design `omarchy-p5b.md` §3.3, §4.3, §4.4, §6.2), and the refusal of
//! `desktop.act` with `screen_record`. Task F: stubs. Every one answers the
//! station code `unsupported` with the fixed text, and no program runs.

use fleetdeck_link::control::Control;
use fleetdeck_proto::capture::text::RECORDING_UNSUPPORTED;
use fleetdeck_proto::link::{Outcome, WireError};

use super::StationLink;
use crate::desktop::{Fail, code};

/// The refusal of a build without recording behavior.
pub(super) fn refusal() -> Fail {
    Fail::new(code::UNSUPPORTED, RECORDING_UNSUPPORTED)
}

/// [`refusal`] as a wire answer.
pub(super) fn refused() -> Outcome {
    let e = refusal();
    Outcome::Err(WireError::new(e.code, e.message))
}

/// True for the three methods of this file.
pub(super) fn has(method: &str) -> bool {
    matches!(
        method,
        "desktop.capture_active" | "desktop.capture_stop" | "desktop.capture_policy_now"
    )
}

/// Routes one of the three methods; `method` satisfies [`has`].
pub(super) async fn call(
    ctx: &StationLink,
    control: &Control,
    method: &str,
    params: serde_json::Value,
) -> Outcome {
    match method {
        "desktop.capture_active" => capture_active(ctx, control, params).await,
        "desktop.capture_stop" => capture_stop(ctx, control, params).await,
        _ => capture_policy_now(ctx, control, params).await,
    }
}

/// `desktop.capture_active`.
#[allow(clippy::unused_async, reason = "a stub until task S1")]
pub(super) async fn capture_active(
    _ctx: &StationLink,
    _control: &Control,
    _params: serde_json::Value,
) -> Outcome {
    refused()
}

/// `desktop.capture_stop`.
#[allow(clippy::unused_async, reason = "a stub until task S1")]
pub(super) async fn capture_stop(
    _ctx: &StationLink,
    _control: &Control,
    _params: serde_json::Value,
) -> Outcome {
    refused()
}

/// `desktop.capture_policy_now`.
#[allow(clippy::unused_async, reason = "a stub until task S1")]
pub(super) async fn capture_policy_now(
    _ctx: &StationLink,
    _control: &Control,
    _params: serde_json::Value,
) -> Outcome {
    refused()
}
