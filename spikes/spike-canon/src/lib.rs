//! Spike-only subset of the canonical event vocabulary of PLAN §6.3.
//!
//! The S1 (Claude) and S2 (Codex) adapter spikes map provider frames to these
//! events, so the replay tests can compare both providers with one shape. The
//! product version lives in `fleetdeck-proto` (Phase 1); this crate never ships.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Native ids of a provider, scoped by provider + session (L6). Never a global key.
pub type ProviderRefs = serde_json::Map<String, Value>;

/// Kind of a tool call (PLAN §6.3 `tool.started.kind`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolKind {
    Shell,
    Edit,
    Read,
    Search,
    Web,
    Mcp,
    Subagent,
    Other,
}

/// Kind of a runtime request (approval or question).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestKind {
    Command,
    FileChange,
    Tool,
    Question,
}

/// How the provider expects the answer to a runtime request (L4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResponseChannel {
    /// The answer is a reply to the provider's own request id.
    RpcReply,
    /// The answer is a new user message.
    Message,
}

/// One answer option with the provider's own option id (L3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalOption {
    pub option_id: String,
    pub label: String,
}

/// Terminal status of a turn or a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnStatus {
    Done,
    Interrupted,
    Failed,
}

/// Terminal status of a tool call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolStatus {
    Ok,
    Failed,
    Denied,
}

/// Change kind of a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileChange {
    Added,
    Modified,
    Deleted,
}

/// Token usage. `None` means unknown, which is not zero.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cost_usd: Option<f64>,
}

/// Canonical, provider-blind event (spike subset of PLAN §6.3).
///
/// App ids (`turn_id`, `node_id`, `request_id`) are allocated by the adapter by
/// ordinal (L6); native ids go into `provider_refs`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CanonEvent {
    #[serde(rename = "session.started")]
    SessionStarted {
        provider: String,
        provider_version: Option<String>,
        model: Option<String>,
        mode: Option<String>,
        provider_refs: ProviderRefs,
    },
    #[serde(rename = "turn.started")]
    TurnStarted {
        turn_id: String,
        provider_refs: ProviderRefs,
    },
    #[serde(rename = "message.delta")]
    MessageDelta { turn_id: String, text: String },
    #[serde(rename = "message.completed")]
    MessageCompleted { turn_id: String, text: String },
    #[serde(rename = "reasoning.delta")]
    ReasoningDelta { turn_id: String, text: String },
    #[serde(rename = "tool.started")]
    ToolStarted {
        node_id: String,
        parent_node_id: Option<String>,
        tool_kind: ToolKind,
        title: String,
        provider_refs: ProviderRefs,
    },
    #[serde(rename = "tool.completed")]
    ToolCompleted { node_id: String, status: ToolStatus },
    #[serde(rename = "file.changed")]
    FileChanged { path: String, change: FileChange },
    #[serde(rename = "approval.requested")]
    ApprovalRequested {
        request_id: String,
        node_id: Option<String>,
        request_kind: RequestKind,
        title: String,
        options: Vec<ApprovalOption>,
        response: ResponseChannel,
        provider_refs: ProviderRefs,
    },
    #[serde(rename = "approval.resolved")]
    ApprovalResolved {
        request_id: String,
        option_id: String,
    },
    #[serde(rename = "approval.expired")]
    ApprovalExpired { request_id: String, reason: String },
    #[serde(rename = "plan.proposed")]
    PlanProposed { markdown: String },
    #[serde(rename = "usage.updated")]
    UsageUpdated { usage: Usage },
    #[serde(rename = "turn.completed")]
    TurnCompleted {
        turn_id: String,
        status: TurnStatus,
        usage: Option<Usage>,
    },
    #[serde(rename = "mode.changed")]
    ModeChanged { mode: String },
    #[serde(rename = "error")]
    Error { code: String, message: String },
    #[serde(rename = "session.ended")]
    SessionEnded {
        reason: String,
        exit_code: Option<i32>,
    },
}

impl CanonEvent {
    /// The dotted kind name, for compact snapshots.
    #[must_use]
    pub fn kind_name(&self) -> &'static str {
        match self {
            Self::SessionStarted { .. } => "session.started",
            Self::TurnStarted { .. } => "turn.started",
            Self::MessageDelta { .. } => "message.delta",
            Self::MessageCompleted { .. } => "message.completed",
            Self::ReasoningDelta { .. } => "reasoning.delta",
            Self::ToolStarted { .. } => "tool.started",
            Self::ToolCompleted { .. } => "tool.completed",
            Self::FileChanged { .. } => "file.changed",
            Self::ApprovalRequested { .. } => "approval.requested",
            Self::ApprovalResolved { .. } => "approval.resolved",
            Self::ApprovalExpired { .. } => "approval.expired",
            Self::PlanProposed { .. } => "plan.proposed",
            Self::UsageUpdated { .. } => "usage.updated",
            Self::TurnCompleted { .. } => "turn.completed",
            Self::ModeChanged { .. } => "mode.changed",
            Self::Error { .. } => "error",
            Self::SessionEnded { .. } => "session.ended",
        }
    }
}

/// Allocates app ids by ordinal (L6), so replays are deterministic.
#[derive(Debug, Default, Clone)]
pub struct OrdinalIds {
    next: u64,
}

impl OrdinalIds {
    /// Returns the next id with a prefix, for example `turn-1`.
    pub fn next(&mut self, prefix: &str) -> String {
        self.next += 1;
        format!("{prefix}-{}", self.next)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_serializes_with_dotted_kind() {
        let ev = CanonEvent::TurnCompleted {
            turn_id: "turn-1".into(),
            status: TurnStatus::Interrupted,
            usage: None,
        };
        let v = serde_json::to_value(&ev).unwrap();
        assert_eq!(v["kind"], "turn.completed");
        assert_eq!(v["status"], "interrupted");
        assert_eq!(ev.kind_name(), "turn.completed");
        let back: CanonEvent = serde_json::from_value(v).unwrap();
        assert_eq!(back, ev);
    }

    #[test]
    fn ordinal_ids_are_deterministic() {
        let mut ids = OrdinalIds::default();
        assert_eq!(ids.next("turn"), "turn-1");
        assert_eq!(ids.next("node"), "node-2");
    }
}
