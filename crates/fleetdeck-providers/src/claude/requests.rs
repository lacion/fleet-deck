//! Runtime requests (ADR-001 §2.5): `can_use_tool` becomes
//! `approval.requested`; the owner's answer becomes a `control_response`.
//! Unknown permission requests fail closed.

use fleetdeck_proto::event::{
    ApprovalOption, ExpiredReason, OptionKind, RequestKind, ResolvedBy, ResponseChannel, ToolKind,
};
use fleetdeck_proto::{Event, RequestId};
use serde_json::Value;

use super::control::{self, PermissionDecision};
use super::core::{ClaudeCore, Output, ReqState, RuntimeRequest};
use super::frames::{ControlRequest, valid_agent_id};
use super::map::{input_summary, refs, tool_kind};
use crate::adapter::{Answer, ProviderError, Result};

/// How many answered or expired requests the core keeps (NV-18). The
/// oldest ones are dropped, with their native id mapping. An open request
/// is never dropped.
const MAX_CLOSED_REQUESTS: usize = 256;

/// Maps a Claude tool name to the kind of its permission request.
#[must_use]
pub fn request_kind(name: &str) -> RequestKind {
    match tool_kind(name) {
        ToolKind::Shell => RequestKind::Command,
        ToolKind::Edit => RequestKind::FileChange,
        _ if name == "AskUserQuestion" => RequestKind::Question,
        _ => RequestKind::Tool,
    }
}

/// The part of the CLI's `permission_suggestions` that `allow_always` may send
/// (DL-009): allow rules only, re-scoped to `destination: "session"`. Rules
/// that write settings files are re-scoped; `setMode` (L8: modes change only
/// through `set_permission_mode`) and `addDirectories` (widens the workspace)
/// are dropped. `None` when nothing is left.
pub(super) fn session_rules(suggestions: Option<&Value>) -> Option<Value> {
    let kept: Vec<Value> = suggestions?
        .as_array()?
        .iter()
        .filter(|s| {
            matches!(
                s.get("type").and_then(Value::as_str),
                Some("addRules" | "replaceRules")
            ) && s.get("behavior").and_then(Value::as_str) == Some("allow")
        })
        .map(|s| {
            let mut s = s.clone();
            if let Some(m) = s.as_object_mut() {
                m.insert("destination".to_owned(), Value::from("session"));
            }
            s
        })
        .collect();
    (!kept.is_empty()).then_some(Value::Array(kept))
}

/// The semantic kind of a Claude option id (ADR-008 §5.1). Any id that this
/// adapter does not offer is `Other`, so a coordinator fails closed.
#[must_use]
pub(super) fn option_kind(id: &str) -> OptionKind {
    match id {
        "allow" => OptionKind::AllowOnce,
        "allow_always" => OptionKind::AllowAlways,
        "deny" => OptionKind::Deny,
        _ => OptionKind::Other,
    }
}

/// `classify` is false for a question request: its options get no kind.
fn option(id: &str, label: &str, classify: bool) -> ApprovalOption {
    ApprovalOption {
        option_id: id.to_owned(),
        label: label.to_owned(),
        kind: classify.then(|| option_kind(id)),
    }
}

fn options_for(session_rules: Option<&Value>, classify: bool) -> Vec<ApprovalOption> {
    let mut options = vec![option("allow", "Allow once", classify)];
    if session_rules.is_some() {
        options.push(option(
            "allow_always",
            "Allow always (this session)",
            classify,
        ));
    }
    options.push(option("deny", "Deny", classify));
    options
}

pub(super) fn on_control_request(
    core: &mut ClaudeCore,
    native_id: &str,
    request: ControlRequest,
) -> Vec<Output> {
    match request {
        ControlRequest::CanUseTool {
            tool_name,
            input,
            tool_use_id,
            permission_suggestions,
            body,
            ..
        } => {
            if core.native_to_app.contains_key(native_id) {
                // A repeated request with the same native id is the same request.
                return Vec::new();
            }
            let request_id = RequestId(core.ids.next("req"));
            let node_id = tool_use_id
                .as_ref()
                .and_then(|t| core.tools.get(t))
                .map(|n| n.node_id.clone());
            let rules = session_rules(permission_suggestions.as_ref());
            let options = options_for(
                rules.as_ref(),
                request_kind(&tool_name) != RequestKind::Question,
            );
            let details = input_summary(&tool_name, &input);
            // The asking agent of a workflow agent's request (B12).
            let agent_id = body
                .get("agent_id")
                .and_then(Value::as_str)
                .filter(|id| valid_agent_id(id));
            core.native_to_app
                .insert(native_id.to_owned(), request_id.clone());
            core.requests.insert(
                request_id.clone(),
                RuntimeRequest {
                    native_request_id: native_id.to_owned(),
                    tool_use_id: tool_use_id.clone(),
                    input: input.clone(),
                    suggestions: rules,
                    options: options.clone(),
                    state: ReqState::Open,
                },
            );
            vec![Output::Emit(Event::ApprovalRequested {
                request_id,
                node_id,
                request_kind: request_kind(&tool_name),
                title: tool_name,
                details,
                input: Some(input),
                options,
                response: ResponseChannel::RpcReply,
                provider_refs: refs(&[
                    ("control_request_id", Some(native_id)),
                    ("tool_use_id", tool_use_id.as_deref()),
                    ("agent_id", agent_id),
                ]),
            })]
        }
        // The host registers no hooks and no SDK MCP servers: fail closed.
        ControlRequest::HookCallback { .. } | ControlRequest::McpMessage { .. } => {
            vec![Output::Write(control::control_error(
                native_id,
                "fleetdeck: not registered",
            ))]
        }
        ControlRequest::Unknown { subtype, .. } => vec![
            Output::Write(control::control_error(
                native_id,
                &format!("fleetdeck: unsupported control request {subtype}"),
            )),
            Output::Emit(Event::Error {
                code: "unknown_control_request".to_owned(),
                message: format!("denied unknown control request subtype '{subtype}'"),
                retry_at: None,
            }),
        ],
    }
}

/// `control_cancel_request`: the CLI withdrew one of its requests.
pub(super) fn on_cancel(core: &mut ClaudeCore, native_id: &str) -> Vec<Output> {
    let Some(app) = core.native_to_app.get(native_id).cloned() else {
        return Vec::new();
    };
    match core.requests.get_mut(&app) {
        Some(req) if req.state == ReqState::Open => {
            let reason = ExpiredReason::CancelledByProvider;
            req.state = ReqState::Expired(reason);
            core.note_closed(app.clone());
            vec![Output::Emit(Event::ApprovalExpired {
                request_id: app,
                reason,
            })]
        }
        _ => Vec::new(),
    }
}

impl ClaudeCore {
    /// Notes that a request is answered or expired. When more than
    /// [`MAX_CLOSED_REQUESTS`] are closed, the oldest one is forgotten
    /// (NV-18).
    pub(super) fn note_closed(&mut self, id: RequestId) {
        self.closed_order.push_back(id);
        while self.closed_order.len() > MAX_CLOSED_REQUESTS {
            let Some(old) = self.closed_order.pop_front() else {
                break;
            };
            self.requests.remove(&old);
            self.native_to_app.retain(|_, app| *app != old);
        }
    }

    /// Answers a runtime request with a native option id. Idempotent: a second
    /// answer writes nothing and emits nothing (L3).
    ///
    /// # Errors
    /// An unknown request, an option that the request does not offer, or a
    /// request that expired.
    pub fn answer(&mut self, answer: &Answer) -> Result<Vec<Output>> {
        let request_id = &answer.request;
        let option_id = answer.decision.option_id.as_str();
        let req = self
            .requests
            .get_mut(request_id)
            .ok_or_else(|| ProviderError::UnknownRequest(request_id.clone()))?;
        match &req.state {
            ReqState::Answered(_) => return Ok(Vec::new()),
            ReqState::Expired(reason) => {
                return Err(ProviderError::RequestClosed {
                    request: request_id.clone(),
                    reason: *reason,
                });
            }
            ReqState::Open => {}
        }
        if !req.options.iter().any(|o| o.option_id == option_id) {
            return Err(ProviderError::UnknownOption {
                request: request_id.clone(),
                option: option_id.to_owned(),
            });
        }
        let decision = match option_id {
            "allow" | "allow_always" => PermissionDecision::Allow {
                updated_input: answer
                    .decision
                    .updated_input
                    .clone()
                    .unwrap_or_else(|| req.input.clone()),
            },
            _ => PermissionDecision::Deny {
                message: answer
                    .decision
                    .text
                    .clone()
                    .unwrap_or_else(|| "Denied by the owner".to_owned()),
                interrupt: false,
            },
        };
        let mut frame = control::permission_response(
            &req.native_request_id,
            &decision,
            req.tool_use_id.as_deref(),
        );
        // A coordinator never widens the session's permission rules
        // (ADR-008 §5.3): its answer carries no `updatedPermissions`.
        if option_id == "allow_always"
            && answer.by != ResolvedBy::Coordinator
            && let (Some(body), Some(rules)) = (
                frame["response"]["response"].as_object_mut(),
                req.suggestions.clone(),
            )
        {
            body.insert("updatedPermissions".to_owned(), rules);
        }
        if matches!(decision, PermissionDecision::Deny { .. })
            && let Some(node) = req.tool_use_id.as_ref().and_then(|t| self.tools.get_mut(t))
        {
            node.denied = true;
        }
        req.state = ReqState::Answered(option_id.to_owned());
        self.note_closed(request_id.clone());
        Ok(vec![
            Output::Write(frame),
            Output::Emit(Event::ApprovalResolved {
                request_id: request_id.clone(),
                option_id: option_id.to_owned(),
                by: answer.by,
                answered_by: answer.decision.answered_by.clone(),
            }),
        ])
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::super::checkpoint::CHECKPOINT_VERSION;
    use super::super::tests::{answer, can_use_tool, config, feed, running_core};
    use super::*;
    use crate::checkpoint::AdapterCheckpoint;

    /// Opens request `n` and returns its app id.
    fn open(core: &mut ClaudeCore, n: usize) -> RequestId {
        let ev = can_use_tool(core, &format!("n-{n}"), "tu-none");
        ev.into_iter()
            .find_map(|e| match e {
                Event::ApprovalRequested { request_id, .. } => Some(request_id),
                _ => None,
            })
            .unwrap()
    }

    #[test]
    fn answered_and_cancelled_requests_are_bounded_and_the_open_ones_stay() {
        // NV-18.
        let mut core = running_core();
        let kept = open(&mut core, 0);
        for n in 1..=300 {
            let id = open(&mut core, n);
            if n % 2 == 0 {
                core.answer(&answer(id.as_str(), "allow")).unwrap();
            } else {
                feed(
                    &mut core,
                    json!({"type":"control_cancel_request","request_id":format!("n-{n}")}),
                );
            }
        }
        assert_eq!(core.closed_order.len(), MAX_CLOSED_REQUESTS);
        assert_eq!(core.requests.len(), MAX_CLOSED_REQUESTS + 1);
        assert_eq!(core.native_to_app.len(), MAX_CLOSED_REQUESTS + 1);
        assert_eq!(core.open_requests(), std::slice::from_ref(&kept));
        assert_eq!(core.native_request_id(&kept), Some("n-0"));
        // The oldest closed request is forgotten; the newest is kept.
        assert!(core.request_for_native("n-1").is_none());
        assert!(core.request_for_native("n-300").is_some());
        // The open request still answers.
        let out = core.answer(&answer(kept.as_str(), "deny")).unwrap();
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn the_closed_order_survives_a_checkpoint_round_trip() {
        let mut core = running_core();
        for n in 0..10 {
            let id = open(&mut core, n);
            core.answer(&answer(id.as_str(), "allow")).unwrap();
        }
        let text = core.checkpoint().unwrap().to_json().unwrap();
        let stored = AdapterCheckpoint::from_json(&text).unwrap();
        let restored = ClaudeCore::restore(config(false), &stored).unwrap();
        assert_eq!(restored.closed_order.len(), 10);
        assert_eq!(restored.closed_order, core.closed_order);
    }

    #[test]
    fn a_checkpoint_without_the_closed_order_loads() {
        let mut core = running_core();
        let id = open(&mut core, 0);
        core.answer(&answer(id.as_str(), "allow")).unwrap();
        let mut stored = core.checkpoint().unwrap();
        stored.state.as_object_mut().unwrap().remove("closed_order");
        assert_eq!(stored.version, CHECKPOINT_VERSION);
        let restored = ClaudeCore::restore(config(false), &stored).unwrap();
        assert!(restored.closed_order.is_empty());
        assert_eq!(restored.requests.len(), 1);
    }
}
