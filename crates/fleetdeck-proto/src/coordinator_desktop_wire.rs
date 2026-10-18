//! The wire shapes of the P6 coordinator desktop methods (feature
//! `desktop_mcp.v1`; `docs/v2/design/omarchy-p6.md` §5): the two envelopes,
//! the reply, the coordinator context of an action authority, the protected
//! reads, the dispatch authority check and the live target policy.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;

use crate::coordinator_desktop::{DesktopCall, DesktopQuery};
use crate::ids::{CommandId, IncarnationId, SessionId, StationId};
use crate::mutation::CoordinatorBinding;

/// Who a coordinator desktop action runs for (§5.4). The deck builds it from
/// the checked grant and binding; the action hash covers it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "coordinator_desktop/")]
pub struct CoordinatorContext {
    pub grant_id: String,
    pub binding_id: String,
    pub generation: u32,
    pub caller_station: StationId,
    pub caller_session: SessionId,
    /// The generated callsign of the caller when it is 1 to 32 characters
    /// from `[a-z0-9-]`, else the caller session ID. The target refuses more
    /// than 64 characters or a character outside `[A-Za-z0-9_-]`.
    pub caller_label: String,
}

/// `coordinator.desktop_call` params (§5.3): a mutation with a durable
/// record on the deck. No transport retries it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "coordinator_desktop/")]
pub struct CoordinatorDesktopCallEnvelope {
    pub command_id: CommandId,
    /// The deck's database incarnation.
    pub target_incarnation: IncarnationId,
    pub retry_until: i64,
    /// The session of the coordinator agent on the sending station.
    pub caller: SessionId,
    pub binding: CoordinatorBinding,
    pub call: DesktopCall,
}

/// `coordinator.desktop_query` params (§5.3). A query keeps no receipt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "coordinator_desktop/")]
pub struct CoordinatorDesktopQueryEnvelope {
    pub query_id: String,
    /// The deck's database incarnation.
    pub target_incarnation: IncarnationId,
    pub deadline: i64,
    pub caller: SessionId,
    pub binding: CoordinatorBinding,
    pub query: DesktopQuery,
}

/// The reply of `coordinator.desktop_call` and `coordinator.desktop_query`
/// (a dedicated shape, not a `Receipt`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
#[ts(export_to = "coordinator_desktop/")]
pub enum DesktopReply {
    Ok {
        #[ts(type = "unknown")]
        result: Value,
    },
    Err {
        error: String,
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        operation_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        capture_id: Option<String>,
    },
}

/// The phase of a `desktop.authority_check` (§5.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "coordinator_desktop/")]
pub enum AuthorityPhase {
    Execute,
    Import,
    Read,
}

/// The closed read of `desktop.coordinator_read` (§5.7).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
#[ts(export_to = "coordinator_desktop/")]
pub enum ProtectedRead {
    Info,
    ActionStatus {
        operation_id: String,
    },
    CaptureStatus {
        capture_id: String,
    },
    /// One read is one chunk: the target refuses an `offset` other than 0 and
    /// a `length` that is not the committed byte count.
    CaptureFile {
        artifact_id: String,
        version: u32,
        offset: u64,
        length: u64,
    },
}

/// `desktop.coordinator_read` params (§5.7). The deck computes `read_hash`;
/// the target passes it on unchanged.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "coordinator_desktop/")]
pub struct CoordinatorRead {
    pub read_id: String,
    pub read: ProtectedRead,
    pub read_hash: String,
}

/// `desktop.authority_check` params (§5.6). For the phase `read`,
/// `operation_id` is the `read_id`, `action_hash` is the `read_hash` and
/// `read` is the object that the target is about to run; for the other
/// phases `read` is absent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "coordinator_desktop/")]
pub struct DesktopAuthorityCheck {
    pub operation_id: String,
    pub action_hash: String,
    pub phase: AuthorityPhase,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub read: Option<ProtectedRead>,
}

/// Why an authority check is not valid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "coordinator_desktop/")]
pub enum AuthorityReason {
    AuthorityLost,
    AuthorityUnknown,
}

/// The answer to [`DesktopAuthorityCheck`]. `reason` is always written
/// (`null` when valid).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "coordinator_desktop/")]
pub struct DesktopAuthorityAnswer {
    pub valid: bool,
    pub reason: Option<AuthorityReason>,
}

/// Why the deck asks for the live policy of a target (§5.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "coordinator_desktop/")]
pub enum PolicyPurpose {
    Desktop,
    Capture,
}

/// `desktop.policy_now` params (§5.5). `monitor` is always written.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "coordinator_desktop/")]
pub struct DesktopPolicyNow {
    pub purpose: PolicyPurpose,
    pub monitor: Option<String>,
}

/// The answer to [`DesktopPolicyNow`]: the live policy of the target.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "coordinator_desktop/")]
pub struct DesktopPolicyAnswer {
    pub allow_fleet_control: bool,
    pub allow_desktop: bool,
    pub allow_desktop_capture: bool,
    pub monitor_allowed: bool,
}

#[cfg(test)]
#[path = "coordinator_desktop_wire_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "coordinator_desktop_compat_tests.rs"]
mod compat_tests;
