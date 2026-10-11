//! Server requests (approvals, questions) and host answers (ADR-002 §2.6).
//!
//! Codex has two response channels for questions (PLAN L4):
//! - `item/tool/requestUserInput` is a JSON-RPC request: the answer is the RPC
//!   reply ([`ResponseChannel::RpcReply`]).
//! - An `agentMessage` item with `delivery: "async"` and `questions` asks in
//!   the transcript: the answer is a new user message
//!   ([`ResponseChannel::Message`]).
//!
//! Approval option ids are the native decisions: the strings of
//! `availableDecisions` (`accept`, `acceptForSession`, `decline`, `cancel`),
//! and for an object decision its single key
//! (`acceptWithExecpolicyAmendment`). The reply sends the native decision
//! value unchanged. A request that the core does not model is refused (fail
//! closed).

use std::collections::{BTreeMap, HashMap};

use fleetdeck_proto::command::UserInput;
use fleetdeck_proto::event::{
    ApprovalOption, ExpiredReason, ProviderRefs, RequestKind, ResponseChannel, RunOrigin, ToolKind,
    ToolStatus,
};
use fleetdeck_proto::{Event, NodeId, Operation, RequestId, Unsupported};
use serde_json::{Value, json};

use super::core::{CodexCore, Output};
use super::map::{QUESTION_ITEM, refs};
use super::rpc;
use super::{request_id, truncate};
use crate::adapter::{Answer, HostInput, ProviderError, Result, answer_input_id};

/// The longest request detail text.
const DETAIL_CHARS: usize = 2_000;

#[derive(Debug, Clone)]
pub(super) struct OpenRequest {
    /// JSON-RPC id (`Value::Null` for message-channel questions).
    native_id: Value,
    channel: ResponseChannel,
    kind: RequestKind,
    /// option id → native decision value.
    options: Vec<(String, Value)>,
    node_id: Option<NodeId>,
    /// The option of the accepted answer.
    answered: Option<String>,
    /// Native request parameters (or the question of a message question).
    params: Value,
}

/// How a request left the open set.
#[derive(Debug, Clone, PartialEq)]
enum Closed {
    Answered,
    Expired(ExpiredReason),
}

#[derive(Debug, Default)]
pub(super) struct Requests {
    next: u64,
    open: BTreeMap<u64, (String, OpenRequest)>,
    by_native: HashMap<String, String>,
    /// Requests that left `open`: app id → how. A late answer to a closed
    /// request is a duplicate (no write) or [`ProviderError::RequestClosed`].
    closed: HashMap<String, Closed>,
}

impl Requests {
    fn get(&self, app_id: &str) -> Option<&OpenRequest> {
        self.open
            .values()
            .find(|(id, _)| id == app_id)
            .map(|(_, r)| r)
    }

    fn get_mut(&mut self, app_id: &str) -> Option<&mut OpenRequest> {
        self.open
            .values_mut()
            .find(|(id, _)| id == app_id)
            .map(|(_, r)| r)
    }

    fn remove(&mut self, app_id: &str) -> Option<OpenRequest> {
        let key = self
            .open
            .iter()
            .find(|(_, (id, _))| id == app_id)
            .map(|(k, _)| *k)?;
        self.open.remove(&key).map(|(_, r)| r)
    }
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

/// The free-text details of a request: its reason and, for a command, the
/// working directory.
fn details(params: &Value) -> String {
    let text = |k: &str| params.get(k).and_then(Value::as_str);
    let mut parts = Vec::new();
    if let Some(r) = text("reason") {
        parts.push(r.to_owned());
    }
    if let Some(c) = text("cwd") {
        parts.push(format!("in {c}"));
    }
    truncate(&parts.join(" "), DETAIL_CHARS)
}

impl CodexCore {
    /// Open requests that wait for an answer, oldest first.
    #[must_use]
    pub fn open_requests(&self) -> Vec<RequestId> {
        self.requests
            .open
            .values()
            .filter(|(_, r)| r.answered.is_none())
            .map(|(id, _)| request_id(id))
            .collect()
    }

    /// Native parameters of an open request (for example the question ids of
    /// `item/tool/requestUserInput`).
    #[must_use]
    pub fn request_params(&self, request: &RequestId) -> Option<&Value> {
        self.requests.get(request.as_str()).map(|r| &r.params)
    }

    /// Native JSON-RPC id of an open request.
    #[must_use]
    pub fn native_request_id(&self, request: &RequestId) -> Option<&Value> {
        self.requests.get(request.as_str()).map(|r| &r.native_id)
    }

    /// The request of a native JSON-RPC id.
    #[must_use]
    pub fn request_for_native(&self, native: &Value) -> Option<RequestId> {
        self.requests
            .by_native
            .get(&native_key(native))
            .map(|id| request_id(id))
    }

    fn register(&mut self, req: OpenRequest, title: String, provider_refs: ProviderRefs) -> Output {
        self.requests.next += 1;
        let n = self.requests.next;
        let app_id = format!("req-{n}");
        if !req.native_id.is_null() {
            self.requests
                .by_native
                .insert(native_key(&req.native_id), app_id.clone());
        }
        let input = if req.channel == ResponseChannel::Message {
            req.params.get("question").cloned()
        } else {
            Some(req.params.clone())
        };
        let event = Event::ApprovalRequested {
            request_id: request_id(&app_id),
            node_id: req.node_id.clone(),
            request_kind: req.kind,
            title: truncate(&title, DETAIL_CHARS),
            details: details(&req.params),
            input,
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
        self.requests.open.insert(n, (app_id, req));
        Output::Emit(event)
    }

    pub(super) fn on_server_request(
        &mut self,
        id: Value,
        method: &str,
        params: &Value,
    ) -> Vec<Output> {
        let item = params.get("itemId").and_then(Value::as_str);
        let mut started = Vec::new();
        // Codex starts no item for a question tool call. The core opens a
        // node for it, so the timeline shows the question and the answer
        // gets provider evidence: the node completes when the app-server
        // resolves the answered request (DL-068).
        if method == "item/tool/requestUserInput"
            && let Some(i) = item
            && !self.tools.contains_key(i)
        {
            let question = params
                .pointer("/questions/0/question")
                .cloned()
                .unwrap_or(Value::Null);
            let shape = json!({"type": QUESTION_ITEM, "id": i, "question": question});
            started = self.tool_started(params, &shape, i, QUESTION_ITEM, ToolKind::Other);
        }
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
                return self.refuse(&id, method, Some(json!({"action": "decline"})));
            }
            _ => return self.refuse(&id, method, None),
        };
        started.push(self.register(
            OpenRequest {
                native_id: id,
                channel: ResponseChannel::RpcReply,
                kind,
                options,
                node_id,
                answered: None,
                params: params.clone(),
            },
            title,
            provider_refs,
        ));
        started
    }

    /// Fail closed: a request that the core does not model gets a refusal.
    fn refuse(&mut self, id: &Value, method: &str, result: Option<Value>) -> Vec<Output> {
        let write = match result {
            Some(r) => rpc::reply(id, r),
            None => rpc::reply_error(id, rpc::REFUSED, "fleet deck does not support this request"),
        };
        vec![
            Output::Write(write),
            Output::Emit(Event::Error {
                code: "unsupported_request".into(),
                message: format!("refused server request {method}"),
                retry_at: None,
            }),
        ]
    }

    /// Questions that the agent asked in an `agentMessage` with `delivery:
    /// "async"`: the answer is a new user message (L4 message channel). Each
    /// question becomes its own request, so none is lost.
    pub(super) fn on_async_questions(&mut self, item_id: &str, questions: &[Value]) -> Vec<Output> {
        let count = questions.len();
        let mut out = Vec::with_capacity(count);
        for (index, question) in questions.iter().enumerate() {
            let title = question
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or("Question")
                .to_owned();
            let mut options: Vec<(String, Value)> = question
                .get("options")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(|o| (o.to_owned(), Value::from(o)))
                .collect();
            options.push(("answer".into(), Value::Null));
            let index_text = index.to_string();
            out.push(self.register(
                OpenRequest {
                    native_id: Value::Null,
                    channel: ResponseChannel::Message,
                    kind: RequestKind::Question,
                    options,
                    node_id: None,
                    answered: None,
                    params: json!({"question": question, "index": index, "count": count}),
                },
                title,
                refs(&[
                    ("item_id", Some(item_id)),
                    ("delivery", Some("async")),
                    ("question_index", Some(&index_text)),
                ]),
            ));
        }
        out
    }

    /// Answers an open request. Idempotent: a second answer writes nothing.
    ///
    /// # Errors
    /// An unknown or expired request, an option that the request does not
    /// offer, an edited input (Codex takes none), or an invalid question
    /// answer.
    pub fn answer(&mut self, answer: &Answer) -> Result<Vec<Output>> {
        if self.ended {
            return Err(ProviderError::SessionEnded);
        }
        let app_id = answer.request.as_str().to_owned();
        let Some(req) = self.requests.get(&app_id).cloned() else {
            return match self.requests.closed.get(&app_id) {
                Some(Closed::Answered) => Ok(Vec::new()),
                Some(Closed::Expired(reason)) => Err(ProviderError::RequestClosed {
                    request: answer.request.clone(),
                    reason: *reason,
                }),
                None => Err(ProviderError::UnknownRequest(answer.request.clone())),
            };
        };
        if req.answered.is_some() {
            return Ok(Vec::new());
        }
        let option = answer.decision.option_id.clone();
        let decision = req
            .options
            .iter()
            .find(|(id, _)| *id == option)
            .map(|(_, v)| v.clone())
            .ok_or_else(|| ProviderError::UnknownOption {
                request: answer.request.clone(),
                option: option.clone(),
            })?;
        let mut out = match (req.channel, req.kind) {
            (ResponseChannel::Message, _) => {
                let text = match (&answer.decision.text, decision.as_str()) {
                    (Some(m), _) if !m.is_empty() => m.clone(),
                    (_, Some(o)) => o.to_owned(),
                    _ => {
                        return Err(ProviderError::InvalidAnswer(
                            "the answer needs a message".into(),
                        ));
                    }
                };
                let text = message_answer(&req.params, &text);
                self.prompt(&HostInput {
                    id: answer_input_id(&answer.request),
                    origin: RunOrigin::Board,
                    input: UserInput { text },
                })?
            }
            (ResponseChannel::RpcReply, RequestKind::Question) => {
                let result =
                    validate_question_answer(&req.params, answer.decision.updated_input.as_ref())?;
                vec![Output::Write(rpc::reply(&req.native_id, result))]
            }
            (ResponseChannel::RpcReply, _) => {
                if answer.decision.updated_input.is_some() {
                    return Err(Unsupported::new(Operation::EditInput).into());
                }
                if matches!(option.as_str(), "decline" | "cancel")
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
        if let Some(r) = self.requests.get_mut(&app_id) {
            r.answered = Some(option.clone());
        }
        out.push(Output::Emit(Event::ApprovalResolved {
            request_id: answer.request.clone(),
            option_id: option,
            by: answer.by,
        }));
        Ok(out)
    }

    /// Moves a request from `open` to `closed`.
    fn close(&mut self, app_id: &str, reason: ExpiredReason) -> Option<OpenRequest> {
        let r = self.requests.remove(app_id)?;
        let how = if r.answered.is_some() {
            Closed::Answered
        } else {
            Closed::Expired(reason)
        };
        self.requests.closed.insert(app_id.to_owned(), how);
        Some(r)
    }

    /// `serverRequest/resolved`: the app-server closed a request. If the host
    /// answered it, this confirms the answer; if not, the request expired.
    pub(super) fn on_resolved(&mut self, params: &Value) -> Vec<Output> {
        let Some(native) = params.get("requestId") else {
            return Vec::new();
        };
        let Some(app_id) = self.requests.by_native.remove(&native_key(native)) else {
            return Vec::new();
        };
        match self.close(&app_id, ExpiredReason::CancelledByProvider) {
            Some(r) if r.answered.is_none() => vec![Output::Emit(Event::ApprovalExpired {
                request_id: request_id(&app_id),
                reason: ExpiredReason::CancelledByProvider,
            })],
            Some(r) => self.answered_question(r.node_id.as_ref()),
            None => Vec::new(),
        }
    }

    /// The app-server took the answer of a question: its node completes.
    fn answered_question(&mut self, node_id: Option<&NodeId>) -> Vec<Output> {
        let Some(node) = node_id.and_then(|n| {
            self.tools
                .values_mut()
                .find(|t| &t.node_id == n && t.item_type == QUESTION_ITEM && !t.done)
        }) else {
            return Vec::new();
        };
        node.done = true;
        vec![Output::Emit(Event::ToolCompleted {
            node_id: node.node_id.clone(),
            status: ToolStatus::Ok,
            files: Vec::new(),
            exit_code: None,
        })]
    }

    /// Expires open requests. At turn end only the RPC requests expire; a
    /// message-channel question stays answerable until the process ends.
    pub(super) fn expire_open(&mut self, reason: ExpiredReason) -> Vec<Output> {
        let all = reason == ExpiredReason::ProcessLost;
        let mut out = Vec::new();
        let ids: Vec<(String, bool, bool)> = self
            .requests
            .open
            .values()
            .map(|(id, r)| {
                (
                    id.clone(),
                    r.answered.is_some(),
                    r.channel == ResponseChannel::Message,
                )
            })
            .collect();
        for (app_id, answered, message) in ids {
            if !all && message && !answered {
                continue;
            }
            if !answered {
                out.push(Output::Emit(Event::ApprovalExpired {
                    request_id: request_id(&app_id),
                    reason,
                }));
            }
            self.close(&app_id, reason);
        }
        self.requests.by_native.clear();
        out
    }
}

/// The user message that answers an async question. A message with one
/// question gets the bare answer; with several, the answer names its
/// question (`<title>: <answer>`).
fn message_answer(params: &Value, text: &str) -> String {
    let count = params.get("count").and_then(Value::as_u64).unwrap_or(1);
    match params
        .get("question")
        .and_then(|q| q.get("title"))
        .and_then(Value::as_str)
    {
        Some(title) if count > 1 => format!("{title}: {text}"),
        _ => text.to_owned(),
    }
}

/// Validates the host's answer to `item/tool/requestUserInput` against the
/// request and the schema (`ToolRequestUserInputResponse`): `{"answers":
/// {<question id>: {"answers": [<string>, ..]}}}`, only ids that the request
/// asked, at least one answer. Anything else is refused before it is written.
///
/// `params` is the request's native params (the `input` of its
/// `approval.requested`); `input` is the answer's `updated_input`. The
/// result is the native RPC reply. Pure: the core uses it before it writes
/// the reply, and the station uses it to admit an answer command before it
/// claims the request (M2 gate correction).
///
/// # Errors
/// [`ProviderError::InvalidAnswer`] for a missing or malformed answer.
pub fn validate_question_answer(params: &Value, input: Option<&Value>) -> Result<Value> {
    let bad = |m: &str| ProviderError::InvalidAnswer(m.to_owned());
    let input = input.ok_or_else(|| bad("a question needs {\"answers\": {..}}"))?;
    let answers = input
        .get("answers")
        .and_then(Value::as_object)
        .ok_or_else(|| bad("a question needs {\"answers\": {..}}"))?;
    if answers.is_empty() {
        return Err(bad("the answer map is empty"));
    }
    let asked: Vec<&str> = params
        .get("questions")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|q| q.get("id").and_then(Value::as_str))
        .collect();
    for (id, value) in answers {
        if !asked.contains(&id.as_str()) {
            return Err(bad(&format!("the request asked no question `{id}`")));
        }
        let list = value
            .get("answers")
            .and_then(Value::as_array)
            .ok_or_else(|| bad(&format!("`{id}` needs {{\"answers\": [..]}}")))?;
        if list.is_empty() || !list.iter().all(Value::is_string) {
            return Err(bad(&format!(
                "`{id}`: give at least one answer, each a string"
            )));
        }
    }
    Ok(json!({ "answers": answers }))
}
