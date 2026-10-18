//! The read tools `desktop_info` and `desktop_action_status` (R13 P6 §4.1,
//! §4.8, decision O12-9 items 9 to 11). Both run after the authorization and
//! the query rate. `desktop_info` asks the live policy of both stations and
//! then reads fresh data through a protected read; `desktop_action_status`
//! reads only the deck's own operation ledger.

use std::time::{Duration, Instant};

use fleetdeck_proto::coordinator_desktop::{
    ActionState, CoordinatorDesktopInfo, DesktopActionStatus, DesktopInfoArguments, StatusArguments,
};
use fleetdeck_proto::coordinator_desktop_text::{code, deadline_ms, limits, text};
use fleetdeck_proto::coordinator_desktop_wire::{CoordinatorContext, PolicyPurpose, ProtectedRead};
use fleetdeck_proto::link::Outcome;
use fleetdeck_proto::mutation::CoordinatorBinding;
use fleetdeck_proto::{SessionId, StationId};
use serde_json::{Value, json};

use super::audit::{self, Facts};
use super::authorize::{Authorized, Tool, authorize, begin, store_error};
use super::input;
use super::policy::{caller_policy, target_policy};
use super::reads::coordinator_read;
use super::{reply_err, reply_ok};
use crate::link::DeckLink;
use crate::store::AuditResult;
use crate::store::coordinator_desktop::{DesktopOp, OpState, OpTool, request_key};

/// The status of an operation row (§4.8). The raw `request_id` is the one that
/// the caller sent now; the row holds only its hash.
pub(super) fn status_of(request_id: &str, op: &DesktopOp) -> DesktopActionStatus {
    let mut status = DesktopActionStatus {
        request_id: request_id.to_owned(),
        state: ActionState::Pending,
        operation_id: Some(op.operation_id.clone()),
        station: Some(op.target_station.to_string()),
        result: None,
        error: None,
        message: None,
    };
    match op.state {
        OpState::Prepared | OpState::Attempted => {}
        OpState::Done => {
            status.state = ActionState::Done;
            status.result = Some(op.result.clone().unwrap_or_else(|| json!({})));
        }
        OpState::Failed => {
            let error = op.code.as_deref().unwrap_or(code::DESKTOP_FAILED);
            status.state = ActionState::Failed;
            status.error = Some(error.to_owned());
            status.message = text(error).map(str::to_owned);
        }
        OpState::Unknown => status.state = ActionState::Unknown,
    }
    status
}

/// The status of a request that no operation row holds: `unknown`, or the
/// stored budget refusal (`failed`, no operation ID).
fn status_without_op(request_id: &str, refusal: Option<&str>) -> DesktopActionStatus {
    let mut status = DesktopActionStatus {
        request_id: request_id.to_owned(),
        state: ActionState::Unknown,
        operation_id: None,
        station: None,
        result: None,
        error: None,
        message: None,
    };
    if let Some(error) = refusal {
        status.state = ActionState::Failed;
        status.error = Some(error.to_owned());
        status.message = text(error).map(str::to_owned);
    }
    status
}

fn to_reply(status: &DesktopActionStatus) -> Outcome {
    match serde_json::to_value(status) {
        Ok(v) => reply_ok(v),
        Err(_) => reply_err(code::DESKTOP_FAILED, None),
    }
}

/// The audit row of a query, after the authorization passed.
async fn audit_query(
    link: &DeckLink,
    (action, ctx, target): (&'static str, &CoordinatorContext, &StationId),
    (stage, result, failure): (&'static str, AuditResult, Option<&str>),
) {
    let facts = Facts {
        action,
        ctx,
        target,
        operation_id: None,
        window_id: None,
        monitor: None,
    };
    audit::write_or_log(&link.store, facts.row(stage, result, failure, None)).await;
}

/// `desktop_action_status` (§4.8) after the envelope checks.
pub(super) async fn status(
    link: &DeckLink,
    station: &StationId,
    (caller, binding): (SessionId, CoordinatorBinding),
    args: StatusArguments,
) -> Outcome {
    const ACTION: &str = "desktop.action_status";
    // No target lookup: the authorization stops at the grant and the class.
    let begun = match begin(
        link,
        (station, &caller),
        &binding,
        Tool::DesktopActionStatus,
        None,
    )
    .await
    {
        Ok(begun) => begun,
        Err(refusal) => return reply_err(refusal, None),
    };
    let who = (ACTION, &begun.ctx, &begun.target);
    match link
        .store
        .desktop_query_admit(&begun.ctx.grant_id, link.store.now_ms())
        .await
    {
        Ok(Ok(())) => {}
        Ok(Err(refusal)) => {
            audit_query(
                link,
                who,
                (audit::REFUSED, AuditResult::Refused, Some(refusal)),
            )
            .await;
            return reply_err(refusal, None);
        }
        Err(e) => {
            let refusal = store_error(&e);
            audit_query(
                link,
                who,
                (audit::REFUSED, AuditResult::Refused, Some(refusal)),
            )
            .await;
            return reply_err(refusal, None);
        }
    }
    let key = request_key(&args.request_id);
    let found = match link
        .store
        .desktop_op_by_key(&begun.ctx.grant_id, &key)
        .await
    {
        // A screenshot row and a recording row are invisible to this tool.
        Ok(Some(op)) if matches!(op.tool, OpTool::Screenshot | OpTool::RecordMonitor) => {
            Ok(status_without_op(&args.request_id, None))
        }
        Ok(Some(op)) if input::is_input(op.tool) => Ok(input::status_of(&args.request_id, &op)),
        Ok(Some(op)) => Ok(status_of(&args.request_id, &op)),
        Ok(None) => match link
            .store
            .desktop_reject_by_key(&begun.ctx.grant_id, &key)
            .await
        {
            Ok(stored) => Ok(status_without_op(
                &args.request_id,
                stored.as_ref().map(|(_, c)| c.as_str()),
            )),
            Err(e) => Err(store_error(&e)),
        },
        Err(e) => Err(store_error(&e)),
    };
    match found {
        Ok(status) => {
            audit_query(link, who, (audit::OUTCOME, AuditResult::Ok, None)).await;
            to_reply(&status)
        }
        Err(failure) => {
            audit_query(
                link,
                who,
                (audit::OUTCOME, AuditResult::Failed, Some(failure)),
            )
            .await;
            reply_err(failure, None)
        }
    }
}

/// Filters a `desktop_info` result for the classes of the grant and enforces
/// the limits of §4.1 again.
fn shape_info(
    mut info: CoordinatorDesktopInfo,
    target: &StationId,
    (desktop, capture): (bool, bool),
) -> Result<Value, &'static str> {
    info.station = target.to_string();
    if !desktop {
        info.monitors.retain(|m| m.capture_allowed);
        info.walls.clear();
        info.capabilities.notify = false;
        info.capabilities.preview = false;
    }
    if !capture {
        info.capabilities.screenshot = false;
    }
    let too_large = info.monitors.len() > limits::INFO_MAX_MONITORS as usize
        || info.walls.len() > limits::INFO_MAX_WALLS as usize;
    let value = serde_json::to_value(&info).map_err(|_| code::DESKTOP_FAILED)?;
    let size = u64::try_from(value.to_string().len()).unwrap_or(u64::MAX);
    if too_large || size > limits::INFO_MAX_BYTES {
        return Err(code::DESKTOP_RESULT_TOO_LARGE);
    }
    Ok(value)
}

/// The two policies, then the protected read of the fresh information.
async fn fetch_info(
    link: &DeckLink,
    auth: &Authorized,
    binding: &CoordinatorBinding,
    deadline: Instant,
) -> Result<Value, (&'static str, bool)> {
    let ctx = auth.ctx();
    let purpose = match auth.class() {
        crate::store::coordinator_desktop::OpClass::Desktop => PolicyPurpose::Desktop,
        crate::store::coordinator_desktop::OpClass::Capture => PolicyPurpose::Capture,
        crate::store::coordinator_desktop::OpClass::Input => PolicyPurpose::Input,
    };
    // `false`: the station was not asked for data yet (a refusal).
    caller_policy(
        link,
        &ctx.caller_station,
        &ctx.caller_session,
        binding,
        deadline,
    )
    .await
    .map_err(|c| (c, false))?;
    target_policy(link, auth.route(), purpose, None, deadline)
        .await
        .map_err(|c| (c, false))?;
    let left = deadline.saturating_duration_since(Instant::now());
    let ms = u64::try_from(
        left.min(Duration::from_millis(deadline_ms::INFO_DECK))
            .as_millis(),
    )
    .unwrap_or(0);
    let value = coordinator_read(link, auth, ProtectedRead::Info, ms)
        .await
        .map_err(|c| (c, true))?;
    let info: CoordinatorDesktopInfo =
        serde_json::from_value(value).map_err(|_| (code::DESKTOP_FAILED, true))?;
    let grant = auth.grant();
    use crate::store::GrantClass;
    shape_info(
        info,
        auth.target(),
        (
            grant.classes.contains(&GrantClass::Desktop),
            grant.classes.contains(&GrantClass::Capture),
        ),
    )
    .map_err(|c| (c, true))
}

/// `desktop_info` (§4.1) after the envelope checks.
pub(super) async fn info(
    link: &DeckLink,
    station: &StationId,
    (caller, binding): (SessionId, CoordinatorBinding),
    args: DesktopInfoArguments,
    started: Instant,
) -> Outcome {
    const ACTION: &str = "desktop.info";
    let deadline = started + Duration::from_millis(deadline_ms::INFO_DECK);
    let auth = match authorize(
        link,
        station,
        &caller,
        &binding,
        Tool::DesktopInfo,
        args.station.as_deref(),
    )
    .await
    {
        Ok(auth) => auth,
        Err(refusal) => return reply_err(refusal, None),
    };
    let who = (ACTION, auth.ctx(), auth.target());
    let admitted = link
        .store
        .desktop_query_admit(&auth.ctx().grant_id, link.store.now_ms())
        .await
        .map_err(|e| store_error(&e));
    let refusal = match admitted {
        Ok(Ok(())) => None,
        Ok(Err(refusal)) | Err(refusal) => Some(refusal),
    };
    if let Some(refusal) = refusal {
        audit_query(
            link,
            who,
            (audit::REFUSED, AuditResult::Refused, Some(refusal)),
        )
        .await;
        return reply_err(refusal, None);
    }
    match fetch_info(link, &auth, &binding, deadline).await {
        Ok(value) => {
            audit_query(link, who, (audit::OUTCOME, AuditResult::Ok, None)).await;
            reply_ok(value)
        }
        Err((failure, asked)) => {
            let (stage, result) = if asked {
                (audit::OUTCOME, AuditResult::Failed)
            } else {
                (audit::REFUSED, AuditResult::Refused)
            };
            audit_query(link, who, (stage, result, Some(failure))).await;
            reply_err(failure, None)
        }
    }
}

#[cfg(test)]
#[path = "queries_tests.rs"]
mod tests;
