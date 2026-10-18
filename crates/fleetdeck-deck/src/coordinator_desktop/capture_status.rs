//! `capture_status` and the shape of a capture state (R13 P6 §4.8, §7.4,
//! decision O12-9 items 9 and 10, task T3 items 6).
//!
//! The state of a screenshot comes from its deck capture record
//! (`desktop_captures`, row id = the operation ID). A request that no
//! screenshot operation of this grant holds is `unknown`, and a request of
//! another kind or of another grant is the same `unknown`. The query asks the
//! station once (a protected read) only while the deck state is not final,
//! and that answer never turns a saved capture into a failure.

use std::time::{Duration, Instant};

use fleetdeck_proto::SessionId;
use fleetdeck_proto::StationId;
use fleetdeck_proto::artifacts::ArtifactPublished;
use fleetdeck_proto::capture::{self, CaptureStatus};
use fleetdeck_proto::coordinator_desktop::{
    CaptureState as State, CoordinatorCaptureStatus, StatusArguments,
};
use fleetdeck_proto::coordinator_desktop_text::{code, deadline_ms, text};
use fleetdeck_proto::coordinator_desktop_wire::{CoordinatorContext, ProtectedRead};
use fleetdeck_proto::display_directive::ArtifactRef;
use fleetdeck_proto::link::Outcome;
use fleetdeck_proto::mutation::CoordinatorBinding;
use serde_json::Value;

use super::audit::{self, Facts};
use super::authorize::{Tool, authorize, begin, store_error};
use super::reads::coordinator_read;
use super::{known_code, reply_err, reply_ok};
use crate::desktop_capture::CaptureError;
use crate::link::DeckLink;
use crate::store::AuditResult;
use crate::store::coordinator_desktop::{DesktopOp, OpState, OpTool, request_key};
use crate::store::desktop_captures::{CaptureRow, CaptureState};

/// What `pending` says when the station saved the screenshot and the deck
/// has not indexed it yet (§11.3).
pub(super) const PENDING_INDEXING: &str =
    "The screenshot was saved on the station. The artifact list is still updating.";

/// A state without a result: `pending`, `unknown` or `deleted` of a capture.
/// `unknown` carries the fixed text of §11.3.
pub(super) fn plain(
    request_id: &str,
    station: &StationId,
    capture_id: &str,
    state: State,
    message: Option<&str>,
) -> CoordinatorCaptureStatus {
    CoordinatorCaptureStatus {
        request_id: request_id.to_owned(),
        state,
        capture_id: Some(capture_id.to_owned()),
        station: Some(station.to_string()),
        artifact: None,
        monitor: None,
        size: None,
        sha256: None,
        at: None,
        error: None,
        message: message.map(str::to_owned),
    }
}

/// The `committed` state of an indexed capture: the artifact reference and
/// the allowlisted metadata of its version.
pub(super) fn committed(
    request_id: &str,
    station: &StationId,
    row: (&str, &str),
    published: &ArtifactPublished,
) -> CoordinatorCaptureStatus {
    let (capture_id, monitor) = row;
    let mut status = plain(request_id, station, capture_id, State::Committed, None);
    status.artifact = Some(ArtifactRef {
        station_id: station.clone(),
        artifact_id: published.artifact.artifact_id.clone(),
        version: 1,
    });
    status.monitor = Some(monitor.to_owned());
    status.size = Some(published.version.size);
    status.sha256 = Some(published.version.sha256.clone());
    status.at = Some(published.version.created_at);
    status
}

/// The `failed` state: the public code and its fixed text.
fn failed(
    request_id: &str,
    station: &StationId,
    capture_id: &str,
    failure: (&str, &str),
) -> CoordinatorCaptureStatus {
    let mut status = plain(request_id, station, capture_id, State::Failed, None);
    status.error = Some(failure.0.to_owned());
    status.message = Some(failure.1.to_owned());
    status
}

/// The public code and the fixed text of a stored failure: a capture code
/// keeps its capture text (and the missing `grim` its own), a code of §11
/// keeps the text of §11, and anything else is `capture_failed`.
pub(super) fn failure_of(stored: Option<&str>, message: Option<&str>) -> (String, String) {
    let stored = stored.unwrap_or(capture::code::CAPTURE_FAILED);
    let message = message.unwrap_or(capture::text::CAPTURE_FAILED);
    if let Some((e, _, _)) = CaptureError::from_station(stored, message) {
        let (_, public, fixed) = e.http();
        return (public.to_owned(), fixed.to_owned());
    }
    if let Some(known) = known_code(stored) {
        return (known.to_owned(), text(known).unwrap_or_default().to_owned());
    }
    (
        capture::code::CAPTURE_FAILED.to_owned(),
        capture::text::CAPTURE_FAILED.to_owned(),
    )
}

/// The state of `op` for the caller. `row` is the deck capture record of the
/// operation. A missing record is read from the operation itself (a screenshot
/// that stopped before its record was written).
pub(super) fn state_of(
    request_id: &str,
    op: &DesktopOp,
    row: Option<&CaptureRow>,
) -> CoordinatorCaptureStatus {
    let station = &op.target_station;
    let id = op.operation_id.as_str();
    let Some(row) = row else {
        return match op.state {
            OpState::Failed => {
                let c = op.code.as_deref().unwrap_or(code::DESKTOP_FAILED);
                failed(request_id, station, id, (c, text(c).unwrap_or_default()))
            }
            OpState::Unknown => plain(
                request_id,
                station,
                id,
                State::Unknown,
                Some(capture::text::UNKNOWN),
            ),
            _ => plain(request_id, station, id, State::Pending, None),
        };
    };
    if row.deleted {
        return plain(request_id, station, id, State::Deleted, None);
    }
    match (row.state, row.indexed, &row.published) {
        (CaptureState::Committed, true, Some(published)) => {
            committed(request_id, station, (id, row.monitor.as_str()), published)
        }
        (CaptureState::Failed, _, _) => {
            let (error, message) = failure_of(row.code.as_deref(), row.message.as_deref());
            failed(request_id, station, id, (&error, &message))
        }
        (CaptureState::Unknown, _, _) => plain(
            request_id,
            station,
            id,
            State::Unknown,
            Some(capture::text::UNKNOWN),
        ),
        (CaptureState::Committed, _, _) => plain(
            request_id,
            station,
            id,
            State::Pending,
            Some(PENDING_INDEXING),
        ),
        _ => plain(request_id, station, id, State::Pending, None),
    }
}

fn to_reply(status: &CoordinatorCaptureStatus) -> Outcome {
    match serde_json::to_value(status) {
        Ok(v) => reply_ok(v),
        Err(_) => reply_err(code::DESKTOP_FAILED, None),
    }
}

/// The reply of a duplicate screenshot request (§6.2): the state of the
/// capture record, read on the deck only.
pub(super) async fn replay(link: &DeckLink, request_id: &str, op: &DesktopOp) -> Outcome {
    match link
        .store
        .capture_get(op.target_station.clone(), op.operation_id.clone())
        .await
    {
        Ok(row) => to_reply(&state_of(request_id, op, row.as_ref())),
        Err(e) => reply_err(store_error(&e), None),
    }
}

/// The unknown request: the request ID only.
fn unknown(request_id: &str) -> CoordinatorCaptureStatus {
    CoordinatorCaptureStatus {
        request_id: request_id.to_owned(),
        state: State::Unknown,
        capture_id: None,
        station: None,
        artifact: None,
        monitor: None,
        size: None,
        sha256: None,
        at: None,
        error: None,
        message: None,
    }
}

/// The audit row of one query, after the authorization passed.
async fn audit_query(
    link: &DeckLink,
    (ctx, target): (&CoordinatorContext, &StationId),
    operation_id: Option<&str>,
    (stage, result, failure): (&'static str, AuditResult, Option<&str>),
) {
    let facts = Facts {
        action: "desktop.capture_status",
        ctx,
        target,
        operation_id,
        window_id: None,
        monitor: None,
    };
    audit::write_or_log(&link.store, facts.row(stage, result, failure, None)).await;
}

/// Whether the deck may still learn something from the station: the state is
/// `pending`.
fn is_pending(status: &CoordinatorCaptureStatus) -> bool {
    status.state == State::Pending
}

/// One protected read of the capture status of `op`, when the deck state is
/// not final. A station answer `committed` with no deck index is `pending`
/// (with its text), never `failed`; any failure of the read keeps the deck
/// state. Authorization runs for the target of the operation first: a refusal
/// that is not a missing link is answered.
async fn ask_station(
    link: &DeckLink,
    (station, caller, binding): (&StationId, &SessionId, &CoordinatorBinding),
    op: &DesktopOp,
    mut status: CoordinatorCaptureStatus,
    deadline: Instant,
) -> Result<CoordinatorCaptureStatus, &'static str> {
    let auth = match authorize(
        link,
        station,
        caller,
        binding,
        Tool::CaptureStatus,
        Some(op.target_station.as_str()),
    )
    .await
    {
        Ok(auth) => auth,
        // No link to the target: the deck state stays.
        Err(code::STATION_UNREACHABLE | code::DESKTOP_UNSUPPORTED) => return Ok(status),
        Err(refusal) => return Err(refusal),
    };
    let left = deadline.saturating_duration_since(Instant::now());
    let ms = u64::try_from(
        left.min(Duration::from_millis(deadline_ms::CAPTURE_LOOKUP_DECK))
            .as_millis(),
    )
    .unwrap_or(0);
    if ms == 0 {
        return Ok(status);
    }
    let read = ProtectedRead::CaptureStatus {
        capture_id: op.operation_id.clone(),
    };
    let Ok(value) = coordinator_read(link, &auth, read, ms).await else {
        return Ok(status);
    };
    // A parse error is not logged: the answer could hold desktop data.
    if let Ok(CaptureStatus::Committed { capture_id, .. }) = serde_json::from_value(value)
        && capture_id == op.operation_id
    {
        status.message = Some(PENDING_INDEXING.to_owned());
    }
    Ok(status)
}

/// `capture_status` (§4.8) after the envelope checks.
pub(super) async fn status(
    link: &DeckLink,
    station: &StationId,
    (caller, binding): (SessionId, CoordinatorBinding),
    args: StatusArguments,
    started: Instant,
) -> Outcome {
    let deadline = started + Duration::from_millis(deadline_ms::STATUS_DECK);
    // No station argument: the target is the one of the operation.
    let begun = match begin(
        link,
        (station, &caller),
        &binding,
        Tool::CaptureStatus,
        None,
    )
    .await
    {
        Ok(begun) => begun,
        Err(refusal) => return reply_err(refusal, None),
    };
    let who = (&begun.ctx, &begun.target);
    let admitted = link
        .store
        .desktop_query_admit(&begun.ctx.grant_id, link.store.now_ms())
        .await
        .map_err(|e| store_error(&e));
    if let Ok(Err(refusal)) | Err(refusal) = admitted {
        let stage = (audit::REFUSED, AuditResult::Refused, Some(refusal));
        audit_query(link, who, None, stage).await;
        return reply_err(refusal, None);
    }
    let key = request_key(&args.request_id);
    let op = match link
        .store
        .desktop_op_by_key(&begun.ctx.grant_id, &key)
        .await
    {
        // A request of another kind is the same as no request.
        Ok(op) => op.filter(|op| op.tool == OpTool::Screenshot),
        Err(e) => return failed_query(link, who, store_error(&e)).await,
    };
    let Some(op) = op else {
        audit_query(link, who, None, (audit::OUTCOME, AuditResult::Ok, None)).await;
        return to_reply(&unknown(&args.request_id));
    };
    let row = match link
        .store
        .capture_get(op.target_station.clone(), op.operation_id.clone())
        .await
    {
        Ok(row) => row,
        Err(e) => return failed_query(link, who, store_error(&e)).await,
    };
    let mut state = state_of(&args.request_id, &op, row.as_ref());
    if is_pending(&state) && row.is_some() {
        state = match ask_station(link, (station, &caller, &binding), &op, state, deadline).await {
            Ok(state) => state,
            Err(refusal) => return failed_query(link, who, refusal).await,
        };
    }
    let id = Some(op.operation_id.as_str());
    audit_query(link, who, id, (audit::OUTCOME, AuditResult::Ok, None)).await;
    to_reply(&state)
}

async fn failed_query(
    link: &DeckLink,
    who: (&CoordinatorContext, &StationId),
    failure: &'static str,
) -> Outcome {
    let stage = (audit::OUTCOME, AuditResult::Failed, Some(failure));
    audit_query(link, who, None, stage).await;
    reply_err(failure, None)
}

/// A status as the JSON that the operation row keeps: without the raw
/// request ID (the ledger never stores it, O12-4 item 14).
pub(super) fn stored_result(status: &CoordinatorCaptureStatus) -> Value {
    let mut value = serde_json::to_value(status).unwrap_or(Value::Null);
    if let Some(object) = value.as_object_mut() {
        object.remove("request_id");
    }
    value
}

#[cfg(test)]
#[path = "capture_status_tests.rs"]
mod tests;
