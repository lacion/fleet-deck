//! App-server replies and notifications → canonical events.

use serde_json::{Map, Value};
use spike_canon::{
    BackgroundTask, CanonEvent, FileChange, InputState, ToolKind, ToolStatus, TurnStatus, Usage,
};

use super::{CodexCore, HostRequest, Output, ToolNode};

/// Notifications that the spike reads but maps to no event. Every other
/// unknown method counts as an unknown frame, so a new protocol feature
/// shows up in replay (`unknown=N`).
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

/// Notifications that map to canonical events (the `match` arms of
/// `on_notification`); the schema test checks both lists against the vendored
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

pub(super) fn refs(pairs: &[(&str, Option<&str>)]) -> Map<String, Value> {
    pairs
        .iter()
        .filter_map(|(k, v)| v.map(|v| ((*k).to_owned(), Value::from(v))))
        .collect()
}

fn str_at<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
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

fn tool_title(item: &Value, item_type: &str) -> String {
    str_at(item, "command")
        .or_else(|| str_at(item, "tool"))
        .or_else(|| str_at(item, "query"))
        .or_else(|| str_at(item, "agentPath"))
        .unwrap_or(item_type)
        .to_owned()
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

impl CodexCore {
    pub(crate) fn on_response(
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
                vec![Output::Write(crate::rpc::notification("initialized"))]
            }
            (
                HostRequest::ThreadStart | HostRequest::ThreadResume | HostRequest::ThreadFork,
                Ok(r),
            ) => {
                let thread = r.get("thread").cloned().unwrap_or_default();
                self.thread_id = str_at(&thread, "id").map(str::to_owned);
                let model = str_at(&r, "model")
                    .or_else(|| str_at(&thread, "model"))
                    .map(str::to_owned);
                self.thread_model.clone_from(&model);
                vec![Output::Emit(CanonEvent::SessionStarted {
                    provider: "codex".into(),
                    provider_version: self.provider_version.clone(),
                    model,
                    mode: self.mode(),
                    provider_refs: refs(&[("thread_id", self.thread_id.as_deref())]),
                })]
            }
            (HostRequest::TurnStart { input_id }, Ok(r)) => self.on_turn_accepted(&input_id, &r),
            (HostRequest::TurnSteer { input_id }, Ok(_)) => {
                let turn = self.turn.clone();
                self.set_input_state(&input_id, InputState::Received, turn)
            }
            (
                HostRequest::TurnStart { input_id } | HostRequest::TurnSteer { input_id },
                Err((code, message)),
            ) => {
                self.turn_requested = false;
                let mut out = self.set_input_state(&input_id, InputState::Refused, None);
                out.push(Output::Emit(CanonEvent::Error {
                    code: format!("rpc_{code}"),
                    message,
                }));
                out.extend(self.dispatch_follow_up());
                out
            }
            (HostRequest::Interrupt, Ok(_)) => Vec::new(),
            (HostRequest::BackgroundList, Ok(r)) => {
                let tasks = r
                    .get("data")
                    .and_then(Value::as_array)
                    .map(|data| {
                        data.iter()
                            .filter_map(|t| str_at(t, "processId"))
                            .map(|id| BackgroundTask {
                                task_id: id.to_owned(),
                                task_kind: "background_terminal".into(),
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                vec![Output::Emit(CanonEvent::BackgroundUpdated { tasks })]
            }
            (_, Err((code, message))) => vec![Output::Emit(CanonEvent::Error {
                code: format!("rpc_{code}"),
                message,
            })],
        }
    }

    fn on_turn_accepted(&mut self, input_id: &str, r: &Value) -> Vec<Output> {
        self.turn_requested = false;
        let mut out = Vec::new();
        if let Some(m) = self.pending_mode.take() {
            let name = m.name();
            self.mode = Some(m);
            out.push(Output::Emit(CanonEvent::ModeChanged { mode: name }));
        }
        let native = r.pointer("/turn/id").and_then(Value::as_str);
        if let Some(native) = native {
            out.extend(self.ensure_turn(native));
        }
        // Accepted, not confirmed: the reply names the turn that will consume
        // the input; `Started` needs the input's own `userMessage` item.
        let turn = self.turn.clone();
        out.extend(self.set_input_state(input_id, InputState::Received, turn));
        // A held interrupt waits for `turn/started` (see `native_started`).
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
        if self.interrupt_pending
            && let Some(thread) = self.thread_id.clone()
        {
            out.extend(self.send_interrupt(&thread, native));
        }
        out
    }

    /// Starts the canonical turn for a native turn id, once.
    fn ensure_turn(&mut self, native: &str) -> Vec<Output> {
        if self.native_turn.as_deref() == Some(native) {
            return Vec::new();
        }
        if self.turn.is_some() {
            // Another native turn while one runs: never replace the running
            // turn (its completion would be lost). Count it as unexpected.
            self.unknown_frames += 1;
            return Vec::new();
        }
        let turn_id = self.ids.next("turn");
        self.turn = Some(turn_id.clone());
        self.native_turn = Some(native.to_owned());
        self.files_seen.clear();
        self.turn_diff = None;
        vec![Output::Emit(CanonEvent::TurnStarted {
            turn_id,
            provider_refs: refs(&[
                ("thread_id", self.thread_id.as_deref()),
                ("turn_id", Some(native)),
            ]),
        })]
    }

    pub(crate) fn set_input_state(
        &mut self,
        input_id: &str,
        state: InputState,
        turn: Option<String>,
    ) -> Vec<Output> {
        let Some(entry) = self.inputs.iter_mut().find(|i| i.id == input_id) else {
            return Vec::new();
        };
        if entry.state == Some(state) {
            return Vec::new();
        }
        // A late `Received` never moves an input back.
        if state == InputState::Received && entry.state.is_some() {
            return Vec::new();
        }
        entry.state = Some(state);
        if turn.is_some() {
            entry.turn = turn;
        }
        vec![Output::Emit(CanonEvent::InputUpdated {
            input_id: input_id.to_owned(),
            turn_id: entry.turn.clone(),
            state,
        })]
    }

    pub(crate) fn dispatch_follow_up(&mut self) -> Vec<Output> {
        if self.turn.is_some() || self.turn_requested {
            return Vec::new();
        }
        let (Some(thread), Some((input_id, text))) =
            (self.thread_id.clone(), self.follow_ups.pop_front())
        else {
            return Vec::new();
        };
        self.turn_start(&thread, &text, &input_id)
    }

    pub(crate) fn on_notification(&mut self, method: &str, p: &Value) -> Vec<Output> {
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
            "thread/tokenUsage/updated" => {
                let last = p.pointer("/tokenUsage/last").cloned().unwrap_or_default();
                vec![Output::Emit(CanonEvent::UsageUpdated {
                    usage: Usage {
                        input_tokens: last.get("inputTokens").and_then(Value::as_u64),
                        output_tokens: last.get("outputTokens").and_then(Value::as_u64),
                        cost_usd: None,
                    },
                })]
            }
            "error" => {
                let info = p.pointer("/error/codexErrorInfo");
                let code = match info.and_then(Value::as_str) {
                    // Trap 7: usage limits are their own state.
                    Some("usageLimitExceeded") => "usage_limit".to_owned(),
                    Some(other) => other.to_owned(),
                    None => "provider_error".to_owned(),
                };
                vec![Output::Emit(CanonEvent::Error {
                    code,
                    message: p
                        .pointer("/error/message")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
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
        if !self.own_thread(p) || text.is_empty() {
            return Vec::new();
        }
        let text = text.to_owned();
        vec![Output::Emit(if reasoning {
            CanonEvent::ReasoningDelta { turn_id, text }
        } else {
            CanonEvent::MessageDelta { turn_id, text }
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
                    self.set_input_state(client, InputState::Started, turn)
                }
                None => Vec::new(),
            },
            "agentMessage" if completed && own => {
                let mut out = Vec::new();
                // An empty commentary message before a tool call carries nothing.
                if let (Some(turn_id), Some(text)) = (self.turn.clone(), str_at(&item, "text"))
                    && !text.is_empty()
                {
                    out.push(Output::Emit(CanonEvent::MessageCompleted {
                        turn_id,
                        text: text.to_owned(),
                    }));
                }
                if str_at(&item, "delivery") == Some("async")
                    && let Some(qs) = item.get("questions").and_then(Value::as_array)
                {
                    let qs = qs.clone();
                    out.extend(self.on_async_questions(item_id, &qs));
                }
                out
            }
            "plan" if completed && own => match str_at(&item, "text") {
                Some(text) => vec![Output::Emit(CanonEvent::PlanProposed {
                    markdown: text.to_owned(),
                })],
                None => Vec::new(),
            },
            t => match tool_kind(t) {
                Some(_) if completed => self.tool_completed(&item, item_id, t),
                Some(kind) => self.tool_started(p, &item, item_id, t, kind),
                None => Vec::new(),
            },
        }
    }

    fn tool_started(
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
        let node_id = self.ids.next("node");
        let parent_node_id = str_at(p, "threadId")
            .and_then(|t| self.child_threads.get(t))
            .cloned();
        self.link_child_threads(item, item_type, &node_id);
        self.tools.insert(
            item_id.to_owned(),
            ToolNode {
                node_id: node_id.clone(),
                item_type: item_type.to_owned(),
                done: false,
                denied: false,
                background: false,
            },
        );
        vec![Output::Emit(CanonEvent::ToolStarted {
            node_id,
            parent_node_id,
            tool_kind: kind,
            title: tool_title(item, item_type),
            provider_refs: refs(&[("item_id", Some(item_id)), ("item_type", Some(item_type))]),
        })]
    }

    fn tool_completed(&mut self, item: &Value, item_id: &str, item_type: &str) -> Vec<Output> {
        let mut out = Vec::new();
        if !self.tools.contains_key(item_id) {
            // A completed item without a started item: start it first.
            if let Some(kind) = tool_kind(item_type) {
                out.extend(self.tool_started(&Value::Null, item, item_id, item_type, kind));
            }
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
        if item_type == "fileChange" && status == ToolStatus::Ok {
            for change in item
                .get("changes")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                if let Some(path) = str_at(change, "path") {
                    out.extend(self.file_changed(path, change_kind(change)));
                }
            }
        }
        out.push(Output::Emit(CanonEvent::ToolCompleted { node_id, status }));
        out
    }

    /// A `collabAgentToolCall` names the sub-agent threads it started or waits
    /// for. Their tool items get this node as parent. The first parent wins.
    fn link_child_threads(&mut self, item: &Value, item_type: &str, node_id: &str) {
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
                .or_insert_with(|| node_id.to_owned());
        }
    }

    /// One `file.changed` per path and turn. `fileChange` items carry absolute
    /// paths and `turn/diff/updated` carries paths relative to the thread cwd,
    /// so relative paths are joined with the cwd before the duplicate check.
    fn file_changed(&mut self, path: &str, change: FileChange) -> Vec<Output> {
        // A path under the thread cwd counts as absolute; this also holds for a
        // scrubbed fixture where the cwd is the placeholder `<workdir>`.
        let path = match &self.spec.cwd {
            Some(cwd) if !path.starts_with('/') && !path.starts_with(cwd.as_str()) => {
                format!("{}/{path}", cwd.trim_end_matches('/'))
            }
            _ => path.to_owned(),
        };
        if self.files_seen.contains(&path) {
            return Vec::new();
        }
        self.files_seen.push(path.clone());
        vec![Output::Emit(CanonEvent::FileChanged { path, change })]
    }

    /// `turn/diff/updated`: keeps the aggregated diff; a path that no
    /// `fileChange` item reported yet becomes a `file.changed`.
    fn on_diff(&mut self, p: &Value) -> Vec<Output> {
        let Some(diff) = str_at(p, "diff") else {
            return Vec::new();
        };
        self.turn_diff = Some(diff.to_owned());
        diff_paths(diff)
            .iter()
            .flat_map(|path| self.file_changed(path, FileChange::Modified))
            .collect()
    }

    fn on_turn_completed(&mut self, p: &Value) -> Vec<Output> {
        let native = p.pointer("/turn/id").and_then(Value::as_str);
        if native.is_some() && native != self.native_turn.as_deref() {
            // Completion of a turn this core never saw start (another client).
            return Vec::new();
        }
        let mut out = self.expire_open("turn_ended");
        let status = match p.pointer("/turn/status").and_then(Value::as_str) {
            Some("completed") => TurnStatus::Done,
            Some("interrupted") => TurnStatus::Interrupted,
            _ => TurnStatus::Failed,
        };
        if status == TurnStatus::Done {
            // A command that is still running when the turn completes became a
            // background terminal (`thread/backgroundTerminals/list` names its
            // item id). Its `item/completed` arrives later with the real status,
            // so it stays open; every other open tool closes (trap 6).
            self.keep_background_commands();
        }
        out.extend(self.close_open_tools());
        if let Some(turn_id) = self.turn.take() {
            let consumed: Vec<String> = self
                .inputs
                .iter()
                .filter(|i| i.turn.as_deref() == Some(turn_id.as_str()))
                .map(|i| i.id.clone())
                .collect();
            let final_state = if status == TurnStatus::Done {
                InputState::Completed
            } else {
                InputState::Cancelled
            };
            for id in consumed {
                out.extend(self.set_input_state(&id, final_state, None));
            }
            out.push(Output::Emit(CanonEvent::TurnCompleted {
                turn_id,
                status,
                usage: None,
            }));
        }
        self.native_turn = None;
        self.native_started = false;
        self.interrupt_requested = false;
        self.interrupt_pending = false;
        out.extend(self.dispatch_follow_up());
        out
    }

    /// Marks the open `commandExecution` nodes as background work.
    fn keep_background_commands(&mut self) {
        for node in self.tools.values_mut() {
            if !node.done && node.item_type == "commandExecution" {
                node.background = true;
            }
        }
    }

    /// Closes every open tool that is not background work (trap 6). Background
    /// commands stay open until their `item/completed` arrives or the process
    /// ends (`on_exit` calls [`Self::close_all_tools`]).
    pub(crate) fn close_open_tools(&mut self) -> Vec<Output> {
        self.close_tools(false)
    }

    /// Closes every open tool, background work too (the process ended).
    pub(crate) fn close_all_tools(&mut self) -> Vec<Output> {
        self.close_tools(true)
    }

    fn close_tools(&mut self, background_too: bool) -> Vec<Output> {
        let mut open: Vec<_> = self
            .tools
            .values_mut()
            .filter(|n| !n.done && (background_too || !n.background))
            .collect();
        open.sort_by(|a, b| a.node_id.cmp(&b.node_id));
        open.into_iter()
            .map(|n| {
                n.done = true;
                Output::Emit(CanonEvent::ToolCompleted {
                    node_id: n.node_id.clone(),
                    status: if n.denied {
                        ToolStatus::Denied
                    } else {
                        ToolStatus::Failed
                    },
                })
            })
            .collect()
    }
}
