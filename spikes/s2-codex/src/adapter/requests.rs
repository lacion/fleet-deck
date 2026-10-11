//! Server requests (approvals, questions) and host answers.
//!
//! Codex has two response channels for questions (PLAN L4):
//! - `item/tool/requestUserInput` is a JSON-RPC request: the answer is the RPC
//!   reply ([`ResponseChannel::RpcReply`]).
//! - An `agentMessage` item with `delivery: "async"` and `questions` asks in the
//!   transcript: the answer is a new user message ([`ResponseChannel::Message`]).
//!
//! Approval option ids are the native decisions: the strings of
//! `availableDecisions` (`accept`, `acceptForSession`, `decline`, `cancel`), and
//! for an object decision its single key (`acceptWithExecpolicyAmendment`). The
//! reply sends the native decision value unchanged.

use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use spike_canon::{ApprovalOption, CanonEvent, RequestKind, ResponseChannel};

use super::{AdapterError, CodexCore, Output};
use crate::rpc;

/// The host's answer to an open request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Answer {
    pub option_id: String,
    /// Question answers (`{"answers": {<question id>: {"answers": [..]}}}`).
    /// Codex approvals take no edited input: the core refuses it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_input: Option<Value>,
    /// Free-text answer for a question on the message channel.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// Result of an accepted answer. A second answer returns the first receipt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnswerReceipt {
    pub request_id: String,
    pub option_id: String,
    pub duplicate: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct OpenRequest {
    /// JSON-RPC id (`Value::Null` for message-channel questions).
    native_id: Value,
    channel: ResponseChannel,
    kind: RequestKind,
    /// option id → native decision value.
    options: Vec<(String, Value)>,
    node_id: Option<String>,
    receipt: Option<AnswerReceipt>,
}

#[derive(Debug, Default)]
pub(crate) struct Requests {
    next: u64,
    open: BTreeMap<String, OpenRequest>,
    by_native: HashMap<String, String>,
}

/// Server requests that become canonical approvals or questions.
pub const HANDLED_REQUESTS: &[&str] = &[
    "item/commandExecution/requestApproval",
    "item/fileChange/requestApproval",
    "item/tool/requestUserInput",
];

/// Server requests that get a fixed refusal (`decline`).
pub const DECLINED_REQUESTS: &[&str] = &["mcpServer/elicitation/request"];

const DEFAULT_DECISIONS: &[&str] = &["accept", "acceptForSession", "decline", "cancel"];

fn native_key(id: &Value) -> String {
    id.to_string()
}

fn option_id(decision: &Value) -> Option<String> {
    match decision {
        Value::String(s) => Some(s.clone()),
        Value::Object(m) if m.len() == 1 => m.keys().next().cloned(),
        _ => None,
    }
}

fn decisions(params: &Value) -> Vec<(String, Value)> {
    match params.get("availableDecisions").and_then(Value::as_array) {
        Some(list) => list
            .iter()
            .filter_map(|d| option_id(d).map(|id| (id, d.clone())))
            .collect(),
        None => DEFAULT_DECISIONS
            .iter()
            .map(|d| ((*d).to_owned(), Value::from(*d)))
            .collect(),
    }
}

fn label(option_id: &str) -> String {
    match option_id {
        "accept" => "Allow once".into(),
        "acceptForSession" => "Allow for this session".into(),
        "acceptWithExecpolicyAmendment" => "Allow and remember this command".into(),
        "applyNetworkPolicyAmendment" => "Allow this network host".into(),
        "decline" => "Deny (the turn continues)".into(),
        "cancel" => "Deny and stop the turn".into(),
        "answer" => "Answer".into(),
        other => other.into(),
    }
}

fn refs(pairs: &[(&str, Option<&str>)]) -> Map<String, Value> {
    pairs
        .iter()
        .filter_map(|(k, v)| v.map(|v| ((*k).to_owned(), Value::from(v))))
        .collect()
}

impl CodexCore {
    /// Open requests (app ids), oldest first.
    pub fn open_requests(&self) -> Vec<String> {
        let mut ids: Vec<_> = self
            .requests
            .open
            .iter()
            .filter(|(_, r)| r.receipt.is_none())
            .map(|(k, _)| k.clone())
            .collect();
        ids.sort_by_key(|k| k.trim_start_matches("req-").parse::<u64>().unwrap_or(0));
        ids
    }

    /// Native JSON-RPC id of an open request.
    pub fn native_request_id(&self, request_id: &str) -> Option<&Value> {
        self.requests.open.get(request_id).map(|r| &r.native_id)
    }

    /// App id of a native JSON-RPC id.
    pub fn app_request_id(&self, native: &Value) -> Option<&str> {
        self.requests
            .by_native
            .get(&native_key(native))
            .map(String::as_str)
    }

    fn register(
        &mut self,
        req: OpenRequest,
        title: String,
        provider_refs: Map<String, Value>,
    ) -> Output {
        self.requests.next += 1;
        let app_id = format!("req-{}", self.requests.next);
        if !req.native_id.is_null() {
            self.requests
                .by_native
                .insert(native_key(&req.native_id), app_id.clone());
        }
        let event = CanonEvent::ApprovalRequested {
            request_id: app_id.clone(),
            node_id: req.node_id.clone(),
            request_kind: req.kind,
            title,
            options: req
                .options
                .iter()
                .map(|(id, _)| ApprovalOption {
                    option_id: id.clone(),
                    label: label(id),
                })
                .collect(),
            response: req.channel,
            provider_refs,
        };
        self.requests.open.insert(app_id, req);
        Output::Emit(event)
    }

    pub(crate) fn on_server_request(
        &mut self,
        id: Value,
        method: &str,
        params: &Value,
    ) -> Vec<Output> {
        let item = params.get("itemId").and_then(Value::as_str);
        let node_id = item
            .and_then(|i| self.tools.get(i))
            .map(|n| n.node_id.clone());
        let native = native_key(&id);
        let provider_refs = refs(&[
            ("method", Some(method)),
            ("native_request_id", Some(&native)),
            ("item_id", item),
            ("turn_id", params.get("turnId").and_then(Value::as_str)),
        ]);
        let text = |k: &str| params.get(k).and_then(Value::as_str).map(str::to_owned);
        let (kind, title, options) = match method {
            "item/commandExecution/requestApproval" => (
                RequestKind::Command,
                text("command").unwrap_or_else(|| "Run a command".into()),
                decisions(params),
            ),
            "item/fileChange/requestApproval" => (
                RequestKind::FileChange,
                text("reason").unwrap_or_else(|| "Apply file changes".into()),
                decisions(params),
            ),
            "item/tool/requestUserInput" => (
                RequestKind::Question,
                params
                    .pointer("/questions/0/question")
                    .and_then(Value::as_str)
                    .unwrap_or("Question")
                    .to_owned(),
                vec![("answer".to_owned(), Value::Null)],
            ),
            "mcpServer/elicitation/request" => {
                return self.refuse(id, method, Some(json!({"action": "decline"})));
            }
            _ => return self.refuse(id, method, None),
        };
        let channel = ResponseChannel::RpcReply;
        vec![self.register(
            OpenRequest {
                native_id: id,
                channel,
                kind,
                options,
                node_id,
                receipt: None,
            },
            title,
            provider_refs,
        )]
    }

    /// Fail closed: a request that the core does not model gets a refusal.
    fn refuse(&mut self, id: Value, method: &str, result: Option<Value>) -> Vec<Output> {
        let write = match result {
            Some(r) => rpc::reply(&id, r),
            None => rpc::reply_error(
                &id,
                rpc::REFUSED,
                "fleet deck does not support this request",
            ),
        };
        vec![
            Output::Write(write),
            Output::Emit(CanonEvent::Error {
                code: "unsupported_request".into(),
                message: format!("refused server request {method}"),
            }),
        ]
    }

    /// A question that the agent asked in an `agentMessage` with `delivery:
    /// "async"`: the answer is a new user message (L4 message channel).
    pub(crate) fn on_async_questions(&mut self, item_id: &str, questions: &[Value]) -> Vec<Output> {
        let Some(first) = questions.first() else {
            return Vec::new();
        };
        let title = first
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or("Question")
            .to_owned();
        let mut options: Vec<(String, Value)> = first
            .get("options")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(|o| (o.to_owned(), Value::from(o)))
            .collect();
        options.push(("answer".into(), Value::Null));
        vec![self.register(
            OpenRequest {
                native_id: Value::Null,
                channel: ResponseChannel::Message,
                kind: RequestKind::Question,
                options,
                node_id: None,
                receipt: None,
            },
            title,
            refs(&[("item_id", Some(item_id)), ("delivery", Some("async"))]),
        )]
    }

    /// Answers an open request. Idempotent: a second answer writes nothing and
    /// returns the first receipt.
    pub fn answer(
        &mut self,
        request_id: &str,
        answer: &Answer,
    ) -> Result<(AnswerReceipt, Vec<Output>), AdapterError> {
        let req = self
            .requests
            .open
            .get(request_id)
            .cloned()
            .ok_or_else(|| AdapterError::UnknownRequest(request_id.to_owned()))?;
        if let Some(r) = &req.receipt {
            return Ok((
                AnswerReceipt {
                    duplicate: true,
                    ..r.clone()
                },
                Vec::new(),
            ));
        }
        let decision = req
            .options
            .iter()
            .find(|(id, _)| *id == answer.option_id)
            .map(|(_, v)| v.clone())
            .ok_or_else(|| AdapterError::UnknownOption {
                request: request_id.to_owned(),
                option: answer.option_id.clone(),
            })?;
        let mut out = match (req.channel, req.kind) {
            (ResponseChannel::Message, _) => {
                let text = match (&answer.message, decision.as_str()) {
                    (Some(m), _) => m.clone(),
                    (None, Some(o)) => o.to_owned(),
                    (None, None) => {
                        return Err(AdapterError::InvalidAnswer(
                            "the answer needs a message".into(),
                        ));
                    }
                };
                self.prompt(&text, &format!("answer-{request_id}"))?
            }
            (ResponseChannel::RpcReply, RequestKind::Question) => {
                let result = answer
                    .updated_input
                    .clone()
                    .filter(|v| v.get("answers").is_some_and(Value::is_object))
                    .ok_or_else(|| {
                        AdapterError::InvalidAnswer("a question needs {\"answers\": {..}}".into())
                    })?;
                vec![Output::Write(rpc::reply(&req.native_id, result))]
            }
            (ResponseChannel::RpcReply, _) => {
                if answer.updated_input.is_some() {
                    return Err(AdapterError::Unsupported("edited input before allow"));
                }
                if matches!(answer.option_id.as_str(), "decline" | "cancel")
                    && let Some(node) = req
                        .node_id
                        .as_ref()
                        .and_then(|n| self.tools.values_mut().find(|t| &t.node_id == n))
                {
                    node.denied = true;
                }
                vec![Output::Write(rpc::reply(
                    &req.native_id,
                    json!({"decision": decision}),
                ))]
            }
        };
        let receipt = AnswerReceipt {
            request_id: request_id.to_owned(),
            option_id: answer.option_id.clone(),
            duplicate: false,
        };
        if let Some(r) = self.requests.open.get_mut(request_id) {
            r.receipt = Some(receipt.clone());
        }
        out.push(Output::Emit(CanonEvent::ApprovalResolved {
            request_id: request_id.to_owned(),
            option_id: answer.option_id.clone(),
        }));
        Ok((receipt, out))
    }

    /// `serverRequest/resolved`: the app-server closed a request. If the host
    /// answered it, this confirms the answer; if not, the request expired.
    pub(crate) fn on_resolved(&mut self, params: &Value) -> Vec<Output> {
        let Some(native) = params.get("requestId") else {
            return Vec::new();
        };
        let Some(app_id) = self.requests.by_native.remove(&native_key(native)) else {
            return Vec::new();
        };
        match self.requests.open.remove(&app_id) {
            Some(r) if r.receipt.is_none() => vec![Output::Emit(CanonEvent::ApprovalExpired {
                request_id: app_id,
                reason: "cancelled_by_provider".into(),
            })],
            _ => Vec::new(),
        }
    }

    /// Expires open requests. At turn end only the RPC requests expire; a
    /// message-channel question stays answerable until the process ends.
    pub(crate) fn expire_open(&mut self, reason: &str) -> Vec<Output> {
        let all = reason == "process_lost";
        let mut out = Vec::new();
        for app_id in self.open_requests() {
            let Some(r) = self.requests.open.get(&app_id) else {
                continue;
            };
            // An answered request is done; a message-channel question stays
            // answerable after the turn.
            if r.receipt.is_some() || (!all && r.channel == ResponseChannel::Message) {
                continue;
            }
            out.push(Output::Emit(CanonEvent::ApprovalExpired {
                request_id: app_id.clone(),
                reason: reason.to_owned(),
            }));
        }
        self.requests
            .open
            .retain(|_, r| r.receipt.is_none() && !all && r.channel == ResponseChannel::Message);
        self.requests.by_native.clear();
        out
    }
}
