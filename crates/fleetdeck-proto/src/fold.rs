//! The session projection (PLAN §6.4, §7.2): [`SessionSummary::apply`] folds
//! one event into a summary. The station updates its `sessions` projection
//! with it in the event's transaction; the deck's read model uses the same
//! fold, so both sides agree on a session's state.
//!
//! Rules:
//! - `usage.updated` is the only usage source (`turn.completed` repeats the
//!   turn's usage); a part that is `None` stays unknown, never zero.
//! - A run or turn closes only when its own id completes: a child completion
//!   never closes the parent (L7).
//! - Activity: `ended` after the process ended, else `needs_you` with an open
//!   request, else `working` during a run, else `background` while
//!   background work runs, else `idle`.
//! - An unknown event kind changes only the sequence and the time (L16).

use crate::event::{Event, EventEnvelope, SessionState, Usage};
use crate::session::{
    Activity, MAX_RUNNING_TOOLS, OpenRequest, RunningTool, SUBAGENT_TASK_KINDS, SessionError,
    SessionSummary,
};

impl SessionSummary {
    /// Folds one event. Events at or below the summary's `session_seq` are
    /// ignored, so a replay of an applied event changes nothing.
    pub fn apply(&mut self, envelope: &EventEnvelope) {
        if envelope.session_seq <= self.session_seq {
            return;
        }
        self.session_seq = envelope.session_seq;
        self.updated_at = envelope.at;
        if let Some(event) = envelope.event.known() {
            self.fold(event, envelope.at);
        }
        self.activity = self.derive_activity();
    }

    fn fold(&mut self, event: &Event, at: i64) {
        match event {
            Event::SessionStarted {
                provider,
                provider_instance,
                provider_version,
                model,
                cwd,
                mode,
                capabilities,
                title,
                branch,
                ..
            } => {
                if title.is_some() {
                    self.title.clone_from(title);
                }
                if branch.is_some() {
                    self.branch.clone_from(branch);
                }
                self.provider = Some(*provider);
                self.provider_instance = Some(provider_instance.clone());
                self.provider_version.clone_from(provider_version);
                self.model.clone_from(model);
                self.cwd = Some(cwd.clone());
                self.mode.clone_from(mode);
                self.capabilities = Some(capabilities.as_ref().clone());
                self.state = SessionState::Running;
            }
            Event::SessionStateChanged { state, .. } => self.state = *state,
            Event::RunStarted { run_id, .. } => self.current_run = Some(run_id.clone()),
            Event::TurnStarted { turn_id, .. } => self.current_turn = Some(turn_id.clone()),
            Event::TurnCompleted { turn_id, .. } => {
                if self.current_turn.as_ref() == Some(turn_id) {
                    self.current_turn = None;
                }
            }
            Event::RunCompleted { run_id, .. } => {
                if self.current_run.as_ref() == Some(run_id) {
                    self.current_run = None;
                    self.current_turn = None;
                    // A tool that reported no completion does not outlive
                    // its run.
                    self.running_tools.clear();
                }
            }
            Event::ToolStarted {
                node_id,
                tool_kind,
                title,
                input_summary,
                ..
            } => {
                self.running_tools.retain(|t| &t.node_id != node_id);
                if self.running_tools.len() >= MAX_RUNNING_TOOLS {
                    self.running_tools.remove(0);
                }
                self.running_tools.push(RunningTool {
                    node_id: node_id.clone(),
                    tool_kind: *tool_kind,
                    title: title.clone(),
                    input_summary: input_summary.clone(),
                });
            }
            Event::ToolCompleted { node_id, .. } => {
                self.running_tools.retain(|t| &t.node_id != node_id);
            }
            Event::ApprovalRequested {
                request_id,
                node_id,
                request_kind,
                title,
                details,
                input,
                options,
                response,
                ..
            } => {
                self.open_requests.retain(|r| &r.request_id != request_id);
                self.open_requests.push(OpenRequest {
                    request_id: request_id.clone(),
                    node_id: node_id.clone(),
                    request_kind: *request_kind,
                    title: title.clone(),
                    details: details.clone(),
                    input: input.clone(),
                    options: options.clone(),
                    response: *response,
                    requested_at: at,
                });
            }
            Event::ApprovalResolved { request_id, .. }
            | Event::ApprovalExpired { request_id, .. } => {
                self.open_requests.retain(|r| &r.request_id != request_id);
            }
            Event::ModeChanged { mode } => self.mode = Some(mode.clone()),
            Event::ModelChanged { model } => self.model = Some(model.clone()),
            Event::UsageUpdated { usage, .. } => add_usage(&mut self.usage, usage),
            Event::BackgroundUpdated { tasks } => {
                self.background_tasks = tasks.iter().map(|t| t.task_id.clone()).collect();
                let agents = tasks
                    .iter()
                    .filter(|t| SUBAGENT_TASK_KINDS.contains(&t.task_kind.as_str()))
                    .count();
                self.background_agents = u32::try_from(agents).unwrap_or(u32::MAX);
            }
            Event::Error {
                code,
                message,
                retry_at,
            } => {
                self.last_error = Some(SessionError {
                    code: code.clone(),
                    message: message.clone(),
                    retry_at: *retry_at,
                });
            }
            Event::SessionEnded { .. } => {
                if !matches!(self.state, SessionState::Lost) {
                    self.state = SessionState::Exited;
                }
                self.current_run = None;
                self.current_turn = None;
                self.background_tasks.clear();
                self.background_agents = 0;
                self.running_tools.clear();
            }
            Event::MessageDelta { .. }
            | Event::MessageCompleted { .. }
            | Event::ReasoningDelta { .. }
            | Event::ToolOutput { .. }
            | Event::FileChanged { .. }
            | Event::PlanProposed { .. }
            | Event::InterruptResult { .. }
            | Event::InputUpdated { .. } => {}
        }
    }

    fn derive_activity(&self) -> Activity {
        if matches!(self.state, SessionState::Exited | SessionState::Lost) {
            Activity::Ended
        } else if !self.open_requests.is_empty() {
            Activity::NeedsYou
        } else if self.current_run.is_some() {
            Activity::Working
        } else if !self.background_tasks.is_empty() {
            Activity::Background
        } else {
            Activity::Idle
        }
    }
}

fn add_usage(total: &mut Usage, more: &Usage) {
    fn add<T: std::ops::Add<Output = T> + Copy>(a: Option<T>, b: Option<T>) -> Option<T> {
        match (a, b) {
            (Some(a), Some(b)) => Some(a + b),
            (a, None) => a,
            (None, b) => b,
        }
    }
    total.input_tokens = add(total.input_tokens, more.input_tokens);
    total.output_tokens = add(total.output_tokens, more.output_tokens);
    total.cost_usd = add(total.cost_usd, more.cost_usd);
}

#[cfg(test)]
#[path = "fold_tests.rs"]
mod tests;
