//! Runtime requests (ADR-001 §2.5): `can_use_tool` becomes
//! `approval.requested`; the owner's answer becomes a `control_response`.
//! Unknown permission requests fail closed.

use fleetdeck_proto::event::{
    ApprovalOption, ExpiredReason, RequestKind, ResponseChannel, ToolKind,
};
use fleetdeck_proto::{Event, RequestId};
use serde_json::Value;

use super::control::{self, PermissionDecision};
use super::core::{ClaudeCore, Output, ReqState, RuntimeRequest};
use super::frames::ControlRequest;
use super::map::{input_summary, refs, tool_kind};
use crate::adapter::{Answer, ProviderError, Result};

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

fn option(id: &str, label: &str) -> ApprovalOption {
    ApprovalOption {
        option_id: id.to_owned(),
        label: label.to_owned(),
    }
}

fn options_for(session_rules: Option<&Value>) -> Vec<ApprovalOption> {
    let mut options = vec![option("allow", "Allow once")];
    if session_rules.is_some() {
        options.push(option("allow_always", "Allow always (this session)"));
    }
    options.push(option("deny", "Deny"));
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
            let options = options_for(rules.as_ref());
            let details = input_summary(&tool_name, &input);
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
            vec![Output::Emit(Event::ApprovalExpired {
                request_id: app,
                reason,
            })]
        }
        _ => Vec::new(),
    }
}

impl ClaudeCore {
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
        if option_id == "allow_always"
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
