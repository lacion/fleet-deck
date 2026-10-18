//! `desktop.act` with `screen_capture` and `desktop.capture_status` (R13 P5a
//! task D2, design `omarchy-p5a.md` §2.2, §2.4). The work is in
//! [`crate::desktop::capture`] and [`crate::artifacts::capture_import`]; this
//! file reads the station policy and the link identity and calls it.

use std::sync::Arc;
use std::time::Duration;

use fleetdeck_link::control::Control;
use fleetdeck_link::desktop_authority::action_hash_for;
use fleetdeck_proto::artifacts::ArtifactPublished;
use fleetdeck_proto::capture::{CaptureStatus, CaptureStatusQuery, code, text, valid_capture_id};
use fleetdeck_proto::coordinator_desktop_wire::CoordinatorContext;
use fleetdeck_proto::desktop::DesktopAction;
use fleetdeck_proto::link::{Outcome, WireError};

use super::StationLink;
use super::ok;
use crate::artifacts::ArtifactService;
use crate::artifacts::capture_import::SyncFs;
use crate::desktop::capture::{ADMISSION, Allowed, Launch, Params, Request};
use crate::desktop::{self, Fail, authority};
use crate::store::capture_ops::Lookup;

/// The deadline of `desktop.capture_status` (design §7.1).
const STATUS_TIMEOUT: Duration = Duration::from_secs(5);
/// The text of `failed` when the status lookup passes its deadline.
const STATUS_TIMED_OUT: &str = "The screenshot status did not finish in time.";
/// The text of `unsupported` for a station without an artifact directory.
pub(super) const NO_ARTIFACTS: &str = "this station has no artifact directory";

fn failed(e: Fail) -> Outcome {
    Outcome::Err(WireError::new(e.code, e.message))
}

fn service(ctx: &StationLink) -> Result<&ArtifactService, Fail> {
    ctx.manager
        .artifacts()
        .ok_or_else(|| Fail::new(desktop::code::UNSUPPORTED, NO_ARTIFACTS))
}

/// `desktop.act` with `screen_capture`: `capture_id` is the `action_id` that
/// `authority::admit` consumed. `coordinator` is the context of a coordinator
/// capture (R13 P6); the action hash covers it.
pub(super) async fn capture(
    ctx: &StationLink,
    control: &Control,
    admitted: authority::Admitted,
    capture_id: String,
    monitor: String,
    coordinator: Option<CoordinatorContext>,
) -> Outcome {
    if !valid_capture_id(&capture_id) {
        return failed(Fail::new(
            desktop::code::INVALID_PARAMS,
            "The capture ID is not valid.",
        ));
    }
    let service = match service(ctx) {
        Ok(s) => s.clone(),
        Err(e) => return failed(e),
    };
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(e) => {
            tracing::warn!(kind = ?e.kind(), "cannot find the station executable");
            return failed(Fail::new(code::CAPTURE_FAILED, text::CAPTURE_FAILED));
        }
    };
    let station_node = ctx.node.identity.fingerprint().to_string();
    let action = DesktopAction::ScreenCapture {
        monitor: monitor.clone(),
    };
    let manager = Arc::clone(&ctx.manager);
    let allowed = Arc::new(move || {
        let policy = manager.policy();
        Allowed {
            desktop: policy.allow_desktop(),
            capture: policy.allow_desktop_capture(),
            monitors: policy.capture_monitors().to_vec(),
        }
    });
    // The live station name, as `station.info` reports it.
    let name = match ctx.manager.policy().name() {
        "" => ctx.name.clone(),
        live => live.to_owned(),
    };
    let request = Request {
        service,
        allowed,
        admitted,
        deck_fp: control.params().peer.to_string(),
        capture_id,
        action_hash: action_hash_for(&action, &station_node, coordinator.as_ref()),
        monitor,
        station_id: name.clone(),
        station_name: name,
        params: Params {
            launch: Launch::ShimExec { exe },
            fs: Arc::new(SyncFs),
            admission: ADMISSION,
        },
    };
    match desktop::service().screen_capture(request).await {
        Ok(mut result) => {
            super::intake_desktop::stamp(ctx, &mut result.info);
            ok(result)
        }
        Err(e) => failed(e),
    }
}

/// The `CaptureStatus` of a lookup (design §2.4).
pub(super) fn status_of(capture_id: &str, lookup: Lookup) -> CaptureStatus {
    let capture_id = capture_id.to_owned();
    match lookup {
        Lookup::Unknown => CaptureStatus::Unknown { capture_id },
        Lookup::Running(_) => CaptureStatus::Pending { capture_id },
        Lookup::Deleted(_) => CaptureStatus::Deleted { capture_id },
        Lookup::Failed(op) => CaptureStatus::Failed {
            capture_id,
            error: op.code.unwrap_or_else(|| desktop::code::FAILED.to_owned()),
            message: op.message.unwrap_or_default(),
        },
        Lookup::Committed {
            artifact, version, ..
        } => CaptureStatus::Committed {
            capture_id,
            published: ArtifactPublished {
                artifact,
                version,
                created: true,
            },
        },
    }
}

/// `desktop.capture_status`: the durable state of one capture of the
/// calling deck. It checks no gate: it only reads the record.
pub(super) async fn status(
    ctx: &StationLink,
    control: &Control,
    params: serde_json::Value,
) -> Outcome {
    let query: CaptureStatusQuery = match serde_json::from_value(params) {
        Ok(q) => q,
        Err(e) => {
            return failed(Fail::new(
                desktop::code::INVALID_PARAMS,
                format!("params: {e}"),
            ));
        }
    };
    if !valid_capture_id(&query.capture_id) {
        return failed(Fail::new(
            desktop::code::INVALID_PARAMS,
            "The capture ID is not valid.",
        ));
    }
    let service = match service(ctx) {
        Ok(s) => s,
        Err(e) => return failed(e),
    };
    let deck_fp = control.params().peer.to_string();
    let lookup = service.store().capture_lookup(&deck_fp, &query.capture_id);
    match tokio::time::timeout(STATUS_TIMEOUT, lookup).await {
        Ok(Ok(lookup)) => ok(status_of(&query.capture_id, lookup)),
        Ok(Err(e)) => {
            tracing::warn!(error = %e, "the capture status cannot be read");
            Outcome::Err(WireError::new("internal", e.to_string()))
        }
        Err(_) => failed(Fail::new(desktop::code::FAILED, STATUS_TIMED_OUT)),
    }
}
