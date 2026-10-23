//! The fleet skill requests of the Codex handshake (MCP P3 §4.2):
//! `skills/extraRoots/set`, then `skills/list` with `forceReload`, before
//! the thread open.

use serde_json::{Value, json};

use fleetdeck_proto::Event;

use super::{CodexCore, HostRequest, Output, rpc};
use crate::adapter::Result;

/// The most skill names in one list (B11, the same as the Claude core).
const MAX_COMMANDS: usize = 500;
/// The longest skill name, in bytes.
const MAX_COMMAND_NAME: usize = 128;

/// Bare, valid, sorted, unique, at most `MAX_COMMANDS` (a local copy of the
/// Claude rule: the two cores share nothing here).
fn normalize(names: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut out: Vec<String> = names
        .into_iter()
        .map(|n| n.strip_prefix('/').map_or(n.clone(), str::to_owned))
        .filter(|n| {
            !n.is_empty() && n.len() <= MAX_COMMAND_NAME && !n.chars().any(char::is_whitespace)
        })
        .collect();
    out.sort();
    out.dedup();
    out.truncate(MAX_COMMANDS);
    out
}

impl CodexCore {
    /// How long each fleet skill request of the handshake waits for its
    /// answer (MCP P3 §4.2), never past the handshake deadline.
    pub const SKILL_REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

    /// `skills/extraRoots/set` with the root `<dir>/skills` of the fleet
    /// skill tree (MCP P3 §4.2). A path that is not UTF-8 sends nothing and
    /// logs a warning.
    ///
    /// # Errors
    /// [`ProviderError::SessionEnded`] after the process ended.
    pub fn set_skill_root(&mut self, dir: &std::path::Path) -> Result<Vec<Output>> {
        self.live()?;
        let Some(root) = dir.join("skills").to_str().map(str::to_owned) else {
            tracing::warn!("the fleet skill tree path is not UTF-8; no skill injection");
            return Ok(Vec::new());
        };
        Ok(vec![self.skill_request(
            "skills/extraRoots/set",
            json!({"extraRoots": [root]}),
        )])
    }

    /// `skills/list` with `forceReload`, after [`Self::set_skill_root`]
    /// (MCP P3 §4.2).
    ///
    /// # Errors
    /// [`ProviderError::SessionEnded`] after the process ended.
    pub fn reload_skills(&mut self) -> Result<Vec<Output>> {
        self.live()?;
        Ok(vec![self.skill_request(
            "skills/list",
            json!({"forceReload": true}),
        )])
    }

    /// True while the skill request `request` (its method) waits for its
    /// answer; one that timed out stays until its late answer, if any.
    #[must_use]
    pub fn is_skill_request_pending(&self, request: &str) -> bool {
        self.skill_requests.values().any(|m| *m == request)
    }

    fn skill_request(&mut self, method: &'static str, params: Value) -> Output {
        self.next_host_id += 1;
        let id = format!("fd-{}", self.next_host_id);
        self.skill_requests.insert(id.clone(), method);
        Output::Write(rpc::request(&id, method, params))
    }

    /// Takes the answer of a skill request. An error answer is a warning
    /// with the request name only; the open goes on either way. Returns
    /// `false` when `id` is not a skill request.
    pub(super) fn skill_answer(&mut self, id: &Value, failed: bool) -> bool {
        let Some(method) = id.as_str().and_then(|i| self.skill_requests.remove(i)) else {
            return false;
        };
        if failed {
            tracing::warn!(request = method, "the fleet skill request failed");
        }
        true
    }

    /// `skills/list` with the session folder, for the board (B11).
    pub(in crate::codex) fn skill_catalog(&mut self) -> Output {
        let cwd = self
            .spec
            .cwd
            .clone()
            .unwrap_or_else(|| self.config.cwd.clone());
        let id = self.host_id(HostRequest::SkillCatalog);
        Output::Write(rpc::request(&id, "skills/list", json!({"cwds": [cwd]})))
    }

    /// The answer of [`Self::skill_catalog`]: the enabled skill names of
    /// every entry of `data`. Emits `SlashCommands { commands: [], skills }`
    /// when the list changed; the first list is emitted also when empty.
    pub(in crate::codex) fn on_skill_catalog(&mut self, r: &Value) -> Vec<Output> {
        let Some(data) = r.get("data").and_then(Value::as_array) else {
            return Vec::new();
        };
        let names = data
            .iter()
            .filter_map(|entry| entry.get("skills").and_then(Value::as_array))
            .flatten()
            .filter(|s| s.get("enabled").and_then(Value::as_bool) == Some(true))
            .filter_map(|s| s.get("name").and_then(Value::as_str))
            .map(str::to_owned);
        let skills = normalize(names);
        if self.emitted_skills.as_ref() == Some(&skills) {
            return Vec::new();
        }
        self.emitted_skills = Some(skills.clone());
        vec![Output::Emit(Event::SlashCommands {
            commands: Vec::new(),
            skills,
        })]
    }
}
