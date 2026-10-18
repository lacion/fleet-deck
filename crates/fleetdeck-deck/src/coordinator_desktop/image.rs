//! `read_capture_image` (R13 P6 §4.9, §5.7, §8, §9, decisions O12-2 item 1,
//! O12-9 items 9 and 10, task T3 item 7): the one narrow read of a capture
//! PNG for the coordinator that took the screenshot.
//!
//! In order: authorization, the version, the capture record of this grant and
//! caller, the size, the two live policies, the in-flight slots, the
//! reservation of the image budget (never refunded), the protected read of the
//! whole file in one chunk, the checks of the bytes, the recheck of the
//! tombstone and of the authority, and the answer. Every case in which the
//! artifact is not this caller's committed capture is the same
//! `unknown_artifact`.

use std::time::{Duration, Instant};

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use fleetdeck_proto::artifacts::ArtifactFile;
use fleetdeck_proto::capture::CaptureKind;
use fleetdeck_proto::coordinator_desktop::{CaptureImageAnswer, CaptureImageMeta, ImageArguments};
use fleetdeck_proto::coordinator_desktop_text::{code, deadline_ms, limits, text};
use fleetdeck_proto::coordinator_desktop_wire::{
    CoordinatorContext, DesktopReply, PolicyPurpose, ProtectedRead,
};
use fleetdeck_proto::display_directive::ArtifactRef;
use fleetdeck_proto::link::{MAX_CONTROL_FRAME_BYTES, Outcome, WireError};
use fleetdeck_proto::mutation::CoordinatorBinding;
use fleetdeck_proto::{SessionId, StationId};
use serde_json::{Value, json};

use super::audit::{self, Facts};
use super::authorize::{Authorized, Held, Tool, authorize, local_pass, store_error};
use super::policy::{caller_policy, target_policy};
use super::reads::coordinator_read;
use super::{reply_err, reply_err_capture};
use crate::link::DeckLink;
use crate::store::AuditResult;
use crate::store::desktop_captures::{CaptureRow, CaptureState};

/// The eight bytes that start every PNG file.
const PNG_SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

/// What the frame envelope of the answer may use of the control frame limit.
const FRAME_ROOM: usize = 4096;

/// The audit action of an image read (§12).
const ACTION: &str = "desktop.capture_read";

/// The lowercase hex SHA-256 of `bytes`.
fn sha256_hex(bytes: &[u8]) -> String {
    ring::digest::digest(&ring::digest::SHA256, bytes)
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Checks the answer of the one-chunk read against the committed version and
/// returns the received base64 text (the decoded bytes are dropped here):
/// offset 0, end of file, exactly `size` bytes, the SHA-256 of the version and
/// the PNG signature. Any miss is `image_invalid`.
pub(super) fn check_file(value: Value, size: u64, sha256: &str) -> Result<String, &'static str> {
    let invalid = code::IMAGE_INVALID;
    let file: ArtifactFile = serde_json::from_value(value).map_err(|_| invalid)?;
    if file.offset != 0 || !file.eof {
        return Err(invalid);
    }
    // The base64 of `size` bytes has this many characters: a longer text is
    // refused before it is decoded.
    let longest = size.div_ceil(3).saturating_mul(4);
    if !u64::try_from(file.data_base64.len()).is_ok_and(|n| n <= longest) {
        return Err(invalid);
    }
    let bytes = STANDARD.decode(&file.data_base64).map_err(|_| invalid)?;
    if u64::try_from(bytes.len()) != Ok(size)
        || !bytes.starts_with(&PNG_SIGNATURE)
        || sha256_hex(&bytes) != sha256
    {
        return Err(invalid);
    }
    Ok(file.data_base64)
}

/// The complete reply of a success, or `image_too_large` when its serialized
/// form does not fit the control frame with room for the envelope.
pub(super) fn answer(meta: CaptureImageMeta, png_base64: String) -> Result<Outcome, &'static str> {
    let result = serde_json::to_value(CaptureImageAnswer { meta, png_base64 })
        .map_err(|_| code::DESKTOP_FAILED)?;
    let reply = DesktopReply::Ok { result };
    let bytes = serde_json::to_vec(&reply).map_err(|_| code::DESKTOP_FAILED)?;
    if bytes.len() > MAX_CONTROL_FRAME_BYTES - FRAME_ROOM {
        return Err(code::IMAGE_TOO_LARGE);
    }
    drop(bytes);
    Ok(match serde_json::to_value(&reply) {
        Ok(v) => Outcome::Ok(v),
        Err(e) => Outcome::Err(WireError::new(
            fleetdeck_link::methods::code::PROTOCOL_ERROR,
            e.to_string(),
        )),
    })
}

/// Whether `row` is a committed and indexed capture of this caller and this
/// grant, with no tombstone.
fn is_callers(row: &CaptureRow, ctx: &CoordinatorContext) -> bool {
    let owner = row
        .coordinator
        .as_deref()
        .and_then(|c| serde_json::from_str::<CoordinatorContext>(c).ok());
    // Only a screenshot passes: a recording artifact is `unknown_artifact`,
    // also when it is small (R13 P5b-2 §13.4).
    let screenshot = row
        .published
        .as_ref()
        .and_then(|p| p.version.capture.as_ref())
        .is_some_and(|c| c.kind == CaptureKind::Screenshot);
    !row.deleted
        && row.state == CaptureState::Committed
        && row.indexed
        && screenshot
        && owner.is_some_and(|c| {
            c.grant_id == ctx.grant_id
                && c.caller_station == ctx.caller_station
                && c.caller_session == ctx.caller_session
        })
}

/// The one audit row of a read. The artifact ID is written only when a
/// capture of this caller holds it (a refused unresolved identifier is
/// recorded as its code).
struct Row<'a> {
    link: &'a DeckLink,
    auth: &'a Authorized,
    capture_id: Option<String>,
    artifact_id: Option<String>,
    reserved: bool,
}

impl Row<'_> {
    async fn write(&self, stage: &'static str, result: AuditResult, failure: Option<&str>) {
        let facts = Facts {
            action: ACTION,
            ctx: self.auth.ctx(),
            target: self.auth.target(),
            operation_id: None,
            window_id: None,
            monitor: None,
        };
        let mut extra = vec![("capture_id", json!(self.capture_id))];
        if let Some(artifact_id) = &self.artifact_id {
            extra.push(("artifact_id", json!(artifact_id)));
        }
        if self.reserved {
            extra.push(("reserved", json!(limits::IMAGE_RESERVE_BYTES)));
        }
        let row = facts.row_with(stage, result, failure, None, &extra);
        audit::write_or_log(&self.link.store, row).await;
    }

    /// The reply of a code: with the capture ID once a capture of this caller
    /// holds the artifact. `unknown_artifact` never names a capture, so every
    /// case of it is the same answer.
    fn reply(&self, failure: &'static str) -> Outcome {
        match &self.capture_id {
            Some(id) if failure != code::UNKNOWN_ARTIFACT => {
                reply_err_capture(failure, text(failure).unwrap_or_default(), id)
            }
            _ => reply_err(failure, None),
        }
    }

    /// A refusal before the reservation.
    async fn refuse(&self, refusal: &'static str) -> Outcome {
        self.write(audit::REFUSED, AuditResult::Refused, Some(refusal))
            .await;
        self.reply(refusal)
    }

    /// A failure after the reservation: the charge stays.
    async fn fail(&self, failure: &'static str) -> Outcome {
        self.write(audit::OUTCOME, AuditResult::Failed, Some(failure))
            .await;
        self.reply(failure)
    }
}

/// The recheck before the answer (§4.9, task T3 item 9): the tombstone, the
/// deck switches and the grant. A tombstone is `unknown_artifact`; a lost
/// authority is `authority_lost`; a store error is `authority_unknown`.
async fn recheck(
    link: &DeckLink,
    auth: &Authorized,
    artifact: &ArtifactRef,
) -> Result<(), &'static str> {
    match link
        .store
        .tombstone(artifact.station_id.clone(), artifact.artifact_id.clone())
        .await
    {
        Ok(None) => {}
        Ok(Some(_)) => return Err(code::UNKNOWN_ARTIFACT),
        Err(e) => return Err(store_error(&e)),
    }
    let ctx = auth.ctx();
    let held = Held {
        grant_id: &ctx.grant_id,
        binding_id: &ctx.binding_id,
        generation: ctx.generation,
        caller: &ctx.caller_station,
        target: auth.target(),
        class: auth.class(),
    };
    local_pass(link, &held)
        .await
        .map(|_| ())
        .map_err(|refusal| {
            if refusal == code::AUTHORITY_UNKNOWN {
                code::AUTHORITY_UNKNOWN
            } else {
                code::AUTHORITY_LOST
            }
        })
}

/// `read_capture_image` (§4.9) after the envelope checks.
pub(super) async fn read(
    link: &DeckLink,
    station: &StationId,
    (caller, binding): (SessionId, CoordinatorBinding),
    args: ImageArguments,
    started: Instant,
) -> Outcome {
    let deadline = started + Duration::from_millis(deadline_ms::IMAGE_DECK);
    let artifact = args.artifact;
    // 1. The artifact's station must be a station of the grant.
    let auth = match authorize(
        link,
        station,
        &caller,
        &binding,
        Tool::ReadCaptureImage,
        Some(artifact.station_id.as_str()),
    )
    .await
    {
        Ok(auth) => auth,
        Err(refusal) => return reply_err(refusal, None),
    };
    let mut row = Row {
        link,
        auth: &auth,
        capture_id: None,
        artifact_id: None,
        reserved: false,
    };
    // 2. Only version 1 exists.
    if artifact.version != 1 {
        return row.refuse(code::UNKNOWN_ARTIFACT).await;
    }
    // 3. The coordinator capture of this artifact, of this caller.
    let found = link
        .store
        .coordinator_capture_by_artifact(artifact.station_id.clone(), artifact.artifact_id.clone())
        .await;
    let capture = match found {
        Ok(Some(c)) if is_callers(&c, auth.ctx()) => c,
        Ok(_) => return row.refuse(code::UNKNOWN_ARTIFACT).await,
        Err(e) => return row.refuse(store_error(&e)).await,
    };
    row.capture_id = Some(capture.capture_id.clone());
    row.artifact_id = Some(artifact.artifact_id.clone());
    let Some(published) = capture.published.as_ref() else {
        return row.refuse(code::UNKNOWN_ARTIFACT).await;
    };
    let (size, sha256) = (published.version.size, published.version.sha256.as_str());
    // 4. A known oversized artifact is refused before any reservation.
    if size > limits::IMAGE_MAX_BYTES {
        return row.refuse(code::IMAGE_TOO_LARGE).await;
    }
    // 5. The live policies, with the monitor of the capture.
    let ctx = auth.ctx();
    if let Err(refusal) = caller_policy(
        link,
        &ctx.caller_station,
        &ctx.caller_session,
        &binding,
        deadline,
    )
    .await
    {
        return row.refuse(refusal).await;
    }
    let monitor = Some(capture.monitor.as_str());
    if let Err(refusal) = target_policy(
        link,
        auth.route(),
        PolicyPurpose::Capture,
        monitor,
        deadline,
    )
    .await
    {
        return row.refuse(refusal).await;
    }
    // 6. The slots; released on every path (a drop guard).
    let Some(_slots) = link.coordinator_desktop.slots().take_image(&ctx.grant_id) else {
        return row.refuse(code::BUSY).await;
    };
    // 7. The reservation: never refunded.
    match link
        .store
        .desktop_image_reserve(&ctx.grant_id, link.store.now_ms())
        .await
    {
        Ok(Ok(())) => row.reserved = true,
        Ok(Err(refusal)) => return row.refuse(refusal).await,
        Err(e) => return row.refuse(store_error(&e)).await,
    }
    // 8. The protected read of the whole file, and the checks of the bytes.
    let left = deadline.saturating_duration_since(Instant::now());
    let wait = left.min(Duration::from_millis(deadline_ms::IMAGE_ACQUISITION));
    let wait_ms = u64::try_from(wait.as_millis()).unwrap_or(0);
    if wait_ms == 0 {
        return row.fail(code::DESKTOP_READ_TIMEOUT).await;
    }
    let request = ProtectedRead::CaptureFile {
        artifact_id: artifact.artifact_id.clone(),
        version: 1,
        offset: 0,
        length: size,
    };
    let value = match coordinator_read(link, &auth, request, wait_ms).await {
        Ok(value) => value,
        Err(failure) => return row.fail(failure).await,
    };
    let png_base64 = match check_file(value, size, sha256) {
        Ok(text) => text,
        Err(failure) => return row.fail(failure).await,
    };
    // 9. The tombstone and the authority, after the awaited work.
    if let Err(failure) = recheck(link, &auth, &artifact).await {
        return row.fail(failure).await;
    }
    // 10. The answer, if its whole serialized frame fits.
    let meta = CaptureImageMeta {
        artifact: ArtifactRef {
            station_id: artifact.station_id.clone(),
            artifact_id: artifact.artifact_id.clone(),
            version: 1,
        },
        capture_id: capture.capture_id.clone(),
        monitor: capture.monitor.clone(),
        at: published.version.created_at,
        size,
        sha256: sha256.to_owned(),
    };
    match answer(meta, png_base64) {
        Ok(outcome) => {
            row.write(audit::OUTCOME, AuditResult::Ok, None).await;
            outcome
        }
        Err(failure) => row.fail(failure).await,
    }
}

#[cfg(test)]
#[path = "image_tests.rs"]
mod tests;
