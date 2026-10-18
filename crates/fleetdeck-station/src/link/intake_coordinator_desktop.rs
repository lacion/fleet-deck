//! The target side of the coordinator desktop methods of the station link
//! (R13 P6, feature `desktop_mcp.v1`; `docs/v2/design/omarchy-p6.md` §4, §5.5,
//! §5.7, §6.3, §7.3).
//!
//! - [`act`]: a coordinator authority of `desktop.act`, right after the
//!   admission. It checks the context, claims the operation (`notify`,
//!   `preview_open`, `preview_close`), reads the live gates and runs the
//!   action with the remote dispatch check right before the effect.
//! - [`policy_now`]: `desktop.policy_now`, the live policy of this station.
//! - [`coordinator_read`]: `desktop.coordinator_read`, one protected read:
//!   the work first, then the remote check, then the live gates, then the
//!   data.

use std::sync::Arc;
use std::time::Duration;

use fleetdeck_link::control::Control;
use fleetdeck_proto::artifacts::{ArtifactFileQuery, ArtifactGetQuery, ArtifactKind};
use fleetdeck_proto::capture::valid_capture_id;
use fleetdeck_proto::coordinator_desktop_text::{code as p6_code, deadline_ms, limits};
use fleetdeck_proto::coordinator_desktop_wire::{
    AuthorityPhase, CoordinatorContext, CoordinatorRead, DesktopInputPolicyAnswer,
    DesktopPolicyAnswer, DesktopPolicyNow, PolicyPurpose, ProtectedRead,
};
use fleetdeck_proto::desktop::{DesktopActResult, DesktopAction};
use fleetdeck_proto::link::{Outcome, WireError};
use fleetdeck_proto::version::feature;
use serde_json::{Value, json};

use super::StationLink;
use super::ok;
use crate::artifacts::{ArtifactError, ArtifactService};
use crate::desktop::authority::Admitted;
use crate::desktop::coordinator::{
    DispatchCheck, Extras, Gates, LinkCheck, Live, check_live, claim_kind, map_info, p6_fail,
    refusal, tailnet_host_of, validate,
};
use crate::desktop::{self, Desktop, Fail};
use crate::store::Store;
use crate::store::capture_ops::Lookup;
use crate::store::desktop_ops::Claim;

/// The text of `failed` when the claim of an operation cannot be written.
const CLAIM_FAILED: &str = "The station could not record the desktop action.";
/// The text of `failed` when a capture artifact cannot be read.
const READ_FAILED: &str = "The station could not read the capture.";

/// What the deck gave with a coordinator action: the operation id, the hash
/// and the coordinator context of the authority.
pub(super) struct Given {
    pub action_id: String,
    pub action_hash: String,
    pub coordinator: Option<CoordinatorContext>,
}

fn failed(e: Fail) -> Outcome {
    Outcome::Err(WireError::new(e.code, e.message))
}

fn bad_params(e: impl std::fmt::Display) -> Fail {
    Fail::new(desktop::code::INVALID_PARAMS, format!("params: {e}"))
}

/// The refusal of both methods for a build or a link without the behavior:
/// the code and the fixed text of §11.2.
pub(super) fn unsupported() -> Outcome {
    failed(p6_fail(p6_code::DESKTOP_UNSUPPORTED))
}

/// The live settings of this station, read at each call.
fn live_of(ctx: &StationLink) -> Arc<dyn Fn() -> Live + Send + Sync> {
    let manager = Arc::clone(&ctx.manager);
    Arc::new(move || Live::of(&manager.policy()))
}

/// This station's tailnet name from the Taildrive status read (3 s at most);
/// `None` when it is not known.
async fn tailnet_host(ctx: &StationLink) -> Option<String> {
    let source = ctx.fleet_git.taildrive_source()?;
    match source.cli.status().await {
        Ok(status) => tailnet_host_of(&status.this.dns_name),
        Err(e) => {
            tracing::debug!(error = %e, "the tailnet name is not known");
            None
        }
    }
}

// ------------------------------------------------------------------ act

/// A coordinator `desktop.act`, right after the admission (§6.3).
pub(super) async fn act(
    ctx: &StationLink,
    control: &Control,
    admitted: Admitted,
    action: DesktopAction,
    given: Given,
) -> Outcome {
    // A wake authority with `preview_close` has no coordinator (the owner
    // `preview_close` takes the owner path of `intake_desktop`).
    let coordinator = match validate(
        given.coordinator.as_ref(),
        &action,
        &control.params().negotiated,
    ) {
        Ok(c) => c.clone(),
        Err(e) => return failed(e),
    };
    let deck_fp = control.params().peer.to_string();
    let capture = matches!(
        action,
        DesktopAction::ScreenCapture { .. } | DesktopAction::ScreenRecord { .. }
    );
    let monitor = match &action {
        DesktopAction::ScreenCapture { monitor } | DesktopAction::ScreenRecord { monitor, .. } => {
            Some(monitor.clone())
        }
        _ => None,
    };
    let live = live_of(ctx);
    let gates: Gates = Arc::new(move || check_live(&live(), capture, monitor.as_deref()));
    let tailnet_host = if matches!(action, DesktopAction::BrowserOpen { .. }) {
        tailnet_host(ctx).await
    } else {
        None
    };
    let remote: Arc<dyn DispatchCheck> = Arc::new(LinkCheck::new(
        control.clone(),
        given.action_id.clone(),
        given.action_hash.clone(),
    ));
    let extras = Extras {
        deck_fp: deck_fp.clone(),
        tailnet_host,
    };
    let admitted = admitted.coordinated(remote, Arc::clone(&gates), extras);
    if let DesktopAction::ScreenCapture { monitor } = action {
        return super::intake_capture::capture(
            ctx,
            control,
            admitted,
            given.action_id,
            monitor,
            Some(coordinator),
        )
        .await;
    }
    // R13 P5b-2: a coordinator recording has the recording operation with
    // the context; `validate` allowed it only on a link with the three
    // recording features.
    if let DesktopAction::ScreenRecord { monitor, seconds } = action {
        return super::intake_recording::record_for(
            ctx,
            control,
            admitted,
            given.action_id,
            monitor,
            seconds,
            Some(coordinator),
        )
        .await;
    }
    // The checks of `validate` leave only the three claimed kinds here.
    let Some(kind) = claim_kind(&action) else {
        return failed(refusal());
    };
    let allowed = super::intake_desktop::gate(ctx);
    let desktop = desktop::service();
    let run = Run {
        store: ctx.manager.store(),
        desktop: &desktop,
        deck_fp: &deck_fp,
        gate: &allowed,
    };
    match run_claimed(&run, &given, &coordinator, kind, &gates, action, admitted).await {
        Ok(mut result) => {
            super::intake_desktop::stamp(ctx, &mut result.info);
            ok(result)
        }
        Err(e) => failed(e),
    }
}

/// What the claimed run of an action needs of the station.
pub(crate) struct Run<'a> {
    pub store: &'a Store,
    pub desktop: &'a Arc<Desktop>,
    pub deck_fp: &'a str,
    /// The live `allow_desktop`.
    pub gate: &'a (dyn Fn() -> bool + Send + Sync),
}

/// The claimed run of `notify`, `preview_open` and `preview_close` (§7.3):
/// the claim, one early read of the live gates, the action, the end of the
/// claim. A repeat of a `done` operation answers its stored result with a
/// fresh read-only info; every other repeat is `authority_unknown`. Neither
/// runs an effect.
///
/// # Errors
/// The failure of the claim write, `authority_unknown`, a gate that is off,
/// or the failure of the action.
pub(crate) async fn run_claimed(
    run: &Run<'_>,
    given: &Given,
    coordinator: &CoordinatorContext,
    kind: &str,
    gates: &Gates,
    action: DesktopAction,
    admitted: Admitted,
) -> Result<DesktopActResult, Fail> {
    match claim(run.store, run.deck_fp, given, coordinator, kind).await? {
        Claim::New => {}
        Claim::Done(result) => return replay(run, result).await,
        Claim::Seen => return Err(p6_fail(p6_code::AUTHORITY_UNKNOWN)),
    }
    if let Err(e) = gates() {
        finish(run, &given.action_id, Err(e.code)).await;
        return Err(e);
    }
    let result = run.desktop.act(run.gate, action, admitted).await;
    let stored = match &result {
        Ok(r) => Ok(Some(stored_result(r))),
        Err(e) => Err(e.code),
    };
    finish(run, &given.action_id, stored).await;
    result
}

/// The durable claim of one operation (§7.3).
async fn claim(
    store: &Store,
    deck_fp: &str,
    given: &Given,
    coordinator: &CoordinatorContext,
    kind: &str,
) -> Result<Claim, Fail> {
    store
        .desktop_op_claim(
            deck_fp,
            &given.action_id,
            &given.action_hash,
            kind,
            coordinator,
            store.now_ms(),
        )
        .await
        .map_err(|e| {
            tracing::warn!(error = %e, "a desktop claim cannot be written");
            Fail::new(desktop::code::FAILED, CLAIM_FAILED)
        })
}

/// The result that a claimed operation keeps: the handle of a preview, else
/// an empty object.
fn stored_result(result: &DesktopActResult) -> Value {
    match &result.window_id {
        Some(id) => json!({ "window_id": id }),
        None => json!({}),
    }
}

/// Ends the claim. A failure to write is logged; the answer stays the real
/// result, and the row stays `claimed`, so a repeat gives `authority_unknown`.
async fn finish(run: &Run<'_>, id: &str, outcome: Result<Option<Value>, &str>) {
    if let Err(e) = run
        .store
        .desktop_op_finish(run.deck_fp, id, outcome, run.store.now_ms())
        .await
    {
        tracing::warn!(error = %e, "a desktop claim cannot be finished");
    }
}

/// The answer of a repeated operation that is `done`: its stored result and a
/// fresh read-only info. The stored operation is untouched.
async fn replay(run: &Run<'_>, result: Option<Value>) -> Result<DesktopActResult, Fail> {
    let window_id = result
        .as_ref()
        .and_then(|v| v.get("window_id"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    let info = run.desktop.info_fresh(run.gate).await?;
    Ok(DesktopActResult {
        address: None,
        window_id,
        info,
        capture: None,
        info_fresh: None,
    })
}

// ----------------------------------------------------------- policy_now

/// The answer of `desktop.policy_now` for the live settings `live` (§5.5).
pub(crate) fn policy_answer(live: &Live, monitor: Option<&str>) -> DesktopPolicyAnswer {
    DesktopPolicyAnswer {
        allow_fleet_control: live.fleet_control,
        allow_desktop: live.desktop,
        allow_desktop_capture: live.capture,
        monitor_allowed: monitor
            .is_none_or(|m| live.monitors.is_empty() || live.monitors.iter().any(|x| x == m)),
    }
}

/// The answer of `desktop.policy_now` for the input purpose (R13 P7 §6.4).
/// A null monitor is allowed only when the capture monitor list is empty (all
/// monitors): the target checks the real monitor before any input. A named
/// monitor follows the capture monitor rule.
pub(crate) fn input_policy_answer(live: &Live, monitor: Option<&str>) -> DesktopInputPolicyAnswer {
    DesktopInputPolicyAnswer {
        allow_fleet_control: live.fleet_control,
        allow_desktop: live.desktop,
        allow_desktop_capture: live.capture,
        allow_desktop_input: live.desktop_input,
        monitor_allowed: match monitor {
            None => live.monitors.is_empty(),
            Some(m) => live.monitors.is_empty() || live.monitors.iter().any(|x| x == m),
        },
    }
}

/// `desktop.policy_now` (§5.5): the live policy of this station.
pub(super) fn policy_now(
    ctx: &StationLink,
    control: &Control,
    params: serde_json::Value,
) -> Outcome {
    let query: DesktopPolicyNow = match serde_json::from_value(params) {
        Ok(q) => q,
        Err(e) => return failed(bad_params(e)),
    };
    let negotiated = &control.params().negotiated;
    policy_reply(
        &Live::of(&ctx.manager.policy()),
        &query,
        negotiated.has(feature::CAPTURE_MCP),
        negotiated.has(feature::DESKTOP_INPUT_MCP),
    )
}

/// The reply of `desktop.policy_now` for the live settings `live`. The
/// capture purpose needs `capture_mcp.v1` and the input purpose needs
/// `desktop_input_mcp.v1` (R13 P7 §6.4): without it the answer is
/// `unsupported`. The `desktop` and `capture` answers keep their P6 bytes.
pub(crate) fn policy_reply(
    live: &Live,
    query: &DesktopPolicyNow,
    capture_mcp: bool,
    input_mcp: bool,
) -> Outcome {
    match query.purpose {
        PolicyPurpose::Capture if !capture_mcp => unsupported(),
        PolicyPurpose::Input if !input_mcp => unsupported(),
        PolicyPurpose::Input => ok(input_policy_answer(live, query.monitor.as_deref())),
        PolicyPurpose::Desktop | PolicyPurpose::Capture => {
            ok(policy_answer(live, query.monitor.as_deref()))
        }
    }
}

// ------------------------------------------------------ coordinator_read

/// A 26-character ASCII alphanumeric id.
fn id26(id: &str) -> bool {
    id.len() == 26 && id.bytes().all(|b| b.is_ascii_alphanumeric())
}

/// Lower case hex of exactly `len` characters.
fn hex_of(text: &str, len: usize) -> bool {
    text.len() == len
        && text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Step 1: the shapes of the request (Q2 of the first T2 worker).
pub(crate) fn validate_read(req: &CoordinatorRead) -> Result<(), Fail> {
    let shape = hex_of(&req.read_id, 32)
        && hex_of(&req.read_hash, 64)
        && match &req.read {
            ProtectedRead::Info => true,
            ProtectedRead::ActionStatus { operation_id } => id26(operation_id),
            ProtectedRead::CaptureStatus { capture_id } => valid_capture_id(capture_id),
            ProtectedRead::CaptureFile {
                artifact_id,
                offset,
                ..
            } => id26(artifact_id) && *offset == 0,
        };
    if shape {
        Ok(())
    } else {
        Err(p6_fail(p6_code::INVALID_PARAMS))
    }
}

/// What a read needs of the station.
pub(crate) struct Reader<'a> {
    pub deck_fp: &'a str,
    pub desktop: &'a Arc<Desktop>,
    pub service: Option<&'a ArtifactService>,
    pub store: &'a Store,
    pub live: &'a (dyn Fn() -> Live + Send + Sync),
}

/// One protected read (§5.7), in this order: the shapes, the bounded work
/// without a return, the remote check, the live gates, the data. Every
/// failure returns no data.
pub(crate) async fn read_with(
    reader: &Reader<'_>,
    check: &dyn DispatchCheck,
    req: &CoordinatorRead,
) -> Result<Value, Fail> {
    validate_read(req)?;
    let (data, monitor) = reader.work(&req.read).await?;
    check.check(AuthorityPhase::Read, Some(&req.read)).await?;
    let capture = matches!(
        req.read,
        ProtectedRead::CaptureStatus { .. } | ProtectedRead::CaptureFile { .. }
    );
    // A capture read checks the monitor of its capture against the live
    // `capture_monitors`, read after the await (§2.3, §4.9).
    check_live(&(reader.live)(), capture, monitor.as_deref())?;
    Ok(data)
}

fn read_timeout() -> Fail {
    p6_fail(p6_code::DESKTOP_READ_TIMEOUT)
}

/// The failure of an artifact service error: a miss and a missing file are
/// the same `unknown_artifact`; anything else is `failed`.
fn artifact_fail(e: &ArtifactError) -> Fail {
    use fleetdeck_proto::artifacts::code as art;
    if e.code == art::UNKNOWN_ARTIFACT || e.code == art::NOT_FOUND {
        miss()
    } else if e.code == art::INVALID_PARAMS {
        p6_fail(p6_code::INVALID_PARAMS)
    } else {
        tracing::warn!(code = e.code, "a capture artifact cannot be read");
        Fail::new(desktop::code::FAILED, READ_FAILED)
    }
}

fn miss() -> Fail {
    p6_fail(p6_code::UNKNOWN_ARTIFACT)
}

impl Reader<'_> {
    fn artifacts(&self) -> Result<&ArtifactService, Fail> {
        self.service.ok_or_else(|| {
            Fail::new(
                desktop::code::UNSUPPORTED,
                super::intake_capture::NO_ARTIFACTS,
            )
        })
    }

    /// Step 2: the read work. It returns the data to the caller of
    /// [`read_with`], which gives it out only after the checks, and the
    /// monitor of the capture (from its `capture_ops` row) for a capture
    /// read; `None` for another read and for a capture without a row.
    async fn work(&self, read: &ProtectedRead) -> Result<(Value, Option<String>), Fail> {
        match read {
            ProtectedRead::Info => Ok((self.info().await?, None)),
            ProtectedRead::ActionStatus { operation_id } => {
                Ok((self.action_status(operation_id).await?, None))
            }
            ProtectedRead::CaptureStatus { capture_id } => self.capture_status(capture_id).await,
            ProtectedRead::CaptureFile {
                artifact_id,
                version,
                length,
                ..
            } => self.capture_file(artifact_id, *version, *length).await,
        }
    }

    async fn info(&self) -> Result<Value, Fail> {
        let live = self.live;
        let gate = || live().desktop;
        let limit = Duration::from_millis(deadline_ms::INFO_STATION);
        let info = tokio::time::timeout(limit, self.desktop.info_fresh(&gate))
            .await
            .map_err(|_| read_timeout())??;
        let mapped = map_info(&info, &live())?;
        serde_json::to_value(mapped).map_err(|_| p6_fail(p6_code::DESKTOP_RESULT_TOO_LARGE))
    }

    async fn action_status(&self, operation_id: &str) -> Result<Value, Fail> {
        let row = self
            .store
            .desktop_op_get(self.deck_fp, operation_id)
            .await
            .map_err(|e| {
                tracing::warn!(error = %e, "a desktop claim cannot be read");
                Fail::new(desktop::code::FAILED, CLAIM_FAILED)
            })?;
        Ok(match row {
            None => json!({ "state": "unknown" }),
            Some(row) => {
                let mut out = json!({ "state": row.state });
                if row.state == "done"
                    && let Some(result) = row.result
                {
                    out["result"] = result;
                }
                if row.state == "failed"
                    && let Some(code) = row.code
                {
                    out["code"] = Value::String(code);
                }
                out
            }
        })
    }

    async fn capture_status(&self, capture_id: &str) -> Result<(Value, Option<String>), Fail> {
        let store = self.artifacts()?.store();
        let limit = Duration::from_millis(deadline_ms::CAPTURE_LOOKUP_STATION);
        let lookup = tokio::time::timeout(limit, store.capture_lookup(self.deck_fp, capture_id))
            .await
            .map_err(|_| read_timeout())?
            .map_err(|e| {
                tracing::warn!(error = %e, "the capture status cannot be read");
                Fail::new(desktop::code::FAILED, READ_FAILED)
            })?;
        let monitor = match &lookup {
            Lookup::Unknown => None,
            Lookup::Running(op) | Lookup::Failed(op) | Lookup::Deleted(op) => {
                Some(op.monitor.clone())
            }
            Lookup::Committed { op, .. } => Some(op.monitor.clone()),
        };
        let value = serde_json::to_value(super::intake_capture::status_of(capture_id, lookup))
            .map_err(|_| Fail::new(desktop::code::FAILED, READ_FAILED))?;
        Ok((value, monitor))
    }

    /// One capture file: a capture with coordinator provenance, kind
    /// `image`, version 1, the sole file `capture.png`, no tombstone; the
    /// whole file in one chunk at offset 0.
    async fn capture_file(
        &self,
        artifact_id: &str,
        version: u32,
        length: u64,
    ) -> Result<(Value, Option<String>), Fail> {
        use fleetdeck_proto::capture::CaptureCaller;
        let service = self.artifacts()?;
        if version != 1 {
            return Err(miss());
        }
        let detail = service
            .get(ArtifactGetQuery {
                artifact_id: artifact_id.to_owned(),
                version: Some(1),
            })
            .await
            .map_err(|e| artifact_fail(&e))?;
        let provenance = detail.version.capture.as_ref();
        let coordinator_capture =
            provenance.is_some_and(|c| c.caller == CaptureCaller::Coordinator);
        let sole = detail.version.files == [crate::store::capture_ops::CAPTURE_FILE];
        if detail.artifact.kind != ArtifactKind::Image
            || detail.version.version != 1
            || !coordinator_capture
            || !sole
        {
            return Err(miss());
        }
        // The monitor of the capture, from its `capture_ops` row. A capture
        // without a row is a miss (no data).
        let capture_id = provenance.map(|c| c.capture_id.clone()).unwrap_or_default();
        let op = self
            .store
            .capture_get(self.deck_fp, &capture_id)
            .await
            .map_err(|e| {
                tracing::warn!(error = %e, "the capture operation cannot be read");
                Fail::new(desktop::code::FAILED, READ_FAILED)
            })?
            .ok_or_else(miss)?;
        let size = detail.version.size;
        if length != size {
            return Err(p6_fail(p6_code::INVALID_PARAMS));
        }
        if size > limits::IMAGE_MAX_BYTES {
            return Err(p6_fail(p6_code::IMAGE_TOO_LARGE));
        }
        let limit = Duration::from_millis(deadline_ms::FILE_REQUEST);
        let file = tokio::time::timeout(
            limit,
            service.file(ArtifactFileQuery {
                artifact_id: artifact_id.to_owned(),
                version: 1,
                path: crate::store::capture_ops::CAPTURE_FILE.to_owned(),
                offset: Some(0),
                length: Some(length),
            }),
        )
        .await
        .map_err(|_| read_timeout())?
        .map_err(|e| artifact_fail(&e))?;
        let value = serde_json::to_value(file)
            .map_err(|_| Fail::new(desktop::code::FAILED, READ_FAILED))?;
        Ok((value, Some(op.monitor)))
    }
}

/// `desktop.coordinator_read` (§5.7): one protected read, one chunk.
pub(super) async fn coordinator_read(
    ctx: &StationLink,
    control: &Control,
    params: serde_json::Value,
) -> Outcome {
    let req: CoordinatorRead = match serde_json::from_value(params) {
        Ok(r) => r,
        Err(e) => return failed(bad_params(e)),
    };
    let deck_fp = control.params().peer.to_string();
    let desktop = desktop::service();
    let live = live_of(ctx);
    let reader = Reader {
        deck_fp: &deck_fp,
        desktop: &desktop,
        service: ctx.manager.artifacts(),
        store: ctx.manager.store(),
        live: &*live,
    };
    let check = LinkCheck::new(control.clone(), req.read_id.clone(), req.read_hash.clone());
    match read_with(&reader, &check, &req).await {
        Ok(data) => ok(data),
        Err(e) => failed(e),
    }
}

#[cfg(test)]
#[path = "intake_coordinator_desktop_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "intake_coordinator_desktop_input_tests.rs"]
mod input_tests;
