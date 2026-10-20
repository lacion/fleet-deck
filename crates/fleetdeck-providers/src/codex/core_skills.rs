//! The fleet skill requests of the Codex handshake (MCP P3 §4.2):
//! `skills/extraRoots/set`, then `skills/list` with `forceReload`, before
//! the thread open.

use serde_json::{Value, json};

use super::{CodexCore, Output, rpc};
use crate::adapter::Result;

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
}
