//! The mutating tools `notify`, `preview_open`, `window_close` and
//! `screenshot` (R13 P6 §4.3 to §4.6, §6.2, §7, decision O12-9 items 3 to 8;
//! the screenshot send is in [`super::capture`]). One call: authorize,
//! build the typed action, the caller and target policies, the one admission
//! transaction, the audit row of the attempt, the send ([`super::relay`]), the
//! audit row of the outcome, the reply.

use std::time::{Duration, Instant};

use fleetdeck_link::desktop_authority::action_hash_for;
use fleetdeck_proto::coordinator_desktop::{CoordUrgency, DesktopCall};
use fleetdeck_proto::coordinator_desktop_text::{code, deadline_ms};
use fleetdeck_proto::coordinator_desktop_wire::{CoordinatorDesktopCallEnvelope, PolicyPurpose};
use fleetdeck_proto::desktop::{DesktopAction, Urgency};
use fleetdeck_proto::desktop_text;
use fleetdeck_proto::link::Outcome;
use fleetdeck_proto::mutation::CoordinatorBinding;
use fleetdeck_proto::{SessionId, StationId};
use serde_json::{Value, json};

use super::audit::{self, Facts};
use super::authorize::{Authorized, Tool, authorize, store_error, switches};
use super::policy::{caller_policy, target_policy};
use super::queries::status_of;
use super::relay::{self, Ran};
use super::{capture, capture_status, input, input_run, record, wall};
use super::{reply_err, reply_err_text, reply_ok};
use crate::link::DeckLink;
use crate::store::AuditResult;
use crate::store::coordinator_desktop::{
    Admission, DesktopOp, NewOp, OpOutcome, OpTool, request_key,
};

/// The lowercase hex SHA-256 of `text`.
fn sha256_hex(text: &str) -> String {
    ring::digest::digest(&ring::digest::SHA256, text.as_bytes())
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The text of `capture_cap_reached` for a recording (R13 P5b-2 §16).
const RECORDING_CAPTURE_LIMIT: &str = "This grant has used its capture limit.";

/// The checked call: the typed action and what the operation row and the audit
/// row may hold.
pub(super) struct Built {
    pub(super) op_tool: OpTool,
    pub(super) action: DesktopAction,
    pub(super) request_id: String,
    /// The validated arguments as JSON (`serde_json` writes the keys sorted).
    pub(super) args: Value,
    pub(super) resource: Value,
    /// The validated window handle of a close or an input. A row of an input
    /// call never names it, except the outcome row of a `done` operation
    /// (R13 P7 §12).
    pub(super) window_id: Option<String>,
    pub(super) monitor: Option<String>,
    /// The requested seconds of a recording (R13 P5b-2).
    pub(super) seconds: Option<u32>,
    /// The length of an input call (R13 P7 §3.5).
    pub(super) input_length: Option<u32>,
}

/// The handler deadline of a tool (§9).
fn handler_ms(tool: Tool) -> u64 {
    if tool == Tool::PreviewOpen {
        deadline_ms::PREVIEW_DECK_HANDLER
    } else {
        deadline_ms::ACTION_DECK_HANDLER
    }
}

/// The handler deadline of a call: a recording has its `seconds` plus the
/// handler time of the other actions (R13 P5b-2 §10); any other call has the
/// deadline of its tool.
fn call_handler_ms(tool: Tool, call: &DesktopCall) -> u64 {
    match call {
        DesktopCall::RecordMonitor(a) => {
            u64::from(a.seconds) * 1_000 + deadline_ms::ACTION_DECK_HANDLER
        }
        _ => handler_ms(tool),
    }
}

/// The tool and the target argument of a call. Every tool of
/// `coordinator.desktop_call` is served (`wall_show` by [`super::wall`],
/// `record_monitor` by [`super::record`], the input tools by
/// [`super::input_run`]).
fn shape(call: &DesktopCall) -> Option<(Tool, Option<&str>)> {
    match call {
        DesktopCall::Notify(a) => Some((Tool::Notify, a.station.as_deref())),
        DesktopCall::PreviewOpen(a) => Some((Tool::PreviewOpen, a.station.as_deref())),
        DesktopCall::WindowClose(a) => Some((Tool::WindowClose, a.station.as_deref())),
        DesktopCall::Screenshot(a) => Some((Tool::Screenshot, a.station.as_deref())),
        DesktopCall::WallShow(a) => Some((Tool::WallShow, a.station.as_deref())),
        DesktopCall::RecordMonitor(a) => Some((Tool::RecordMonitor, a.station.as_deref())),
        DesktopCall::KeyPress(a) => Some((Tool::KeyPress, a.station.as_deref())),
        DesktopCall::TypeText(a) => Some((Tool::TypeText, a.station.as_deref())),
        DesktopCall::PointerMove(a) => Some((Tool::PointerMove, a.station.as_deref())),
        // R13 P7b task F stub: `pointer_click` is served by a later task.
        DesktopCall::PointerClick(_) => None,
    }
}

/// Whether the call has a handler.
pub(super) fn served(call: &DesktopCall) -> bool {
    shape(call).is_some()
}

/// Builds the typed action of §4.3 to §4.5 from the validated arguments.
/// `invalid_params` when the text or the URL does not pass the shared rules.
fn build(
    call: &DesktopCall,
    label: &str,
    caller_session: &SessionId,
) -> Result<Built, &'static str> {
    let invalid = |_| code::INVALID_PARAMS;
    match call {
        DesktopCall::Notify(a) => {
            // The deck builds the final prefixed and truncated title (O12-4).
            let title = desktop_text::title(&format!("{label}: {}", a.title))
                .ok_or(code::INVALID_PARAMS)?;
            let urgency = match a.urgency {
                CoordUrgency::Low => Urgency::Low,
                CoordUrgency::Normal => Urgency::Normal,
            };
            Ok(Built {
                op_tool: OpTool::Notify,
                action: DesktopAction::Notify {
                    title,
                    body: desktop_text::body(&a.body),
                    urgency,
                },
                request_id: a.request_id.clone(),
                args: serde_json::to_value(a).map_err(invalid)?,
                resource: json!({}),
                window_id: None,
                monitor: None,
                seconds: None,
                input_length: None,
            })
        }
        DesktopCall::PreviewOpen(a) => {
            let url = desktop_text::url(&a.url).ok_or(code::INVALID_PARAMS)?;
            Ok(Built {
                op_tool: OpTool::PreviewOpen,
                action: DesktopAction::BrowserOpen {
                    url: url.to_owned(),
                    monitor: a.monitor.clone(),
                    session: Some(caller_session.clone()),
                },
                request_id: a.request_id.clone(),
                args: serde_json::to_value(a).map_err(invalid)?,
                resource: a
                    .monitor
                    .as_ref()
                    .map_or_else(|| json!({}), |m| json!({ "monitor": m })),
                window_id: None,
                monitor: a.monitor.clone(),
                seconds: None,
                input_length: None,
            })
        }
        DesktopCall::WindowClose(a) => Ok(Built {
            op_tool: OpTool::WindowClose,
            action: DesktopAction::PreviewClose {
                window_id: a.window_id.clone(),
            },
            request_id: a.request_id.clone(),
            args: serde_json::to_value(a).map_err(invalid)?,
            resource: json!({ "window_id": a.window_id }),
            window_id: Some(a.window_id.clone()),
            monitor: None,
            seconds: None,
            input_length: None,
        }),
        DesktopCall::Screenshot(a) => Ok(Built {
            op_tool: OpTool::Screenshot,
            action: DesktopAction::ScreenCapture {
                monitor: a.monitor.clone(),
            },
            request_id: a.request_id.clone(),
            args: serde_json::to_value(a).map_err(invalid)?,
            resource: json!({ "monitor": a.monitor }),
            window_id: None,
            monitor: Some(a.monitor.clone()),
            seconds: None,
            input_length: None,
        }),
        DesktopCall::RecordMonitor(a) => Ok(Built {
            op_tool: OpTool::RecordMonitor,
            action: DesktopAction::ScreenRecord {
                monitor: a.monitor.clone(),
                seconds: a.seconds,
            },
            request_id: a.request_id.clone(),
            args: serde_json::to_value(a).map_err(invalid)?,
            resource: json!({ "monitor": a.monitor, "seconds": a.seconds }),
            window_id: None,
            monitor: Some(a.monitor.clone()),
            seconds: Some(a.seconds),
            input_length: None,
        }),
        DesktopCall::KeyPress(_) | DesktopCall::TypeText(_) | DesktopCall::PointerMove(_) => {
            input::build(call).unwrap_or(Err(code::DESKTOP_UNSUPPORTED))
        }
        DesktopCall::WallShow(_) | DesktopCall::PointerClick(_) => Err(code::DESKTOP_UNSUPPORTED),
    }
}

/// The payload hash of §7.1: the lowercase hex SHA-256 of the compact JSON of
/// the validated arguments without `request_id`, with `station` set to the
/// target.
pub(super) fn payload_hash(mut args: Value, target: &StationId) -> String {
    if let Some(object) = args.as_object_mut() {
        object.remove("request_id");
        object.insert("station".to_owned(), json!(target));
    }
    sha256_hex(&args.to_string())
}

/// The audit action of a tool.
fn action_name(tool: Tool) -> &'static str {
    match tool {
        Tool::Notify => "desktop.notify",
        Tool::PreviewOpen => "desktop.preview_open",
        Tool::Screenshot | Tool::RecordMonitor => audit::CAPTURE,
        Tool::KeyPress => input::KEY_PRESS,
        Tool::TypeText => input::TYPE_TEXT,
        Tool::PointerMove => input::POINTER_MOVE,
        _ => "desktop.window_close",
    }
}

/// One call after its authorization.
struct Call<'a> {
    link: &'a DeckLink,
    auth: &'a Authorized,
    tool: Tool,
    /// `None` when the arguments were refused: a refusal row names no
    /// supplied identifier.
    built: Option<&'a Built>,
}

impl Call<'_> {
    fn facts<'f>(&'f self, operation_id: Option<&'f str>) -> Facts<'f> {
        Facts {
            action: action_name(self.tool),
            ctx: self.auth.ctx(),
            target: self.auth.target(),
            operation_id,
            // A row of an input call names no supplied handle (R13 P7 §12).
            window_id: self
                .built
                .and_then(|b| b.window_id.as_deref())
                .filter(|_| !input::is_input_tool(self.tool)),
            monitor: self.built.and_then(|b| b.monitor.as_deref()),
        }
    }

    /// A refusal after the authorization: its audit row and its reply.
    async fn refuse(&self, refusal: &'static str) -> Outcome {
        let row = self
            .facts(None)
            .row(audit::REFUSED, AuditResult::Refused, Some(refusal), None);
        audit::write_or_log(&self.link.store, row).await;
        // R13 P5b-2 §16: a recording that finds the shared capture allowance
        // used has its own text; the screenshot text does not change.
        if self.tool == Tool::RecordMonitor && refusal == code::CAPTURE_CAP_REACHED {
            return reply_err_text(refusal, RECORDING_CAPTURE_LIMIT);
        }
        reply_err(refusal, None)
    }

    /// The caller policy and the target policy, one after the other, with the
    /// time that is left.
    async fn policies(
        &self,
        binding: &CoordinatorBinding,
        deadline: Instant,
    ) -> Result<(), &'static str> {
        let ctx = self.auth.ctx();
        caller_policy(
            self.link,
            &ctx.caller_station,
            &ctx.caller_session,
            binding,
            deadline,
        )
        .await?;
        // Only a screenshot and a recording ask for the capture purpose and a
        // monitor.
        let (purpose, monitor) = match (self.tool, self.built) {
            (Tool::Screenshot | Tool::RecordMonitor, Some(built)) => {
                (PolicyPurpose::Capture, built.monitor.as_deref())
            }
            // R13 P7 §6.4: the deck query of an input has a null monitor.
            (Tool::KeyPress | Tool::TypeText | Tool::PointerMove, _) => {
                (PolicyPurpose::Input, None)
            }
            _ => (PolicyPurpose::Desktop, None),
        };
        target_policy(self.link, self.auth.route(), purpose, monitor, deadline).await
    }

    /// Admits the call: the deck switches, then the one admission transaction.
    async fn admit(&self, built: &Built) -> Result<Admission, &'static str> {
        switches(self.link)?;
        let ctx = self.auth.ctx();
        let target = self.auth.target();
        let new = NewOp {
            operation_id: self.link.store.new_command_id().as_str().to_owned(),
            grant_id: ctx.grant_id.clone(),
            request_key: request_key(&built.request_id),
            payload_hash: payload_hash(built.args.clone(), target),
            tool: built.op_tool,
            class: self.auth.class(),
            ctx: ctx.clone(),
            target_station: target.clone(),
            action_hash: Some(action_hash_for(
                &built.action,
                self.auth.route().station_node(),
                Some(ctx),
            )),
            resource: built.resource.clone(),
            seconds: built.seconds,
            input_length: built.input_length,
        };
        self.link
            .store
            .desktop_admit(new, self.link.store.now_ms())
            .await
            .map_err(|e| store_error(&e))
    }

    /// Runs an accepted operation: the attempt row, the send, the outcome row
    /// and the reply.
    async fn run(&self, built: &Built, op: &DesktopOp, deadline: Instant) -> Outcome {
        let id = op.operation_id.as_str();
        let extra = if self.tool == Tool::RecordMonitor {
            audit::recording_extras(op)
        } else if input::is_input(op.tool) {
            input::attempt_extras(op)
        } else {
            Vec::new()
        };
        let attempt = self.facts(Some(id)).row_with(
            audit::ATTEMPT,
            AuditResult::Ok,
            None,
            Some(op.charged),
            &extra,
        );
        // No operation starts without its decision record (§12).
        if let Err(e) = audit::write_required(&self.link.store, attempt).await {
            tracing::warn!(error = %e, "coordinator desktop audit");
            let end = OpOutcome::Failed(code::AUTHORITY_UNKNOWN);
            if let Err(e) = self
                .link
                .store
                .desktop_op_finish(id, end, self.link.store.now_ms())
                .await
            {
                tracing::warn!(error = %e, "coordinator desktop finish");
            }
            return reply_err(code::AUTHORITY_UNKNOWN, Some(id));
        }
        // An input has its own run: the sign-out of the board sessions of the
        // target machine, the send and the final delivery pass.
        if input::is_input(op.tool) {
            return input_run::run_input(self.link, self.auth, op, &built.action, deadline).await;
        }
        // A screenshot has its own send: the capture record and the indexing.
        if let (Tool::Screenshot, Some(monitor)) = (self.tool, built.monitor.as_deref()) {
            let shot = capture::Shot {
                request_id: &built.request_id,
                monitor,
            };
            return capture::run_screenshot(self.link, self.auth, op, shot, deadline).await;
        }
        // A recording has its own send: the capture record, the lease and
        // the indexing.
        if let (Tool::RecordMonitor, Some(monitor), Some(seconds)) =
            (self.tool, built.monitor.as_deref(), built.seconds)
        {
            let rec = record::Rec {
                request_id: &built.request_id,
                monitor,
                seconds,
            };
            return record::run_recording(self.link, self.auth, op, rec, deadline).await;
        }
        match relay::execute(self.link, self.auth, id, &built.action, deadline).await {
            Ran::Stopped(stop) => reply_err(stop, Some(id)),
            Ran::Ended { outcome, first } => {
                let (result, failure) = match &outcome {
                    OpOutcome::Done(_) => (AuditResult::Ok, None),
                    OpOutcome::Failed(c) => (AuditResult::Failed, Some(*c)),
                    OpOutcome::Unknown => (AuditResult::Failed, Some(code::DESKTOP_TIMEOUT)),
                };
                if first {
                    let row = self
                        .facts(Some(id))
                        .row(audit::OUTCOME, result, failure, None);
                    audit::write_or_log(&self.link.store, row).await;
                }
                match (outcome, failure) {
                    (OpOutcome::Done(value), _) => reply_ok(value),
                    (_, Some(c)) => reply_err(c, Some(id)),
                    (_, None) => reply_err(code::DESKTOP_FAILED, Some(id)),
                }
            }
        }
    }
}

/// One call after step 1 (`wall_show` goes on in [`super::wall`]).
pub(super) async fn run(
    link: &DeckLink,
    station: &StationId,
    env: CoordinatorDesktopCallEnvelope,
    started: Instant,
) -> Outcome {
    let CoordinatorDesktopCallEnvelope {
        caller,
        binding,
        call,
        ..
    } = env;
    let Some((tool, station_arg)) = shape(&call) else {
        return reply_err(code::DESKTOP_UNSUPPORTED, None);
    };
    let deadline = started + Duration::from_millis(call_handler_ms(tool, &call));
    let auth = match authorize(link, station, &caller, &binding, tool, station_arg).await {
        Ok(auth) => auth,
        Err(refusal) => return reply_err(refusal, None),
    };
    // A wall directive sends no `desktop.act`: it has its own path.
    if let DesktopCall::WallShow(args) = &call {
        return wall::run(link, &auth, &binding, args, deadline).await;
    }
    let built = match build(&call, &auth.ctx().caller_label, &caller) {
        Ok(built) => built,
        Err(refusal) => {
            let this = Call {
                link,
                auth: &auth,
                tool,
                built: None,
            };
            return this.refuse(refusal).await;
        }
    };
    let this = Call {
        link,
        auth: &auth,
        tool,
        built: Some(&built),
    };
    if let Err(refusal) = this.policies(&binding, deadline).await {
        return this.refuse(refusal).await;
    }
    match this.admit(&built).await {
        Err(refusal) => this.refuse(refusal).await,
        // A screenshot and a recording replay as their capture state (§4.8).
        Ok(Admission::Existing(op))
            if matches!(op.tool, OpTool::Screenshot | OpTool::RecordMonitor) =>
        {
            capture_status::replay(link, &built.request_id, &op).await
        }
        Ok(Admission::Existing(op)) if input::is_input(op.tool) => reply_ok(
            serde_json::to_value(input::status_of(&built.request_id, &op)).unwrap_or(Value::Null),
        ),
        Ok(Admission::Existing(op)) => {
            reply_ok(serde_json::to_value(status_of(&built.request_id, &op)).unwrap_or(Value::Null))
        }
        Ok(Admission::Conflict) => this.refuse(code::REQUEST_ID_CONFLICT).await,
        Ok(Admission::Rejected { code, .. }) => this.refuse(code).await,
        Ok(Admission::Accepted(op)) => this.run(&built, &op, deadline).await,
    }
}

#[cfg(test)]
#[path = "calls_tests.rs"]
mod tests;
