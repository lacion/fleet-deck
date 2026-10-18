//! `desktop.act` with `screen_record`, `desktop.capture_active`,
//! `desktop.capture_stop` and `desktop.capture_policy_now` (R13 P5b task S1,
//! feature `desktop_recording.v1`, design `omarchy-p5b.md` §3.2, §3.3, §4.3,
//! §4.4, §6.2). The work is in [`crate::desktop::recording`]; this file reads
//! the station policy and the link identity and calls it.

use std::sync::Arc;
use std::time::Duration;

use fleetdeck_link::control::Control;
use fleetdeck_link::desktop_authority::action_hash_for;
use fleetdeck_proto::capture::{CaptureStatus, code, text, valid_capture_id};
use fleetdeck_proto::coordinator_desktop_wire::CoordinatorContext;
use fleetdeck_proto::desktop::DesktopAction;
use fleetdeck_proto::link::{Outcome, WireError};
use fleetdeck_proto::recording::{
    ActiveRecording, CapturePolicyAnswer, CapturePolicyNowParams, CaptureStopParams, StopReason,
};
use fleetdeck_proto::version::feature;

use super::StationLink;
use super::intake_capture::{status_answer, status_of};
use super::ok;
use crate::artifacts::ArtifactService;
use crate::artifacts::capture_import::SyncFs;
use crate::desktop::capture::{ADMISSION, Allowed, Launch, room};
use crate::desktop::recording::{ControlLink, RecordParams, RecordRequest, RoomCheck, Timing};
use crate::desktop::{self, Desktop, Fail, authority};
use crate::media::clock::SystemClocks;
use crate::store::StoreError;
use crate::store::capture_ops::Lookup;

/// The deadline of `desktop.capture_stop` on the station (design §10).
const STOP_TIMEOUT: Duration = Duration::from_secs(5);
/// The deadline of the probe of `desktop.capture_policy_now` (design §6.2).
const PROBE_TIMEOUT: Duration = Duration::from_secs(1);
/// The text of `failed` when a stop passes its deadline.
const STOP_TIMED_OUT: &str = "The recording stop did not finish in time.";
/// The text of `not_found` for a capture that this deck does not have.
const NO_SUCH_CAPTURE: &str = "No such capture.";

/// The refusal of a link without the recording features.
pub(super) fn refusal() -> Fail {
    Fail::new(desktop::code::UNSUPPORTED, text::RECORDING_UNSUPPORTED)
}

/// [`refusal`] as a wire answer.
pub(super) fn refused() -> Outcome {
    failed(refusal())
}

fn failed(e: Fail) -> Outcome {
    Outcome::Err(WireError::new(e.code, e.message))
}

fn store_failed(e: &StoreError) -> Outcome {
    tracing::warn!(error = %e, "the recording store failed");
    failed(Fail::new(
        desktop::code::FAILED,
        "The station could not read the recording.",
    ))
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

fn recording_link(control: &Control) -> bool {
    control.params().negotiated.has(feature::DESKTOP_RECORDING)
}

fn video_link(control: &Control) -> bool {
    control.params().negotiated.has(feature::ARTIFACTS_VIDEO)
}

fn service(ctx: &StationLink) -> Result<&ArtifactService, Fail> {
    ctx.manager.artifacts().ok_or_else(|| {
        Fail::new(
            desktop::code::UNSUPPORTED,
            super::intake_capture::NO_ARTIFACTS,
        )
    })
}

/// The executable of the `shim-exec` launch: the one of the adapter, else the
/// running executable.
fn shim_exe(desktop: &Desktop) -> std::io::Result<std::path::PathBuf> {
    desktop
        .shim_exe()
        .map_or_else(std::env::current_exe, |p| Ok(p.to_path_buf()))
}

/// The live gates of the station policy.
fn allowed_of(ctx: &StationLink) -> Arc<dyn Fn() -> Allowed + Send + Sync> {
    let manager = Arc::clone(&ctx.manager);
    Arc::new(move || {
        let policy = manager.policy();
        Allowed {
            desktop: policy.allow_desktop(),
            capture: policy.allow_desktop_capture(),
            monitors: policy.capture_monitors().to_vec(),
        }
    })
}

/// `desktop.act` with `screen_record` of an owner: `capture_id` is the
/// `action_id` that `authority::admit` consumed.
pub(super) async fn record(
    ctx: &StationLink,
    control: &Control,
    admitted: authority::Admitted,
    capture_id: String,
    monitor: String,
    seconds: u32,
) -> Outcome {
    record_for(ctx, control, admitted, capture_id, monitor, seconds, None).await
}

/// [`record`] for an owner (`coordinator` is `None`) or for a coordinator
/// (R13 P5b-2): the context is the one of the authority; the action hash
/// covers it.
pub(super) async fn record_for(
    ctx: &StationLink,
    control: &Control,
    admitted: authority::Admitted,
    capture_id: String,
    monitor: String,
    seconds: u32,
    coordinator: Option<CoordinatorContext>,
) -> Outcome {
    if !(recording_link(control) && video_link(control)) {
        return refused();
    }
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
    let exe = match shim_exe(&desktop::service()) {
        Ok(exe) => exe,
        Err(e) => {
            tracing::warn!(kind = ?e.kind(), "cannot find the station executable");
            return failed(Fail::new(code::RECORDING_FAILED, text::RECORDING_FAILED));
        }
    };
    let station_node = ctx.node.identity.fingerprint().to_string();
    let action = DesktopAction::ScreenRecord {
        monitor: monitor.clone(),
        seconds,
    };
    // The live station name, as `station.info` reports it.
    let name = match ctx.manager.policy().name() {
        "" => ctx.name.clone(),
        live => live.to_owned(),
    };
    let request = RecordRequest {
        service,
        allowed: allowed_of(ctx),
        admitted,
        deck_fp: control.params().peer.to_string(),
        capture_id,
        action_hash: action_hash_for(&action, &station_node, coordinator.as_ref()),
        monitor,
        seconds,
        station_id: name.clone(),
        station_name: name,
        link: Arc::new(ControlLink(control.clone())),
        link_epoch: control
            .params()
            .local_desktop_epoch
            .clone()
            .unwrap_or_default(),
        coordinator,
        params: RecordParams {
            launch: Launch::ShimExec { exe },
            fs: Arc::new(SyncFs),
            admission: ADMISSION,
            clocks: Arc::new(SystemClocks),
            timing: Timing::default(),
            room: Arc::new(room) as RoomCheck,
        },
    };
    match desktop::service().screen_record(request).await {
        Ok(mut result) => {
            super::intake_desktop::stamp(ctx, &mut result.info);
            super::intake_desktop::stamp_previews(control, &mut result.info);
            ok(result)
        }
        Err(e) => failed(e),
    }
}

/// What a handler of the three methods knows of its caller.
pub(super) struct Caller<'a> {
    pub desktop: &'a Desktop,
    pub service: &'a ArtifactService,
    /// The authenticated deck fingerprint (64 hex).
    pub deck_fp: &'a str,
    /// The link negotiated `artifacts_video.v1`.
    pub video: bool,
}

/// The status answer of one capture of the caller.
async fn current(c: &Caller<'_>, capture_id: &str) -> Result<ActiveRecording, StoreError> {
    let store = c.service.store();
    let lookup = store.capture_lookup(c.deck_fp, capture_id).await?;
    let row = store.recording_get(c.deck_fp, capture_id).await?;
    let status = match lookup {
        // The row is not made yet: the recording is preparing.
        Lookup::Unknown => CaptureStatus::Pending {
            capture_id: capture_id.to_owned(),
        },
        other => status_of(capture_id, other),
    };
    Ok(ActiveRecording::Active {
        capture_id: capture_id.to_owned(),
        status: status_answer(status, row.as_ref(), store.now_ms(), true, c.video),
    })
}

/// `desktop.capture_active` (design §4.3): the recording that holds the
/// capture slot for this deck. A screenshot or another deck's recording
/// answers `idle`.
pub(super) async fn active(c: &Caller<'_>, params: &serde_json::Value) -> Outcome {
    let empty = params.is_null() || params.as_object().is_some_and(serde_json::Map::is_empty);
    if !empty {
        return failed(Fail::new(
            desktop::code::INVALID_PARAMS,
            "`desktop.capture_active` takes no params",
        ));
    }
    let Some(handle) = c.desktop.recording_of(c.deck_fp) else {
        return ok(ActiveRecording::Idle);
    };
    match current(c, &handle.capture_id).await {
        Ok(answer) => ok(answer),
        Err(e) => store_failed(&e),
    }
}

/// The reasons that the deck may send to `desktop.capture_stop` (design
/// §4.4, §7.5).
fn stop_reason_allowed(r: StopReason) -> bool {
    matches!(
        r,
        StopReason::OwnerStop
            | StopReason::DeckPolicy
            | StopReason::OwnerLoginEnded
            | StopReason::CoordinatorAuthorityLost
            | StopReason::ArtifactDeleted
            | StopReason::Deadline
    )
}

/// `desktop.capture_stop` (design §4.4).
pub(super) async fn stop(c: &Caller<'_>, params: serde_json::Value) -> Outcome {
    let params: CaptureStopParams = match serde_json::from_value(params) {
        Ok(p) => p,
        Err(e) => {
            return failed(Fail::new(
                desktop::code::INVALID_PARAMS,
                format!("params: {e}"),
            ));
        }
    };
    if !valid_capture_id(&params.capture_id) || !stop_reason_allowed(params.reason) {
        return failed(Fail::new(
            desktop::code::INVALID_PARAMS,
            "The stop request is not valid.",
        ));
    }
    let work = async {
        let store = c.service.store();
        let lookup = store.capture_lookup(c.deck_fp, &params.capture_id).await?;
        if matches!(lookup, Lookup::Unknown) {
            return Ok(failed(Fail::new(desktop::code::NOT_FOUND, NO_SUCH_CAPTURE)));
        }
        if store
            .recording_get(c.deck_fp, &params.capture_id)
            .await?
            .is_none()
        {
            return Ok(failed(Fail::new(code::NOT_RECORDING, text::NOT_RECORDING)));
        }
        if matches!(lookup, Lookup::Running(_)) {
            let outcome = c
                .desktop
                .stop_recording(store, c.deck_fp, &params.capture_id, params.reason)
                .await?;
            tracing::debug!(?outcome, reason = ?params.reason, "a recording stop request");
        }
        // A terminal operation, or the state after the stop: the current
        // status.
        let answer = current(c, &params.capture_id).await?;
        Ok::<_, StoreError>(match answer {
            ActiveRecording::Active { status, .. } => ok(status),
            ActiveRecording::Idle => ok(ActiveRecording::Idle),
        })
    };
    match tokio::time::timeout(STOP_TIMEOUT, work).await {
        Ok(Ok(outcome)) => outcome,
        Ok(Err(e)) => store_failed(&e),
        Err(_) => failed(Fail::new(desktop::code::FAILED, STOP_TIMED_OUT)),
    }
}

/// `desktop.capture_policy_now` (design §6.2): the live policy for one
/// recording of this deck. `epoch` is the `local_desktop_epoch` of the
/// connection; `allowed` is read after the probe.
pub(super) async fn policy_now(
    c: &Caller<'_>,
    allowed: &(dyn Fn() -> Allowed + Send + Sync),
    epoch: &str,
    params: serde_json::Value,
) -> Outcome {
    let params: CapturePolicyNowParams = match serde_json::from_value(params) {
        Ok(p) => p,
        Err(e) => {
            return failed(Fail::new(
                desktop::code::INVALID_PARAMS,
                format!("params: {e}"),
            ));
        }
    };
    if !valid_capture_id(&params.capture_id) {
        return failed(Fail::new(
            desktop::code::INVALID_PARAMS,
            "The capture ID is not valid.",
        ));
    }
    let row = match c
        .service
        .store()
        .recording_get(c.deck_fp, &params.capture_id)
        .await
    {
        Ok(row) => row,
        Err(e) => return store_failed(&e),
    };
    // Only this deck's recording with this hash.
    let Some(row) = row.filter(|r| r.op.action_hash == params.action_hash) else {
        return failed(Fail::new(desktop::code::NOT_FOUND, NO_SUCH_CAPTURE));
    };
    let monitor = row.op.monitor;
    let (present, unlocked) = c.desktop.policy_probe(&monitor, PROBE_TIMEOUT).await;
    // The settings are read after the probe, never from a projection.
    let now = allowed();
    let monitor_allowed = now.monitors.is_empty() || now.monitors.contains(&monitor);
    ok(CapturePolicyAnswer {
        valid: now.desktop && now.capture && monitor_allowed && present && unlocked,
        desktop_epoch: epoch.to_owned(),
        allow_desktop: now.desktop,
        allow_desktop_capture: now.capture,
        monitor_allowed,
        monitor_present: present,
        unlocked,
    })
}

/// `desktop.capture_active`.
pub(super) async fn capture_active(
    ctx: &StationLink,
    control: &Control,
    params: serde_json::Value,
) -> Outcome {
    if !recording_link(control) {
        return refused();
    }
    let service = match service(ctx) {
        Ok(s) => s,
        Err(e) => return failed(e),
    };
    let desktop = desktop::service();
    let deck_fp = control.params().peer.to_string();
    let caller = Caller {
        desktop: &desktop,
        service,
        deck_fp: &deck_fp,
        video: video_link(control),
    };
    active(&caller, &params).await
}

/// `desktop.capture_stop`.
pub(super) async fn capture_stop(
    ctx: &StationLink,
    control: &Control,
    params: serde_json::Value,
) -> Outcome {
    if !recording_link(control) {
        return refused();
    }
    let service = match service(ctx) {
        Ok(s) => s,
        Err(e) => return failed(e),
    };
    let desktop = desktop::service();
    let deck_fp = control.params().peer.to_string();
    let caller = Caller {
        desktop: &desktop,
        service,
        deck_fp: &deck_fp,
        video: video_link(control),
    };
    stop(&caller, params).await
}

/// `desktop.capture_policy_now`.
pub(super) async fn capture_policy_now(
    ctx: &StationLink,
    control: &Control,
    params: serde_json::Value,
) -> Outcome {
    if !recording_link(control) {
        return refused();
    }
    let service = match service(ctx) {
        Ok(s) => s,
        Err(e) => return failed(e),
    };
    let desktop = desktop::service();
    let deck_fp = control.params().peer.to_string();
    let caller = Caller {
        desktop: &desktop,
        service,
        deck_fp: &deck_fp,
        video: video_link(control),
    };
    let epoch = control
        .params()
        .local_desktop_epoch
        .clone()
        .unwrap_or_default();
    let allowed = allowed_of(ctx);
    policy_now(&caller, &*allowed, &epoch, params).await
}

#[cfg(test)]
#[path = "intake_recording_tests.rs"]
mod tests;
