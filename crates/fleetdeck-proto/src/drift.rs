//! The config drift report of a station (design §14, FG-11, O33, O34):
//! hashes of the provider config files. The station sends hashes only,
//! never a file content.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// One watched item of the provider config (design §14.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "fleet_git/")]
pub enum DriftItem {
    /// `$HOME/.claude/CLAUDE.md`.
    ClaudeMd,
    /// `$HOME/.claude/agents/`.
    ClaudeAgents,
    /// `$HOME/.claude/skills/`.
    ClaudeSkills,
    /// `$HOME/.codex/AGENTS.md`.
    CodexAgentsMd,
    /// An item of a newer station.
    #[serde(other)]
    Unknown,
}

/// The hash of one item: 64 lower-case hex characters, `absent`,
/// `unreadable` or `too_large` (design §14.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "fleet_git/")]
pub struct DigestItem {
    pub name: DriftItem,
    pub hash: String,
}

/// The config digest in `station.info` (design §14.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "fleet_git/")]
pub struct ConfigDigest {
    /// The probe version of the first Claude instance.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub claude_version: Option<String>,
    /// The probe version of the first Codex instance.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub codex_version: Option<String>,
    /// Always the four items, in the order of [`DriftItem`].
    #[serde(default)]
    pub items: Vec<DigestItem>,
    /// Unix ms.
    pub computed_at: i64,
}
