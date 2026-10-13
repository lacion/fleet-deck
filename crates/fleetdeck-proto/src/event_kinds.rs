//! The kind names of the events (split from `event.rs` for the module size
//! rule).

use super::Event;

impl Event {
    /// Every known kind name, in declaration order.
    pub const KINDS: [&'static str; 47] = [
        "session.started",
        "session.state",
        "run.started",
        "turn.started",
        "message.delta",
        "message.completed",
        "reasoning.delta",
        "tool.started",
        "tool.output",
        "tool.completed",
        "file.changed",
        "approval.requested",
        "approval.resolved",
        "approval.expired",
        "plan.proposed",
        "usage.updated",
        "turn.completed",
        "turn.finalized",
        "run.completed",
        "mode.changed",
        "model.changed",
        "interrupt.result",
        "input.updated",
        "background.updated",
        "error",
        "session.ended",
        "revert.updated",
        "session.renamed",
        "session.named",
        "session.issue",
        "session.output_gap",
        "session.auto_continue_changed",
        "session.auto_continue_skipped",
        "coordinator.bound",
        "coordinator.ended",
        "session.terminal",
        "session.slash_commands",
        "session.notice",
        "session.models",
        "session.base",
        "artifact.published",
        "artifact.suggested",
        "session.focus",
        "session.batch",
        "session.hibernated",
        "session.title_generated",
        "session.digest",
    ];

    /// The dotted kind name.
    #[must_use]
    pub fn kind_name(&self) -> &'static str {
        match self {
            Self::SessionStarted { .. } => "session.started",
            Self::SessionStateChanged { .. } => "session.state",
            Self::RunStarted { .. } => "run.started",
            Self::TurnStarted { .. } => "turn.started",
            Self::MessageDelta { .. } => "message.delta",
            Self::MessageCompleted { .. } => "message.completed",
            Self::ReasoningDelta { .. } => "reasoning.delta",
            Self::ToolStarted { .. } => "tool.started",
            Self::ToolOutput { .. } => "tool.output",
            Self::ToolCompleted { .. } => "tool.completed",
            Self::FileChanged { .. } => "file.changed",
            Self::ApprovalRequested { .. } => "approval.requested",
            Self::ApprovalResolved { .. } => "approval.resolved",
            Self::ApprovalExpired { .. } => "approval.expired",
            Self::PlanProposed { .. } => "plan.proposed",
            Self::UsageUpdated { .. } => "usage.updated",
            Self::TurnCompleted { .. } => "turn.completed",
            Self::TurnFinalized { .. } => "turn.finalized",
            Self::RunCompleted { .. } => "run.completed",
            Self::ModeChanged { .. } => "mode.changed",
            Self::ModelChanged { .. } => "model.changed",
            Self::InterruptResult { .. } => "interrupt.result",
            Self::InputUpdated { .. } => "input.updated",
            Self::BackgroundUpdated { .. } => "background.updated",
            Self::Error { .. } => "error",
            Self::SessionEnded { .. } => "session.ended",
            Self::RevertUpdated { .. } => "revert.updated",
            Self::SessionRenamed { .. } => "session.renamed",
            Self::SessionNamed { .. } => "session.named",
            Self::SessionIssue { .. } => "session.issue",
            Self::AutoContinueChanged { .. } => "session.auto_continue_changed",
            Self::AutoContinueSkipped { .. } => "session.auto_continue_skipped",
            Self::CoordinatorBound { .. } => "coordinator.bound",
            Self::CoordinatorEnded { .. } => "coordinator.ended",
            Self::OutputGap { .. } => "session.output_gap",
            Self::SessionTerminal { .. } => "session.terminal",
            Self::SlashCommands { .. } => "session.slash_commands",
            Self::SessionNotice { .. } => "session.notice",
            Self::SessionModels { .. } => "session.models",
            Self::SessionBase { .. } => "session.base",
            Self::ArtifactPublished { .. } => "artifact.published",
            Self::ArtifactSuggested { .. } => "artifact.suggested",
            Self::SessionFocus { .. } => "session.focus",
            Self::SessionBatch { .. } => "session.batch",
            Self::SessionHibernated { .. } => "session.hibernated",
            Self::SessionTitleGenerated { .. } => "session.title_generated",
            Self::SessionDigest { .. } => "session.digest",
        }
    }

    /// True if the event ends the session's process life.
    #[must_use]
    pub fn is_terminal_for_session(&self) -> bool {
        matches!(self, Self::SessionEnded { .. })
    }
}
