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

use crate::event::{Event, EventEnvelope, RateLimit, SessionState, Usage};
use crate::session::{
    Activity, MAX_RUNNING_TOOLS, OpenRequest, RunningTool, SUBAGENT_TASK_KINDS, SessionBase,
    SessionError, SessionRevert, SessionSummary, TerminalHold,
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
                repo,
                ..
            } => {
                if repo.is_some() {
                    self.repo = repo.as_deref().cloned();
                }
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
                self.hibernated = false;
            }
            Event::SessionStateChanged { state, .. } => {
                self.state = *state;
                // A lost or exited process runs nothing more: the same
                // cleanup as `session.ended` (M2 gate correction).
                if matches!(state, SessionState::Lost | SessionState::Exited) {
                    self.clear_running();
                }
            }
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
            Event::UsageUpdated { usage, rate_limits } => {
                add_usage(&mut self.usage, usage);
                merge_rate_limits(&mut self.rate_limits, rate_limits);
            }
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
            Event::RevertUpdated {
                revert_id,
                turn_id,
                state,
                reason,
                ..
            } => {
                self.revert = Some(SessionRevert {
                    revert_id: revert_id.clone(),
                    turn_id: turn_id.clone(),
                    state: *state,
                    reason: reason.clone(),
                });
            }
            Event::SessionRenamed { title } => {
                self.title.clone_from(title);
                self.title_generated = false;
            }
            // The marker counts only while the title is the one that the
            // model help set; an owner rename in between wins (DL-167).
            Event::SessionTitleGenerated { title } => {
                self.title_generated = self.title.as_deref() == Some(title.as_str());
            }
            Event::SessionDigest { text } => self.digest = Some(text.clone()),
            Event::SessionNamed { callsign, worktree } => {
                self.callsign = Some(callsign.clone());
                self.worktree.clone_from(worktree);
            }
            Event::SessionIssue { issue } => self.issue = Some(issue.clone()),
            Event::AutoContinueChanged { enabled } => self.auto_continue = *enabled,
            Event::SessionTerminal {
                shell_id: Some(shell_id),
                pid,
                reason,
            } => {
                let same = self.terminal.as_ref().filter(|h| &h.shell_id == shell_id);
                let since = same.map_or(at, |h| h.since);
                let pid = pid.or_else(|| same.and_then(|h| h.pid));
                self.terminal = Some(TerminalHold {
                    shell_id: shell_id.clone(),
                    pid,
                    since,
                    reason: reason.clone(),
                });
            }
            Event::SessionTerminal { shell_id: None, .. } => self.terminal = None,
            Event::AutoContinueSkipped { .. }
            | Event::CoordinatorBound { .. }
            | Event::CoordinatorEnded { .. }
            | Event::SlashCommands { .. }
            | Event::SessionNotice { .. } => {}
            Event::SessionModels { models } => self.models = Some(models.clone()),
            Event::SessionBase {
                git_ref,
                oid,
                inferred,
            } => {
                self.base = Some(SessionBase {
                    git_ref: git_ref.clone(),
                    oid: oid.clone(),
                    inferred: *inferred,
                });
            }
            // A new artifact counts once; a later version does not (R12).
            Event::ArtifactPublished { version, .. } => {
                if *version <= 1 {
                    self.artifacts_count += 1;
                }
            }
            Event::ArtifactSuggested { .. } => {}
            Event::SessionFocus { text } => {
                self.focus = Some(text.clone()).filter(|t| !t.is_empty());
            }
            Event::SessionBatch { batch } => self.batch = Some(batch.clone()),
            Event::SessionHibernated { .. } => self.hibernated = true,
            Event::SessionEnded { .. } => {
                if !matches!(self.state, SessionState::Lost) {
                    self.state = SessionState::Exited;
                }
                self.clear_running();
            }
            Event::MessageDelta { .. }
            | Event::MessageCompleted { .. }
            | Event::ReasoningDelta { .. }
            | Event::ToolOutput { .. }
            | Event::FileChanged { .. }
            | Event::PlanProposed { .. }
            | Event::InterruptResult { .. }
            | Event::InputUpdated { .. }
            | Event::OutputGap { .. }
            // A late checkpoint never changes the activity (L13).
            | Event::TurnFinalized { .. } => {}
        }
    }

    /// Ends every run, turn, tool and background task of the summary. The
    /// timeline history, the open requests and `last_error` stay.
    fn clear_running(&mut self) {
        self.current_run = None;
        self.current_turn = None;
        self.background_tasks.clear();
        self.background_agents = 0;
        self.running_tools.clear();
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
    // A total with one inferred part is inferred (R4.6).
    total.cost_inferred |= more.cost_inferred && more.cost_usd.is_some();
}

/// The most rate-limit windows that a summary keeps.
pub const MAX_RATE_LIMITS: usize = 16;

/// A reported window replaces the window of the same scope; a scope that
/// is not reported keeps its last window (sparse updates).
fn merge_rate_limits(kept: &mut Vec<RateLimit>, more: &[RateLimit]) {
    for limit in more {
        if let Some(k) = kept.iter_mut().find(|k| k.scope == limit.scope) {
            *k = limit.clone();
        } else if kept.len() < MAX_RATE_LIMITS {
            kept.push(limit.clone());
        }
    }
    kept.sort_by(|a, b| a.scope.cmp(&b.scope));
}

#[cfg(test)]
#[path = "fold_tests.rs"]
mod tests;
