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

use std::collections::{BTreeMap, HashMap, VecDeque};

use fleetdeck_proto::command::UserInput;
use fleetdeck_proto::event::{
    ApprovalOption, ExpiredReason, OptionKind, ProviderRefs, RequestKind, ResponseChannel,
    RunOrigin, ToolKind, ToolStatus,
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

/// How many closed requests the core keeps (NV-18). The oldest ones are
/// dropped. An open request is never dropped.
const MAX_CLOSED_REQUESTS: usize = 256;

/// JSON-RPC "invalid request": the code of the reply to a server request
/// whose id is still open (NV-21).
const DUPLICATE_ID: i64 = -32600;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
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
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
enum Closed {
    Answered,
    Expired(ExpiredReason),
}

#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
pub(super) struct Requests {
    next: u64,
    open: BTreeMap<u64, (String, OpenRequest)>,
    by_native: HashMap<String, String>,
    /// Requests that left `open`: app id → how. A late answer to a closed
    /// request is a duplicate (no write) or [`ProviderError::RequestClosed`].
    closed: HashMap<String, Closed>,
    /// The app ids of `closed`, oldest first (NV-18).
    #[serde(default)]
    closed_order: VecDeque<String>,
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

/// The semantic kind of a Codex decision id (ADR-008 §5.1). Only the two
/// amendment ids are `Amend`; any other id, including another object-valued
/// decision, is `Other`, so a coordinator fails closed.
#[must_use]
pub(super) fn option_kind(option_id: &str) -> OptionKind {
    match option_id {
        "accept" => OptionKind::AllowOnce,
        "acceptForSession" => OptionKind::AllowSession,
        "acceptWithExecpolicyAmendment" | "applyNetworkPolicyAmendment" => OptionKind::Amend,
        "decline" | "cancel" => OptionKind::Deny,
        _ => OptionKind::Other,
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
                    kind: (req.kind != RequestKind::Question).then(|| option_kind(id)),
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
        // A second request with an id that is still open would replace the
        // reverse mapping of the first one (NV-21): refuse it, with no event.
        if self
            .requests
            .by_native
            .get(&native_key(&id))
            .is_some_and(|app_id| self.requests.get(app_id).is_some())
        {
            return vec![Output::Write(rpc::reply_error(
                &id,
                DUPLICATE_ID,
                "a request with this id is still open",
            ))];
        }
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
                    input: UserInput::new(text),
                    context: Vec::new(),
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
            answered_by: answer.decision.answered_by.clone(),
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
        self.requests.closed_order.push_back(app_id.to_owned());
        while self.requests.closed_order.len() > MAX_CLOSED_REQUESTS {
            let Some(old) = self.requests.closed_order.pop_front() else {
                break;
            };
            self.requests.closed.remove(&old);
        }
        // The native id of a closed request maps to nothing (NV-18, NV-21),
        // unless a newer request has it.
        if !r.native_id.is_null() {
            let key = native_key(&r.native_id);
            if self.requests.by_native.get(&key).map(String::as_str) == Some(app_id) {
                self.requests.by_native.remove(&key);
            }
        }
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
            provider_refs: ProviderRefs::new(),
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

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::super::checkpoint::CHECKPOINT_VERSION;
    use super::super::tests::{ans, config, emits, feed, notify, ready_core, writes};
    use super::*;
    use crate::checkpoint::AdapterCheckpoint;

    /// A command approval request with the native id `id`.
    fn request(core: &mut CodexCore, id: Value) -> Vec<Output> {
        feed(
            core,
            json!({"id": id, "method": "item/commandExecution/requestApproval", "params": {
                "threadId": "thr-1", "turnId": "n1", "itemId": "c1", "command": "touch x",
                "availableDecisions": ["accept", "cancel"]
            }}),
        )
    }

    fn requested(out: &[Output]) -> RequestId {
        emits(out)
            .into_iter()
            .find_map(|e| match e {
                Event::ApprovalRequested { request_id, .. } => Some(request_id),
                _ => None,
            })
            .unwrap()
    }

    #[test]
    fn closed_requests_are_bounded_and_a_closed_request_has_no_native_mapping() {
        // NV-18.
        let mut core = ready_core();
        let mut first = None;
        for n in 0..300 {
            let id = requested(&request(&mut core, json!(n)));
            first.get_or_insert_with(|| id.clone());
            if n % 2 == 0 {
                core.answer(&ans(&id, "accept")).unwrap();
            }
            notify(&mut core, "serverRequest/resolved", json!({"requestId": n}));
            assert!(core.requests.by_native.is_empty());
        }
        assert_eq!(core.requests.closed.len(), MAX_CLOSED_REQUESTS);
        assert_eq!(core.requests.closed_order.len(), MAX_CLOSED_REQUESTS);
        assert!(core.requests.open.is_empty());
        // The oldest closed request is forgotten; the newest is still known.
        let first = first.unwrap();
        assert!(matches!(
            core.answer(&ans(&first, "accept")),
            Err(ProviderError::UnknownRequest(_))
        ));
        assert!(matches!(
            core.answer(&ans(&request_id("req-300"), "accept")),
            Err(ProviderError::RequestClosed { .. })
        ));
        assert!(
            core.answer(&ans(&request_id("req-299"), "accept"))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn a_turn_end_closes_the_requests_without_a_native_mapping_left() {
        let mut core = ready_core();
        request(&mut core, json!(1));
        request(&mut core, json!(2));
        core.expire_open(ExpiredReason::ProcessLost);
        assert!(core.requests.by_native.is_empty());
        assert_eq!(core.requests.closed.len(), 2);
        assert_eq!(core.requests.closed_order.len(), 2);
    }

    #[test]
    fn the_closed_order_survives_a_checkpoint_round_trip() {
        let mut core = ready_core();
        for n in 0..5 {
            requested(&request(&mut core, json!(n)));
            notify(&mut core, "serverRequest/resolved", json!({"requestId": n}));
        }
        let text = core.checkpoint().unwrap().to_json().unwrap();
        let stored = AdapterCheckpoint::from_json(&text).unwrap();
        let restored = CodexCore::restore(config("/w"), &stored).unwrap();
        assert_eq!(restored.requests.closed_order.len(), 5);
        assert_eq!(restored.requests.closed.len(), 5);
    }

    #[test]
    fn a_checkpoint_without_the_closed_order_loads() {
        let mut core = ready_core();
        requested(&request(&mut core, json!(1)));
        notify(&mut core, "serverRequest/resolved", json!({"requestId": 1}));
        let mut stored = core.checkpoint().unwrap();
        assert_eq!(stored.version, CHECKPOINT_VERSION);
        stored
            .state
            .pointer_mut("/requests")
            .and_then(Value::as_object_mut)
            .unwrap()
            .remove("closed_order");
        let restored = CodexCore::restore(config("/w"), &stored).unwrap();
        assert!(restored.requests.closed_order.is_empty());
        assert_eq!(restored.requests.closed.len(), 1);
    }

    #[test]
    fn a_second_request_with_an_open_id_is_refused_without_an_event() {
        // NV-21.
        let mut core = ready_core();
        let first = requested(&request(&mut core, json!(7)));
        let out = request(&mut core, json!(7));
        assert!(emits(&out).is_empty(), "{out:?}");
        let w = writes(&out);
        assert_eq!(w.len(), 1, "{w:?}");
        assert_eq!(w[0]["id"], 7);
        assert_eq!(w[0]["error"]["code"], -32600);
        assert_eq!(
            w[0]["error"]["message"],
            "a request with this id is still open"
        );
        assert_eq!(core.open_requests(), std::slice::from_ref(&first));
        // The first request still answers, and its reply names id 7 once.
        let out = core.answer(&ans(&first, "accept")).unwrap();
        let w = writes(&out);
        assert_eq!(w.len(), 1, "{w:?}");
        assert_eq!(w[0]["id"], 7);
        assert_eq!(w[0]["result"]["decision"], "accept");
        assert_eq!(core.request_for_native(&json!(7)), Some(first));
    }

    #[test]
    fn a_request_with_the_id_of_a_closed_request_is_registered() {
        let mut core = ready_core();
        requested(&request(&mut core, json!(7)));
        notify(&mut core, "serverRequest/resolved", json!({"requestId": 7}));
        let again = requested(&request(&mut core, json!(7)));
        assert_eq!(core.request_for_native(&json!(7)), Some(again));
    }
}
