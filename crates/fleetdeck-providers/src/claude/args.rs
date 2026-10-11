//! Static facts of the Claude provider: native modes, capabilities, the CLI
//! argument list (ADR-001 §2.1), input uuids, and the probe parsers (trap 9).

use fleetdeck_proto::capability::{BackgroundReporting, TerminalStatusQuality, UsageReporting};
use fleetdeck_proto::event::ResponseChannel;
use fleetdeck_proto::{Capabilities, InputId};
use serde_json::Value;

use crate::adapter::{AuthState, NativeSessionId, ProviderError};

/// Native Claude permission modes as `claude` 2.1.x names them on the wire,
/// most restrictive first (L8). `--permission-mode manual` is a CLI alias of
/// `default` (DL-003). `bypassPermissions` is last and is offered only when
/// the station policy allows it.
pub const NATIVE_MODES: &[&str] = &[
    "default",
    "plan",
    "acceptEdits",
    "dontAsk",
    "auto",
    "bypassPermissions",
];

/// The mode that skips every permission check.
pub const BYPASS_MODE: &str = "bypassPermissions";

/// The most restrictive native mode ("Supervised"): the fallback for an
/// unknown stored mode (L8).
pub const SAFEST_MODE: &str = "default";

/// The capabilities of a Claude session (PLAN §6.2 matrix, Claude column).
#[must_use]
pub fn capabilities(allow_bypass: bool) -> Capabilities {
    Capabilities {
        resume: true,
        fork: true,
        requests_survive_restart: false,
        interrupt: true,
        steer: true,
        native_follow_up: true,
        terminal_status: TerminalStatusQuality::Strong,
        live_deltas: true,
        file_changes: true,
        approvals: true,
        response_channels: vec![ResponseChannel::RpcReply],
        edit_input: true,
        modes: NATIVE_MODES
            .iter()
            .filter(|m| allow_bypass || **m != BYPASS_MODE)
            .map(|m| (*m).to_owned())
            .collect(),
        set_mode: true,
        set_model: true,
        plan_capture: true,
        subagents: true,
        background_work: BackgroundReporting::Pushed,
        native_turn_ids: false,
        usage: UsageReporting {
            tokens: true,
            cost: true,
        },
    }
}

/// How to launch one `claude` process.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Launch {
    /// A model id; `None` and the `default` sentinel pass no `--model` (trap 2).
    pub model: Option<String>,
    /// A native mode. A mode that the session does not offer runs as the most
    /// restrictive one (L8).
    pub mode: Option<String>,
    pub resume: Option<NativeSessionId>,
    pub fork: bool,
    /// More arguments (for example `--safe-mode` for recordings, DL-005).
    pub extra_args: Vec<String>,
}

/// The `claude` argument list of a driven session (ADR-001 §2.1).
#[must_use]
pub fn cli_args(launch: &Launch, caps: &Capabilities) -> Vec<String> {
    let mut args: Vec<String> = [
        "-p",
        "--input-format",
        "stream-json",
        "--output-format",
        "stream-json",
        "--verbose",
        "--include-partial-messages",
        "--replay-user-messages",
        "--permission-prompt-tool",
        "stdio",
    ]
    .iter()
    .map(|s| (*s).to_owned())
    .collect();
    // Always pass a mode: the user's settings can default to bypassPermissions (DL-004).
    let mode = caps
        .effective_mode(launch.mode.as_deref())
        .unwrap_or(SAFEST_MODE);
    args.extend(["--permission-mode".to_owned(), mode.to_owned()]);
    if let Some(model) = launch.model.as_deref().filter(|m| *m != "default") {
        args.extend(["--model".to_owned(), model.to_owned()]);
    }
    if let Some(id) = &launch.resume {
        args.extend(["--resume".to_owned(), id.0.clone()]);
        if launch.fork {
            args.push("--fork-session".to_owned());
        }
    }
    args.extend(launch.extra_args.iter().cloned());
    args
}

fn is_uuid(text: &str) -> bool {
    let parts: Vec<&str> = text.split('-').collect();
    parts.len() == 5
        && parts
            .iter()
            .zip([8, 4, 4, 4, 12])
            .all(|(p, n)| p.len() == n && p.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// The client uuid of an input. The CLI reports the fate of an input under a
/// uuid. A station input id is a ULID: its 128 bits become a version-4 uuid
/// (6 random bits are replaced by the version and variant). A uuid passes
/// unchanged (recorded fixtures).
pub fn input_uuid(id: &InputId) -> Result<String, ProviderError> {
    if is_uuid(id.as_str()) {
        return Ok(id.as_str().to_ascii_lowercase());
    }
    let ulid = ulid::Ulid::from_string(id.as_str())
        .map_err(|_| ProviderError::InvalidInput(id.to_string()))?;
    let bits = (ulid.0 & !(0xf << 76) | (0x4 << 76)) & !(0x3 << 62) | (0x2 << 62);
    Ok(format!(
        "{:08x}-{:04x}-{:04x}-{:04x}-{:012x}",
        bits >> 96,
        (bits >> 80) & 0xffff,
        (bits >> 64) & 0xffff,
        (bits >> 48) & 0xffff,
        bits & 0xffff_ffff_ffff
    ))
}

/// The version of `claude --version` output, for example `2.1.289 (Claude Code)`.
#[must_use]
pub fn parse_version(stdout: &str) -> Option<String> {
    let first = stdout.split_whitespace().next()?;
    first
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_digit())
        .then(|| first.to_owned())
}

/// The sign-in state from `claude auth status` (JSON with `loggedIn` and
/// `authMethod`). A failed command or unexpected output is `Unknown` (trap 9).
#[must_use]
pub fn parse_auth_status(success: bool, stdout: &str) -> AuthState {
    if !success {
        return AuthState::Unknown;
    }
    let Ok(value) = serde_json::from_str::<Value>(stdout.trim()) else {
        return AuthState::Unknown;
    };
    match value.get("loggedIn").and_then(Value::as_bool) {
        Some(true) => AuthState::SignedIn {
            method: value
                .get("authMethod")
                .and_then(Value::as_str)
                .map(str::to_owned),
        },
        Some(false) => AuthState::SignedOut,
        None => AuthState::Unknown,
    }
}
