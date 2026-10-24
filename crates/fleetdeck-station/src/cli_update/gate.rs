//! The gate of an update target (design §3.4). Pure: no clock, no network.

use fleetdeck_proto::cli_update::reason;
use fleetdeck_proto::event::ProviderKind;
use fleetdeck_proto::link::VersionStatus;
use fleetdeck_providers::coordinator::{bridge_handoff, handoff};
use fleetdeck_providers::version::version_status;

pub use fleetdeck_proto::cli_update::is_release;

/// What the gate needs to know.
#[derive(Debug, Clone)]
pub struct GateInput<'a> {
    pub kind: ProviderKind,
    pub installed: Option<&'a str>,
    pub target: &'a str,
    pub pinned: bool,
    /// The station hosts sessions in a shim (`host = "shim"`).
    pub shim_host: bool,
    /// Unix ms when the target was published; `None` when unknown.
    pub published_at: Option<i64>,
    pub now_ms: i64,
    pub min_age_hours: u16,
}

/// What the gate allows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateVerdict {
    Install,
    UpToDate,
}

/// The three numbers of a release version, or of the first word of a probe
/// answer (`2.1.309 (Claude Code)`, `codex-cli 0.163.0`).
fn numbers(text: &str) -> Option<[u64; 3]> {
    let word = text
        .split_whitespace()
        .find(|w| w.bytes().next().is_some_and(|b| b.is_ascii_digit()))?;
    let mut parts = word.split('.');
    let mut out = [0_u64; 3];
    for slot in &mut out {
        *slot = parts.next()?.parse().ok()?;
    }
    parts.next().is_none().then_some(out)
}

/// The verdict for `input`.
///
/// # Errors
/// A code of [`reason`].
pub fn gate(input: &GateInput<'_>) -> Result<GateVerdict, &'static str> {
    // 1. A target that is not a release never reaches an argv.
    if !is_release(input.target) {
        return Err(reason::NOT_A_VERSION);
    }
    // 2. The adapter's version check.
    match version_status(input.kind, Some(input.target)) {
        VersionStatus::Broken => return Err(reason::KNOWN_BROKEN),
        VersionStatus::Ok => {}
        _ => return Err(reason::BELOW_TESTED),
    }
    // 3. The handoff minimums.
    if handoff(input.kind, Some(input.target)).is_err() {
        return Err(reason::BELOW_HANDOFF);
    }
    if input.shim_host && bridge_handoff(input.kind, Some(input.target)).is_err() {
        return Err(reason::BELOW_HANDOFF);
    }
    // 4. Up to date. No downgrade without a pin.
    let installed = input.installed.and_then(numbers);
    let target = numbers(input.target);
    if let (Some(have), Some(want)) = (installed, target) {
        if input.pinned {
            if have == want {
                return Ok(GateVerdict::UpToDate);
            }
        } else if have >= want {
            return Ok(GateVerdict::UpToDate);
        }
    }
    // 5. Soak.
    if !input.pinned && input.min_age_hours > 0 {
        let Some(published) = input.published_at else {
            return Err(reason::PUBLISH_TIME_UNKNOWN);
        };
        if input.now_ms.saturating_sub(published) < i64::from(input.min_age_hours) * 3_600_000 {
            return Err(reason::TOO_NEW);
        }
    }
    // 6.
    Ok(GateVerdict::Install)
}

#[cfg(test)]
#[path = "gate_tests.rs"]
mod tests;
