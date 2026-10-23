//! The native Codex adapter (ADR-002): `codex app-server` over JSON-RPC.
//!
//! The sans-IO core ([`CodexCore`]) is hardened from `spikes/s2-codex`. It
//! holds no process and does no I/O: host commands and app-server frames go
//! in; canonical `fleetdeck-proto` events and frames to write go out
//! ([`Output`]). The live driver and the replay harness wrap the same core, so
//! a replay runs the real mapping code (PLAN §13, L20).
//!
//! Wire facts (codex-cli 0.160.0, ADR-002 §2.4):
//! - `turn/start` while a turn runs joins the running turn. The stable API has
//!   no follow-up queue, so the core keeps follow-ups itself and sends each one
//!   after `turn/completed` (DL-015).
//! - A prompt is confirmed when `item/started` reports a `userMessage` whose
//!   `clientId` is the input id that the host sent as `clientUserMessageId`.
//! - Server request ids are per-process integers that restart at 0.

mod checkpoint;
mod core;
mod core_types;
mod driver;
mod host_cmd;
mod limits;
mod map;
/// The local price table of Codex models (R4.6, DL-161).
pub mod prices;
mod probe;
mod requests;
mod rollback;
pub mod rpc;
mod shell;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_checkpoint;
#[cfg(test)]
mod tests_models;
#[cfg(test)]
mod tests_options;
#[cfg(test)]
mod tests_review;
#[cfg(test)]
mod tests_rollback;
#[cfg(test)]
mod tests_shell;

use fleetdeck_proto::capability::{BackgroundReporting, TerminalStatusQuality, UsageReporting};
use fleetdeck_proto::event::ResponseChannel;
use fleetdeck_proto::{Capabilities, ProviderInstanceId};
use serde_json::{Value, json};

pub use self::checkpoint::{CHECKPOINT_READS, CHECKPOINT_VERSION};
pub use self::core::{CodexCore, Output};
pub use self::rollback::ROLLBACK_TIMEOUT;
pub use driver::{
    CodexAdapter, CodexAdapterConfig, CodexSession, Origin, TOKEN_DELTAS, is_token_delta,
};
pub use host_cmd::HostCmd;
pub use map::apps::{AppOp, TOOLS_PAGES, parse_tools_page, tool_answer};
pub use map::{HANDLED_NOTIFICATIONS, IGNORED_NOTIFICATIONS};
pub use probe::{parse_account, parse_version};
pub use requests::{DECLINED_REQUESTS, HANDLED_REQUESTS, validate_question_answer};

/// The oldest `codex` version that the pinned app-server schema and the
/// recorded fixtures cover (R8.2, DL-162).
pub const TESTED_MIN_VERSION: &str = "0.160.0";
/// `codex` versions that are known to break the protocol (none yet).
pub const KNOWN_BROKEN: &[&str] = &[];

/// Codex collaboration modes (`ModeKind` of the 0.160.0 schema).
pub const COLLABORATION_MODES: &[&str] = &["plan", "default"];

/// Client requests that the core sends.
pub const CLIENT_REQUESTS: &[&str] = &[
    "initialize",
    "thread/start",
    "thread/resume",
    "thread/fork",
    "turn/start",
    "turn/steer",
    "turn/interrupt",
    "thread/backgroundTerminals/list",
    "thread/shellCommand",
];

/// Codex approval policies (`AskForApproval`) that the app-server enforces,
/// most restrictive first.
pub const APPROVAL_POLICIES: &[&str] = &["untrusted", "on-request", "never"];
/// Codex sandbox modes (`SandboxMode`), most restrictive first.
pub const SANDBOX_MODES: &[&str] = &["read-only", "workspace-write", "danger-full-access"];
/// The sandbox that only an explicit station setting offers.
pub const FULL_ACCESS_SANDBOX: &str = "danger-full-access";
/// Mode of a new session when the owner chose none.
pub const DEFAULT_MODE: &str = "untrusted/workspace-write";
/// The most restrictive mode: used for an unknown mode (L8).
pub const SAFEST_MODE: &str = "untrusted/read-only";
/// The longest model name that the core sends.
pub const MAX_MODEL_CHARS: usize = 200;

/// A native mode: approval policy × sandbox, written `"<policy>/<sandbox>"`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Mode {
    pub approval_policy: String,
    pub sandbox: String,
}

impl Mode {
    /// Parses `"<policy>/<sandbox>"`; `None` if either part is not native.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        let (a, b) = s.split_once('/')?;
        (APPROVAL_POLICIES.contains(&a) && SANDBOX_MODES.contains(&b)).then(|| Self {
            approval_policy: a.to_owned(),
            sandbox: b.to_owned(),
        })
    }

    /// The most restrictive mode.
    #[must_use]
    pub fn safest() -> Self {
        Self {
            approval_policy: "untrusted".to_owned(),
            sandbox: "read-only".to_owned(),
        }
    }

    /// Parses a chosen mode: none → [`DEFAULT_MODE`], unknown → [`SAFEST_MODE`].
    #[must_use]
    pub fn or_safest(s: Option<&str>) -> Self {
        Self::parse(s.unwrap_or(DEFAULT_MODE)).unwrap_or_else(Self::safest)
    }

    #[must_use]
    pub fn name(&self) -> String {
        format!("{}/{}", self.approval_policy, self.sandbox)
    }

    /// `SandboxPolicy` value for `turn/start` (the per-turn shape differs from
    /// the `thread/start` `sandbox` enum).
    #[must_use]
    pub fn sandbox_policy(&self) -> Value {
        match self.sandbox.as_str() {
            "danger-full-access" => json!({"type": "dangerFullAccess"}),
            "workspace-write" => json!({"type": "workspaceWrite"}),
            _ => json!({"type": "readOnly"}),
        }
    }
}

/// Every native mode name, most restrictive first. The full-access sandbox
/// is offered only when the station allows it (DL-063).
#[must_use]
pub fn native_modes(allow_full_access: bool) -> Vec<String> {
    SANDBOX_MODES
        .iter()
        .filter(|s| allow_full_access || **s != FULL_ACCESS_SANDBOX)
        .flat_map(|s| APPROVAL_POLICIES.iter().map(move |p| format!("{p}/{s}")))
        .collect()
}

/// The capabilities of a Codex session (PLAN §6.2, Codex column).
#[must_use]
pub fn capabilities(allow_full_access: bool) -> Capabilities {
    Capabilities {
        resume: true,
        fork: true,
        requests_survive_restart: false,
        interrupt: true,
        steer: true,
        // The stable API has no follow-up queue; the core queues host-side.
        native_follow_up: false,
        terminal_status: TerminalStatusQuality::Strong,
        live_deltas: true,
        file_changes: true,
        approvals: true,
        response_channels: vec![ResponseChannel::RpcReply, ResponseChannel::Message],
        edit_input: false,
        modes: native_modes(allow_full_access),
        set_mode: true,
        set_model: true,
        plan_capture: true,
        subagents: true,
        background_work: BackgroundReporting::Polled,
        native_turn_ids: true,
        usage: UsageReporting {
            tokens: true,
            cost: false,
        },
        // `thread/rollback` drops whole turns of the thread (DL-087).
        rollback: true,
    }
}

/// Settings of a driven thread.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SessionSpec {
    pub model: Option<String>,
    pub effort: Option<String>,
    /// `"<policy>/<sandbox>"`; none → [`DEFAULT_MODE`], unknown → [`SAFEST_MODE`].
    pub mode: Option<String>,
    /// Working directory of the thread.
    pub cwd: Option<String>,
    /// Native thread id to resume instead of starting a new thread.
    pub resume: Option<String>,
    /// With `resume`: fork that thread into a new thread (`thread/fork`).
    pub fork: bool,
}

/// Arguments for the `codex` binary.
#[must_use]
pub fn app_server_args() -> Vec<String> {
    vec!["app-server".to_owned()]
}

/// The first line of a text, at most 120 characters.
pub(crate) fn summary_of(text: &str) -> String {
    let line = text.lines().next().unwrap_or_default();
    truncate(line, 120)
}

/// At most `max` characters, with `…` when cut.
pub(crate) fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let mut cut: String = text.chars().take(max.saturating_sub(1)).collect();
    cut.push('…');
    cut
}

/// `RequestId` of an app request id.
pub(crate) fn request_id(app_id: &str) -> fleetdeck_proto::RequestId {
    fleetdeck_proto::RequestId::new(app_id)
}

/// The `clientInfo` that `initialize` sends. The app-server echoes the name
/// in its `userAgent`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientInfo {
    pub name: String,
    pub version: String,
}

impl ClientInfo {
    /// The client info of a production session.
    #[must_use]
    pub fn fleetdeck() -> Self {
        Self {
            name: "fleetdeck".to_owned(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
        }
    }
}

/// What the core needs to know about its session.
#[derive(Debug, Clone, PartialEq)]
pub struct CoreConfig {
    pub provider_instance: ProviderInstanceId,
    /// The canonical workspace path (trap 8).
    pub cwd: String,
    pub capabilities: Capabilities,
    /// `Some(true)` for a resumed thread, `Some(false)` for the "not resumed"
    /// degradation, `None` for a new thread.
    pub resumed: Option<bool>,
    pub client: ClientInfo,
    /// Send `model/list` after the thread opens (DL-151). Replays of
    /// recordings that hold no `model/list` set it to false.
    pub request_models: bool,
}
