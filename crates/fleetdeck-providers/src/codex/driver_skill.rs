//! The fleet skill requests of the Codex handshake (MCP P3 §4.2).

use std::path::Path;

use super::{CodexCore, CodexSession, Output};
use crate::adapter::{ProviderError, Result};

impl CodexSession {
    /// The fleet skill requests before the thread open (MCP P3 §4.2):
    /// `skills/extraRoots/set`, then `skills/list` with `forceReload`. A
    /// request waits for its answer at most [`CodexCore::SKILL_REQUEST_TIMEOUT`]. An
    /// error answer or a timeout is a warning and the open goes on. With
    /// less than two timeouts left before `deadline`, both are skipped.
    pub(super) async fn inject_skill(
        &mut self,
        dir: &Path,
        deadline: tokio::time::Instant,
    ) -> Result<()> {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        if left < CodexCore::SKILL_REQUEST_TIMEOUT * 2 {
            tracing::warn!("skill injection skipped: handshake deadline");
            return Ok(());
        }
        self.skill_step("skills/extraRoots/set", deadline, |c| c.set_skill_root(dir))
            .await?;
        self.skill_step("skills/list", deadline, CodexCore::reload_skills)
            .await
    }

    async fn skill_step(
        &mut self,
        request: &'static str,
        deadline: tokio::time::Instant,
        op: impl FnOnce(&mut CodexCore) -> Result<Vec<Output>> + Send,
    ) -> Result<()> {
        self.run(op).await.map_err(super::handshake_gone)?;
        let until = deadline.min(tokio::time::Instant::now() + CodexCore::SKILL_REQUEST_TIMEOUT);
        match self
            .wait_until(until, |c| !c.is_skill_request_pending(request))
            .await
        {
            Err(ProviderError::NotReady(_)) => {
                tracing::warn!(request, "the fleet skill request timed out");
                Ok(())
            }
            other => other,
        }
    }
}
