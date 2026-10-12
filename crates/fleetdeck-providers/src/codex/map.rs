//! App-server replies and notifications → canonical events (ADR-002 §2.5).

use fleetdeck_proto::event::{
    BackgroundTask, ExpiredReason, FileChange, InputState, ProviderKind, ProviderRefs, ToolKind,
    ToolStatus, TurnStatus, Usage,
};
use fleetdeck_proto::{Event, NodeId, TurnId};
use serde_json::Value;

use super::core::{CodexCore, HostRequest, Output, ToolNode};
use super::truncate;

/// Notifications that the core reads but maps to no event. Every other
/// unknown method counts as an unknown frame, so a new protocol feature
/// shows up in replay.
pub const IGNORED_NOTIFICATIONS: &[&str] = &[
    "thread/started",
    "thread/status/changed",
    "thread/settings/updated",
    "thread/name/updated",
    "thread/archived",
    "thread/unarchived",
    "thread/closed",
    "thread/compacted",
    "thread/goal/updated",
    "thread/goal/cleared",
    "thread/queue/changed",
    "turn/plan/updated",
    "item/commandExecution/outputDelta",
    "item/commandExecution/terminalInteraction",
    "item/fileChange/outputDelta",
    "item/fileChange/patchUpdated",
    "item/plan/delta",
    "item/reasoning/summaryPartAdded",
    "item/mcpToolCall/progress",
    "account/updated",
    "account/rateLimits/updated",
    "account/login/completed",
    "mcpServer/startupStatus/updated",
    "mcpServer/oauthLogin/completed",
    "remoteControl/status/changed",
    "hook/started",
    "hook/completed",
    "skills/changed",
    "app/list/updated",
    "warning",
    "configWarning",
    "deprecationNotice",
    "model/rerouted",
    "model/verification",
];

/// Notifications that map to canonical events (the arms of
/// `on_notification`); a schema test checks both lists against the vendored
/// app-server schema.
pub const HANDLED_NOTIFICATIONS: &[&str] = &[
    "turn/started",
    "turn/completed",
    "item/started",
    "item/completed",
    "item/agentMessage/delta",
    "item/reasoning/summaryTextDelta",
    "item/reasoning/textDelta",
    "serverRequest/resolved",
    "turn/diff/updated",
    "thread/tokenUsage/updated",
    "error",
];

/// The longest input summary of a tool call.
const SUMMARY_CHARS: usize = 200;

pub(super) fn refs(pairs: &[(&str, Option<&str>)]) -> ProviderRefs {
    pairs
        .iter()
        .filter_map(|(k, v)| v.map(|v| ((*k).to_owned(), Value::from(v))))
        .collect()
}

pub(super) fn str_at<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str)
}

fn tool_kind(item_type: &str) -> Option<ToolKind> {
    Some(match item_type {
        "commandExecution" => ToolKind::Shell,
        "fileChange" => ToolKind::Edit,
        "mcpToolCall" => ToolKind::Mcp,
        "webSearch" => ToolKind::Web,
        "collabAgentToolCall" | "subAgentActivity" => ToolKind::Subagent,
        "dynamicToolCall" | "imageView" | "imageGeneration" => ToolKind::Other,
        _ => return None,
    })
}

/// Paths named by a `fileChange` item.
fn change_paths(item: &Value) -> Vec<String> {
    item.get("changes")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|c| str_at(c, "path"))
        .map(str::to_owned)
        .collect()
}

/// Title and input summary of a tool item (DL-063).
/// The item type of the tool node that the core opens for a
/// `item/tool/requestUserInput` question; Codex 0.160.0 starts no item for
/// it (DL-068).
pub(super) const QUESTION_ITEM: &str = "requestUserInput";

fn tool_title(item: &Value, item_type: &str) -> (String, String) {
    let text = |k: &str| str_at(item, k).unwrap_or_default().to_owned();
    let (title, summary) = match item_type {
        "commandExecution" => ("Shell".to_owned(), text("command")),
        "fileChange" => ("Edit".to_owned(), change_paths(item).join(", ")),
        "mcpToolCall" => (
            format!("{}.{}", text("server"), text("tool")),
            item.get("arguments")
                .map(Value::to_string)
                .unwrap_or_default(),
        ),
        "webSearch" => ("Web search".to_owned(), text("query")),
        "collabAgentToolCall" => (
            str_at(item, "tool").unwrap_or("Agent").to_owned(),
            text("prompt"),
        ),
        "subAgentActivity" => ("Subagent".to_owned(), text("agentPath")),
        // The node of a `requestUserInput` question (DL-068).
        QUESTION_ITEM => ("Question".to_owned(), text("question")),
        "dynamicToolCall" => (
            str_at(item, "tool").unwrap_or(item_type).to_owned(),
            item.get("arguments")
                .map(Value::to_string)
                .unwrap_or_default(),
        ),
        other => (other.to_owned(), String::new()),
    };
    (title, truncate(&summary, SUMMARY_CHARS))
}

fn change_kind(change: &Value) -> FileChange {
    match change.pointer("/kind/type").and_then(Value::as_str) {
        Some("add") => FileChange::Added,
        Some("delete") => FileChange::Deleted,
        _ => FileChange::Modified,
    }
}

/// Paths of a unified diff (`diff --git a/X b/X`).
fn diff_paths(diff: &str) -> Vec<String> {
    diff.lines()
        .filter_map(|l| l.strip_prefix("diff --git a/"))
        .filter_map(|l| l.split(" b/").next())
        .map(str::to_owned)
        .collect()
}

fn rpc_error(code: i64, message: String) -> Output {
    Output::Emit(Event::Error {
        code: format!("rpc_{code}"),
        message,
        retry_at: None,
    })
}

impl CodexCore {
    pub(super) fn on_response(
        &mut self,
        id: &Value,
        result: Result<Value, (i64, String)>,
    ) -> Vec<Output> {
        let Some(kind) = id.as_str().and_then(|i| self.host_requests.remove(i)) else {
            self.unknown_frames += 1;
            return Vec::new();
        };
        match (kind, result) {
            (HostRequest::Initialize, Ok(r)) => {
                // `userAgent` is "<client>/<codex version> (...)".
                self.provider_version = str_at(&r, "userAgent")
                    .and_then(|ua| ua.split_whitespace().next())
                    .and_then(|first| first.rsplit('/').next())
                    .map(str::to_owned);
                self.initialized = true;
                vec![Output::Write(super::rpc::notification("initialized"))]
            }
            (
                HostRequest::ThreadStart | HostRequest::ThreadResume | HostRequest::ThreadFork,
                Ok(r),
            ) => self.on_thread(&r),
            (HostRequest::TurnStart { input_id }, Ok(r)) => self.on_turn_accepted(&input_id, &r),
            (HostRequest::TurnSteer { input_id }, Ok(_)) => {
                let turn = self.turn.clone();
                self.set_input_state(&input_id, InputState::Received, turn)
            }
            (HostRequest::TurnStart { input_id }, Err((code, message))) => {
                self.turn_requested = false;
                self.turn_input = None;
                let mut out = self.set_input_state(&input_id, InputState::Refused, None);
                out.push(rpc_error(code, message));
                out.extend(self.refuse_held_interrupt("the turn did not start"));
                out.extend(self.dispatch_follow_up());
                out
            }
            (HostRequest::TurnSteer { input_id }, Err((code, message))) => {
                let mut out = self.set_input_state(&input_id, InputState::Refused, None);
                out.push(rpc_error(code, message));
                out
            }
            (HostRequest::Interrupt(control_id), result) => {
                let accepted = result.is_ok();
                if !accepted {
                    // A refused interrupt does not interrupt the turn.
                    self.interrupt_requested = false;
                }
                vec![Output::Emit(Event::InterruptResult {
                    control_id,
                    accepted,
                    message: result.err().map(|(_, m)| m),
                })]
            }
            (HostRequest::BackgroundList, Ok(r)) => {
                let tasks: Vec<BackgroundTask> = r
                    .get("data")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(|t| str_at(t, "processId"))
                    .map(|id| BackgroundTask {
                        task_id: id.to_owned(),
                        task_kind: "background_terminal".to_owned(),
                    })
                    .collect();
                if tasks == self.background {
                    return Vec::new();
                }
                self.background.clone_from(&tasks);
                vec![Output::Emit(Event::BackgroundUpdated { tasks })]
            }
            (HostRequest::Rollback, result) => self.on_rollback(result),
            (_, Err((code, message))) => vec![rpc_error(code, message)],
        }
    }

    fn on_thread(&mut self, r: &Value) -> Vec<Output> {
        let thread = r.get("thread").cloned().unwrap_or_default();
        self.thread_id = str_at(&thread, "id").map(str::to_owned);
        let model = str_at(r, "model")
            .or_else(|| str_at(&thread, "model"))
            .map(str::to_owned);
        self.thread_model.clone_from(&model);
        let cwd = self
            .spec
            .cwd
            .clone()
            .unwrap_or_else(|| self.config.cwd.clone());
        vec![Output::Emit(Event::SessionStarted {
            provider: ProviderKind::Codex,
            provider_instance: self.config.provider_instance.clone(),
            provider_version: self.provider_version.clone(),
            model,
            cwd,
            mode: self.mode(),
            capabilities: Box::new(self.config.capabilities.clone()),
            resumed: self.config.resumed,
            provider_refs: refs(&[("thread_id", self.thread_id.as_deref())]),
            title: None,
            branch: None,
        })]
    }

    fn on_turn_accepted(&mut self, input_id: &fleetdeck_proto::InputId, r: &Value) -> Vec<Output> {
        self.turn_requested = false;
        let mut out = Vec::new();
        if let Some(m) = self.pending_mode.take() {
            let name = m.name();
            self.mode = Some(m);
            out.push(Output::Emit(Event::ModeChanged { mode: name }));
        }
        if let Some(model) = self.pending_model.take() {
            self.thread_model = Some(model.clone());
            out.push(Output::Emit(Event::ModelChanged { model }));
        }
        if let Some(native) = r.pointer("/turn/id").and_then(Value::as_str) {
            out.extend(self.ensure_turn(native));
        }
        // Accepted, not confirmed: the reply names the turn that will consume
        // the input; `Started` needs the input's own `userMessage` item.
        let turn = self.turn.clone();
        out.extend(self.set_input_state(input_id, InputState::Received, turn));
        out
    }

    /// `turn/started` of the own thread: the turn runs, so a held interrupt
    /// can go out now (trap 3).
    fn on_turn_started(&mut self, native: &str) -> Vec<Output> {
        let mut out = self.ensure_turn(native);
        if self.native_turn.as_deref() != Some(native) {
            return out;
        }
        self.native_started = true;
        if let Some(control_id) = self.interrupt_pending.clone()
            && let Some(thread) = self.thread_id.clone()
        {
            out.extend(self.send_interrupt(&thread, native, control_id));
        }
        out
    }

    pub(super) fn on_notification(&mut self, method: &str, p: &Value) -> Vec<Output> {
        match method {
            // A sub-agent runs its own turns on a child thread; they never
            // start, replace or end the session's turn.
            "turn/started" | "turn/completed" if !self.own_thread(p) => Vec::new(),
            "turn/started" => match p.pointer("/turn/id").and_then(Value::as_str) {
                Some(native) => self.on_turn_started(native),
                None => Vec::new(),
            },
            "turn/completed" => self.on_turn_completed(p),
            "item/started" => self.on_item(p, false),
            "item/completed" => self.on_item(p, true),
            "item/agentMessage/delta" => self.delta(p, false),
            "item/reasoning/summaryTextDelta" | "item/reasoning/textDelta" => self.delta(p, true),
            "serverRequest/resolved" => self.on_resolved(p),
            "turn/diff/updated" => self.on_diff(p),
            "thread/tokenUsage/updated" if self.own_thread(p) => {
                let last = p.pointer("/tokenUsage/last").cloned().unwrap_or_default();
                vec![Output::Emit(Event::UsageUpdated {
                    usage: Usage {
                        input_tokens: last.get("inputTokens").and_then(Value::as_u64),
                        output_tokens: last.get("outputTokens").and_then(Value::as_u64),
                        cost_usd: None,
                    },
                    rate_limits: Vec::new(),
                })]
            }
            "thread/tokenUsage/updated" => Vec::new(),
            "error" => {
                let info = p.pointer("/error/codexErrorInfo");
                let code = match info.and_then(Value::as_str) {
                    // Trap 7: usage limits are their own state.
                    Some("usageLimitExceeded") => "usage_limit".to_owned(),
                    Some(other) => other.to_owned(),
                    None => "provider_error".to_owned(),
                };
                vec![Output::Emit(Event::Error {
                    code,
                    message: p
                        .pointer("/error/message")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                    retry_at: None,
                })]
            }
            m if IGNORED_NOTIFICATIONS.contains(&m) => Vec::new(),
            _ => {
                self.unknown_frames += 1;
                Vec::new()
            }
        }
    }

    /// Is this frame about the session's own thread (not a sub-agent thread)?
    fn own_thread(&self, p: &Value) -> bool {
        match (str_at(p, "threadId"), self.thread_id.as_deref()) {
            (Some(t), Some(own)) => t == own,
            _ => true,
        }
    }

    fn delta(&mut self, p: &Value, reasoning: bool) -> Vec<Output> {
        let (Some(turn_id), Some(text)) = (self.turn.clone(), str_at(p, "delta")) else {
            return Vec::new();
        };
        // Sub-agent deltas stay out: their completed messages carry the text.
        if !self.own_thread(p) || text.is_empty() {
            return Vec::new();
        }
        let text = text.to_owned();
        vec![Output::Emit(if reasoning {
            Event::ReasoningDelta { turn_id, text }
        } else {
            Event::MessageDelta {
                turn_id,
                parent_node_id: None,
                text,
            }
        })]
    }

    fn on_item(&mut self, p: &Value, completed: bool) -> Vec<Output> {
        let item = p.get("item").cloned().unwrap_or_default();
        let (Some(item_type), Some(item_id)) = (str_at(&item, "type"), str_at(&item, "id")) else {
            self.unknown_frames += 1;
            return Vec::new();
        };
        let own = self.own_thread(p);
        match item_type {
            "userMessage" if !completed && own => match str_at(&item, "clientId") {
                Some(client) => {
                    let turn = self.turn.clone();
                    let id = fleetdeck_proto::InputId::new(client);
                    self.set_input_state(&id, InputState::Started, turn)
                }
                None => Vec::new(),
            },
            "agentMessage" if completed => self.agent_message(p, &item, item_id, own),
            "plan" if completed && own => match str_at(&item, "text") {
                Some(text) => vec![Output::Emit(Event::PlanProposed {
                    plan_id: self.ids.plan(),
                    turn_id: self.turn.clone(),
                    markdown: text.to_owned(),
                })],
                None => Vec::new(),
            },
            t => match tool_kind(t) {
                Some(_) if completed => self.tool_completed(p, &item, item_id, t),
                Some(kind) => self.tool_started(p, &item, item_id, t, kind),
                None => Vec::new(),
            },
        }
    }

    /// A completed agent message. A sub-agent's message belongs to the tool
    /// call that spawned its thread (D15 child timelines).
    fn agent_message(&mut self, p: &Value, item: &Value, item_id: &str, own: bool) -> Vec<Output> {
        let mut out = Vec::new();
        let parent: Option<NodeId> = if own {
            None
        } else {
            match str_at(p, "threadId").and_then(|t| self.child_threads.get(t)) {
                Some(node) => Some(node.clone()),
                // A thread that no tool call named: not this session's.
                None => return out,
            }
        };
        // An empty commentary message before a tool call carries nothing.
        if let (Some(turn_id), Some(text)) = (self.turn.clone(), str_at(item, "text"))
            && !text.is_empty()
        {
            out.push(Output::Emit(Event::MessageCompleted {
                turn_id,
                parent_node_id: parent,
                text: text.to_owned(),
            }));
        }
        if own
            && str_at(item, "delivery") == Some("async")
            && let Some(qs) = item.get("questions").and_then(Value::as_array)
        {
            let qs = qs.clone();
            out.extend(self.on_async_questions(item_id, &qs));
        }
        out
    }

    pub(super) fn tool_started(
        &mut self,
        p: &Value,
        item: &Value,
        item_id: &str,
        item_type: &str,
        kind: ToolKind,
    ) -> Vec<Output> {
        if self.tools.contains_key(item_id) {
            return Vec::new();
        }
        let node_id = NodeId(self.ids.next("node"));
        let parent_node_id = str_at(p, "threadId")
            .and_then(|t| self.child_threads.get(t))
            .cloned();
        self.link_child_threads(item, item_type, &node_id);
        let order = self.tool_order();
        self.tools.insert(
            item_id.to_owned(),
            ToolNode {
                node_id: node_id.clone(),
                order,
                item_type: item_type.to_owned(),
                done: false,
                denied: false,
                background: false,
            },
        );
        let (title, input_summary) = tool_title(item, item_type);
        vec![Output::Emit(Event::ToolStarted {
            node_id,
            parent_node_id,
            tool_kind: kind,
            title,
            input_summary,
            files: change_paths(item),
            provider_refs: refs(&[("item_id", Some(item_id)), ("item_type", Some(item_type))]),
        })]
    }

    fn tool_completed(
        &mut self,
        p: &Value,
        item: &Value,
        item_id: &str,
        item_type: &str,
    ) -> Vec<Output> {
        let mut out = Vec::new();
        if !self.tools.contains_key(item_id)
            && let Some(kind) = tool_kind(item_type)
        {
            // A completed item without a started item: start it first.
            out.extend(self.tool_started(p, item, item_id, item_type, kind));
        }
        let Some(node) = self.tools.get_mut(item_id) else {
            return out;
        };
        if node.done {
            return out;
        }
        node.done = true;
        let status = match str_at(item, "status") {
            Some("declined") => ToolStatus::Denied,
            Some("failed") => ToolStatus::Failed,
            _ if node.denied => ToolStatus::Denied,
            _ => ToolStatus::Ok,
        };
        let node_id = node.node_id.clone();
        // `spawnAgent` names its child thread only in the completed item.
        self.link_child_threads(item, item_type, &node_id);
        let files = change_paths(item);
        if item_type == "fileChange" && status == ToolStatus::Ok {
            for change in item
                .get("changes")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                if let Some(path) = str_at(change, "path") {
                    out.extend(self.file_changed(path, change_kind(change), Some(&node_id)));
                }
            }
        }
        let exit_code = item
            .get("exitCode")
            .and_then(Value::as_i64)
            .and_then(|c| i32::try_from(c).ok());
        out.push(Output::Emit(Event::ToolCompleted {
            node_id,
            status,
            files,
            exit_code,
        }));
        out
    }

    /// A `collabAgentToolCall` names the sub-agent threads it started or waits
    /// for. Their items get this node as parent. The first parent wins.
    fn link_child_threads(&mut self, item: &Value, item_type: &str, node_id: &NodeId) {
        if item_type != "collabAgentToolCall" {
            return;
        }
        for t in item
            .get("receiverThreadIds")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
        {
            self.child_threads
                .entry(t.to_owned())
                .or_insert_with(|| node_id.clone());
        }
    }

    /// One `file.changed` per path and turn. `fileChange` items carry absolute
    /// paths and `turn/diff/updated` carries paths relative to the thread cwd,
    /// so relative paths are joined with the cwd before the duplicate check.
    fn file_changed(
        &mut self,
        path: &str,
        change: FileChange,
        node_id: Option<&NodeId>,
    ) -> Vec<Output> {
        let cwd = self
            .spec
            .cwd
            .clone()
            .unwrap_or_else(|| self.config.cwd.clone());
        // A path under the cwd counts as absolute; this also holds for a
        // scrubbed fixture where the cwd is the placeholder `<workdir>`.
        let path = if !path.starts_with('/') && !path.starts_with(cwd.as_str()) {
            format!("{}/{path}", cwd.trim_end_matches('/'))
        } else {
            path.to_owned()
        };
        if self.files_seen.contains(&path) {
            return Vec::new();
        }
        self.files_seen.push(path.clone());
        vec![Output::Emit(Event::FileChanged {
            path,
            change,
            node_id: node_id.cloned(),
        })]
    }

    /// `turn/diff/updated`: keeps the aggregated diff; a path that no
    /// `fileChange` item reported yet becomes a `file.changed`.
    fn on_diff(&mut self, p: &Value) -> Vec<Output> {
        let Some(diff) = str_at(p, "diff") else {
            return Vec::new();
        };
        if !self.own_thread(p) {
            return Vec::new();
        }
        self.turn_diff = Some(diff.to_owned());
        diff_paths(diff)
            .iter()
            .flat_map(|path| self.file_changed(path, FileChange::Modified, None))
            .collect()
    }

    fn on_turn_completed(&mut self, p: &Value) -> Vec<Output> {
        let native = p.pointer("/turn/id").and_then(Value::as_str);
        if native.is_some() && native != self.native_turn.as_deref() {
            // Completion of a turn this core never saw start (another client).
            return Vec::new();
        }
        let mut out = self.expire_open(ExpiredReason::TurnEnded);
        let status = match p.pointer("/turn/status").and_then(Value::as_str) {
            Some("completed") => TurnStatus::Done,
            Some("interrupted") => TurnStatus::Interrupted,
            _ => TurnStatus::Failed,
        };
        if status == TurnStatus::Done {
            // A command that still runs when the turn completes became a
            // background terminal. Its `item/completed` arrives later with
            // the real status, so it stays open; every other tool closes
            // (trap 6).
            self.keep_background_commands();
        }
        out.extend(self.close_open_tools());
        if let Some(turn_id) = self.turn.clone() {
            out.extend(self.settle_inputs(&turn_id, status));
        }
        out.extend(self.refuse_held_interrupt("the turn ended first"));
        out.extend(self.end_turn(status, None));
        self.native_turn = None;
        self.native_started = false;
        self.interrupt_requested = false;
        out.extend(self.dispatch_follow_up());
        out
    }

    /// The inputs that a finished turn consumed end with the turn.
    fn settle_inputs(&mut self, turn_id: &TurnId, status: TurnStatus) -> Vec<Output> {
        let consumed: Vec<_> = self
            .inputs
            .iter()
            .filter(|i| i.turn.as_ref() == Some(turn_id))
            .map(|i| i.id.clone())
            .collect();
        let final_state = if status == TurnStatus::Done {
            InputState::Completed
        } else {
            InputState::Cancelled
        };
        consumed
            .iter()
            .flat_map(|id| self.set_input_state(id, final_state, None))
            .collect()
    }

    /// Marks the open `commandExecution` nodes as background work.
    fn keep_background_commands(&mut self) {
        for node in self.tools.values_mut() {
            if !node.done && node.item_type == "commandExecution" {
                node.background = true;
            }
        }
    }

    /// Closes every open tool that is not background work (trap 6).
    pub(super) fn close_open_tools(&mut self) -> Vec<Output> {
        self.close_tools(false)
    }

    /// Closes every open tool, background work too (the process ended).
    pub(super) fn close_all_tools(&mut self) -> Vec<Output> {
        self.close_tools(true)
    }

    fn close_tools(&mut self, background_too: bool) -> Vec<Output> {
        let mut open: Vec<&mut ToolNode> = self
            .tools
            .values_mut()
            .filter(|n| !n.done && (background_too || !n.background))
            .collect();
        open.sort_by_key(|n| n.order);
        open.into_iter()
            .map(|n| {
                n.done = true;
                Output::Emit(Event::ToolCompleted {
                    node_id: n.node_id.clone(),
                    status: if n.denied {
                        ToolStatus::Denied
                    } else {
                        ToolStatus::Failed
                    },
                    files: Vec::new(),
                    exit_code: None,
                })
            })
            .collect()
    }
}
