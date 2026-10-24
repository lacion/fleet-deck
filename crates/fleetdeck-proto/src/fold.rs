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

use std::collections::HashSet;

use crate::event::{
    BackgroundTask, Event, EventEnvelope, RateLimit, SessionState, ToolKind, Usage, UsageRecount,
    WorkflowUpdate,
};
use crate::fleet_git::{ArtifactsDir, CleanupInfo, MovedTo, PushInfo};
use crate::session::{
    Activity, LegacyUsage, MAX_RUNNING_TOOLS, MAX_RUNNING_WORKFLOWS, OpenRequest, RunningTool,
    RunningWorkflow, SUBAGENT_TASK_KINDS, SessionBase, SessionError, SessionRevert,
    SessionSummary, TerminalHold, WORKFLOW_TASK_KINDS,
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
        self.derive_agents();
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
                // A start or resume that worked ends the error of an earlier
                // attempt; the timeline keeps the error event as history.
                self.last_error = None;
            }
            Event::SessionStateChanged { state, .. } => {
                self.state = *state;
                // A lost or exited process runs nothing more: the same
                // cleanup as `session.ended` (M2 gate correction).
                if matches!(state, SessionState::Lost | SessionState::Exited) {
                    self.clear_running();
                }
            }
            Event::RunStarted { run_id, .. } => {
                self.current_run = Some(run_id.clone());
                // A new run means the session takes input again.
                self.last_error = None;
            }
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
                self.fold_usage(usage, at);
                merge_rate_limits(&mut self.rate_limits, rate_limits);
            }
            Event::UsageSet {
                usage,
                previous,
                source,
                command_id,
            } => {
                self.usage = usage.clone();
                self.usage_legacy = None;
                self.usage_recount = Some(UsageRecount {
                    at,
                    source: *source,
                    command_id: command_id.clone(),
                    previous: Usage::clone(previous),
                });
            }
            Event::BackgroundUpdated { tasks } => {
                self.background_tasks = tasks.iter().map(|t| t.task_id.clone()).collect();
                let agents = tasks
                    .iter()
                    .filter(|t| SUBAGENT_TASK_KINDS.contains(&t.task_kind.as_str()))
                    .count();
                self.background_agents = u32::try_from(agents).unwrap_or(u32::MAX);
                self.fold_workflow_list(tasks);
            }
            Event::WorkflowUpdated(update) => self.fold_workflow(update),
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
            | Event::CoordinatorWorkerOutcome { .. }
            | Event::SessionNotice { .. } => {}
            Event::SlashCommands { commands, skills } => {
                self.slash_commands = Some(commands.clone());
                self.skills = Some(skills.clone());
            }
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
            Event::WorkspaceBaseStale { .. } => self.base_stale = true,
            Event::SessionPush {
                state,
                ref_name,
                oid,
                desired_oid,
                stop_code,
                attempts,
                next_attempt_at,
                code,
                text,
            } => {
                self.push = Some(PushInfo {
                    state: *state,
                    ref_name: ref_name.clone(),
                    oid: oid.clone(),
                    desired_oid: desired_oid.clone(),
                    stop_code: stop_code.clone(),
                    attempts: *attempts,
                    next_attempt_at: *next_attempt_at,
                    code: code.clone(),
                    text: text.clone(),
                });
            }
            Event::SessionCleanup { state, reason } => {
                self.cleanup = Some(CleanupInfo {
                    state: *state,
                    reason: reason.clone(),
                });
            }
            Event::SessionLand(info) => self.land = Some(info.as_ref().clone()),
            Event::SessionMoveIn(info) => self.move_in = Some(info.clone()),
            Event::SessionMoved {
                move_id,
                to_station,
                to_session,
            } => {
                self.moved_to = Some(MovedTo {
                    move_id: move_id.clone(),
                    station_id: to_station.clone(),
                    session_id: to_session.clone(),
                });
            }
            Event::SessionMoveUndone { move_id } => {
                if self.moved_to.as_ref().is_some_and(|m| &m.move_id == move_id) {
                    self.moved_to = None;
                }
            }
            Event::ArtifactsReady { path, url } => {
                self.artifacts_dir = Some(ArtifactsDir {
                    path: path.clone(),
                    url: url.clone(),
                });
            }
            Event::ArtifactsRevoked { .. } => self.artifacts_dir = None,
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
            // The board reads the task events; the fold keeps no state (B6).
            | Event::TaskUpdated { .. }
            // The reconciliation changes no summary field; the board reads the
            // execution check instead (DL-182).
            | Event::ExecutionReconciled { .. }
            | Event::ExecutionSettled { .. }
            // The board reads the skipped files from the event (FG-4).
            | Event::WorkspaceIncludeSkipped { .. }
            // The board reads the reason from the event (FG-12).
            | Event::ArtifactsUnavailable { .. }
            // A late checkpoint never changes the activity (L13).
            | Event::TurnFinalized { .. } => {}
        }
    }

    /// Ends every run, turn, tool and background task of the summary. The
    /// timeline history, the open requests and `last_error` stay (a later
    /// `session.started` or `run.started` clears `last_error`).
    fn clear_running(&mut self) {
        self.current_run = None;
        self.current_turn = None;
        self.background_tasks.clear();
        self.background_agents = 0;
        self.background_workflows = 0;
        self.running_workflows.clear();
        self.workflows_overflow = false;
        self.running_tools.clear();
    }

    /// `workflow.updated` (B12, design §4.3): a `running` status upserts the
    /// entry of the task; any other status removes it and leaves
    /// `workflows_overflow` alone (an omitted workflow can still run).
    fn fold_workflow(&mut self, update: &WorkflowUpdate) {
        if update.status != "running" {
            self.running_workflows
                .retain(|w| w.task_id != update.task_id);
            return;
        }
        let entry = RunningWorkflow {
            task_id: update.task_id.clone(),
            running_agents: update.running_agents,
            agents_total: update.agents_total,
            untracked: update.untracked,
        };
        if let Some(kept) = self
            .running_workflows
            .iter_mut()
            .find(|w| w.task_id == update.task_id)
        {
            *kept = entry;
        } else if self.running_workflows.len() >= MAX_RUNNING_WORKFLOWS {
            self.workflows_overflow = true;
        } else {
            self.running_workflows.push(entry);
        }
    }

    /// `background.updated` is the complete task set: count its workflows,
    /// remove every running workflow that it does not list, and clear
    /// `workflows_overflow` only when every listed workflow has an entry.
    fn fold_workflow_list(&mut self, tasks: &[BackgroundTask]) {
        let listed: HashSet<&str> = tasks.iter().map(|t| t.task_id.as_str()).collect();
        self.running_workflows
            .retain(|w| listed.contains(w.task_id.as_str()));
        let mut count = 0_usize;
        let mut all_known = true;
        for task in tasks
            .iter()
            .filter(|t| WORKFLOW_TASK_KINDS.contains(&t.task_kind.as_str()))
        {
            count += 1;
            all_known &= self
                .running_workflows
                .iter()
                .any(|w| w.task_id == task.task_id);
        }
        self.background_workflows = u32::try_from(count).unwrap_or(u32::MAX);
        if all_known {
            self.workflows_overflow = false;
        }
    }

    /// `running_agents` and `agents_partial` (B19), derived after every event.
    fn derive_agents(&mut self) {
        let tools = self
            .running_tools
            .iter()
            .filter(|t| t.tool_kind == ToolKind::Subagent)
            .count();
        let tools = u32::try_from(tools).unwrap_or(u32::MAX);
        let workflows = self
            .running_workflows
            .iter()
            .fold(0_u32, |sum, w| sum.saturating_add(w.running_agents));
        self.running_agents = tools
            .saturating_add(self.background_agents)
            .saturating_add(workflows);
        self.agents_partial =
            self.workflows_overflow || self.running_workflows.iter().any(|w| w.untracked);
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

impl SessionSummary {
    /// Folds one `usage.updated` part (DL-449). A part of the new scheme is
    /// never added to an old total: the first one sets the old total apart
    /// in `usage_legacy`, and a later old-rule part goes there too.
    fn fold_usage(&mut self, more: &Usage, at: i64) {
        // A rate-limit-only update carries no usage.
        if more.is_empty() {
            return;
        }
        match (self.usage.scheme, more.scheme) {
            (None, Some(_)) => {
                if self.usage.is_empty() {
                    self.usage = Usage {
                        scheme: more.scheme,
                        ..Usage::default()
                    };
                } else {
                    let old = std::mem::take(&mut self.usage);
                    match self.usage_legacy.as_mut() {
                        Some(legacy) => {
                            add_usage(&mut legacy.usage, &old);
                            legacy.until = at;
                        }
                        None => {
                            self.usage_legacy = Some(LegacyUsage {
                                usage: old,
                                until: at,
                            });
                        }
                    }
                    self.usage = Usage {
                        scheme: more.scheme,
                        ..Usage::default()
                    };
                }
                add_usage(&mut self.usage, more);
            }
            (Some(_), None) => {
                let legacy = self.usage_legacy.get_or_insert_with(|| LegacyUsage {
                    usage: Usage::default(),
                    until: at,
                });
                add_usage(&mut legacy.usage, more);
            }
            // Equal schemes, or two different new schemes (a later scheme
            // defines its own rule).
            _ => add_usage(&mut self.usage, more),
        }
    }
}

/// Adds one usage part to a total. A part that is `None` stays unknown.
pub fn add_usage(total: &mut Usage, more: &Usage) {
    fn add<T: std::ops::Add<Output = T> + Copy>(a: Option<T>, b: Option<T>) -> Option<T> {
        match (a, b) {
            (Some(a), Some(b)) => Some(a + b),
            (a, None) => a,
            (None, b) => b,
        }
    }
    // Earlier tokens without a cost are not in a cost that comes now.
    let earlier_unpriced = total.has_tokens() && total.cost_usd.is_none();
    total.input_tokens = add(total.input_tokens, more.input_tokens);
    total.output_tokens = add(total.output_tokens, more.output_tokens);
    total.cache_read_tokens = add(total.cache_read_tokens, more.cache_read_tokens);
    total.cache_write_tokens = add(total.cache_write_tokens, more.cache_write_tokens);
    total.cost_usd = add(total.cost_usd, more.cost_usd);
    // A total with one inferred part is inferred (R4.6).
    total.cost_inferred |= more.cost_inferred && more.cost_usd.is_some();
    // The latest price table wins.
    if more.prices.is_some() {
        total.prices.clone_from(&more.prices);
    }
    total.tokens_partial |= more.tokens_partial;
    total.cost_partial |= more.cost_partial;
    // `None + Some(x)` must not show a complete-looking cost.
    if (more.cost_usd.is_none() && more.has_tokens())
        || (earlier_unpriced && more.cost_usd.is_some())
    {
        total.cost_partial = true;
    }
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

#[cfg(test)]
#[path = "fold_tests_workflow.rs"]
mod tests_workflow;

#[cfg(test)]
#[path = "fold_usage_tests.rs"]
mod usage_tests;
