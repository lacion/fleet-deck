//! MCP Apps in the direct Claude driver (B15, design `docs/v2/design/mcp-apps.md`
//! §4.2): the environment variable that makes the CLI offer the extension, the
//! candidate channel of a session and the [`AppBridge`] handle that sends the
//! three app control requests and waits for their answers.

use std::sync::Arc;

use async_trait::async_trait;
use fleetdeck_core::ChildEnv;
use serde_json::Value;
use tokio::sync::{mpsc, watch};
use tokio::time::Instant;

use super::{Shared, WRITE_TIMEOUT};
use crate::adapter::ProviderError;
use crate::apps::{
    AppBridge, AppCandidate, AppError, AppTool, READ_TIMEOUT, TOOLS_TIMEOUT, ToolAnswer,
};
use crate::claude::map::apps::{AppOp, parse_tools};
use crate::stdio::{Write, flush};

/// The variable that makes Claude offer the `io.modelcontextprotocol/ui`
/// extension to its MCP servers. A CLI that does not know it ignores it.
pub(super) const APPS_HOST_ENV: &str = "CLAUDE_CODE_MCP_APPS_HOST";

/// The candidates that wait for the station. A candidate over the queue is
/// dropped with a warning: the reader never waits for the station.
pub(super) const CANDIDATE_QUEUE: usize = 64;

/// `env` with [`APPS_HOST_ENV`] set to `1`. Every Claude launch, direct and
/// durable, takes its environment through this function.
pub(super) fn with_apps_host(mut env: ChildEnv) -> ChildEnv {
    if let Err(e) = env.set(APPS_HOST_ENV, "1") {
        tracing::warn!(error = %e, "the MCP Apps host variable is not set");
    }
    env
}

/// The app side of a session: the candidate receiver until the station takes it.
#[derive(Debug)]
pub(super) struct SessionApps {
    candidates: Option<mpsc::Receiver<AppCandidate>>,
}

impl SessionApps {
    pub(super) fn new(candidates: mpsc::Receiver<AppCandidate>) -> Self {
        Self {
            candidates: Some(candidates),
        }
    }

    pub(super) fn take_candidates(&mut self) -> Option<mpsc::Receiver<AppCandidate>> {
        self.candidates.take()
    }

    /// The bridge of the session. It always exists; a CLI without the three
    /// capabilities makes every call fail with [`AppError::Unsupported`].
    pub(super) fn bridge(
        &self,
        shared: &Arc<Shared>,
        writes: &mpsc::UnboundedSender<Write>,
        frames: &watch::Receiver<u64>,
    ) -> Arc<dyn AppBridge> {
        Arc::new(ClaudeAppBridge {
            shared: Arc::clone(shared),
            writes: writes.clone(),
            frames: frames.clone(),
        })
    }
}

impl Shared {
    /// Hands a candidate to the station's channel without waiting.
    pub(super) fn offer_candidate(&self, candidate: AppCandidate) {
        if let Err(mpsc::error::TrySendError::Full(_)) = self.candidates.try_send(candidate) {
            tracing::warn!("app candidate dropped: the capture queue is full");
        }
    }
}

/// The app operations of one direct Claude session. It holds the shared core
/// and the write queue, never the process control: dropping the session still
/// stops the process.
#[derive(Debug, Clone)]
struct ClaudeAppBridge {
    shared: Arc<Shared>,
    writes: mpsc::UnboundedSender<Write>,
    frames: watch::Receiver<u64>,
}

type Answer = Result<Value, String>;

impl ClaudeAppBridge {
    /// Writes one request and waits for its answer: until `deadline`, or
    /// without a limit when there is none.
    async fn request(&self, op: AppOp, deadline: Option<Instant>) -> Result<Answer, AppError> {
        let id = {
            let mut core = self.shared.core.lock().await;
            let (id, out) = core.app_request(&op).map_err(|e| match e {
                ProviderError::SessionEnded => AppError::Ended,
                // Nothing was queued: the CLI has no app path.
                ProviderError::NotReady(_) => AppError::Unsupported,
                other => AppError::Answered(other.to_string()),
            })?;
            // The frame may be queued even when the dispatch then fails:
            // the failure proves nothing about the request.
            if let Err(e) = self.shared.dispatch(out, None, &self.writes).await {
                core.forget_app_request(&id);
                return Err(AppError::Answered(e.to_string()));
            }
            id
        };
        if let Err(e) = flush(&self.writes, WRITE_TIMEOUT).await {
            self.forget(&id).await;
            return Err(AppError::Answered(e.to_string()));
        }
        self.wait(&id, deadline).await
    }

    async fn forget(&self, id: &str) {
        self.shared.core.lock().await.forget_app_request(id);
    }

    /// The answer of `id`. A process that ended gives [`AppError::Ended`]; so
    /// does a reader that stopped without the exit (the session was dropped,
    /// which kills the process).
    async fn wait(&self, id: &str, deadline: Option<Instant>) -> Result<Answer, AppError> {
        let mut frames = self.frames.clone();
        loop {
            frames.borrow_and_update();
            {
                let mut core = self.shared.core.lock().await;
                if let Some(answer) = core.take_app_answer(id) {
                    return Ok(answer);
                }
                if core.has_ended() {
                    core.forget_app_request(id);
                    return Err(AppError::Ended);
                }
            }
            let step = match deadline {
                Some(at) => tokio::time::timeout_at(at, frames.changed())
                    .await
                    .map_err(|_| AppError::Timeout),
                None => Ok(frames.changed().await),
            };
            match step {
                Ok(Ok(())) => {}
                Err(timeout) => {
                    self.forget(id).await;
                    return Err(timeout);
                }
                // The reader task is gone: one last look at the core.
                Ok(Err(_)) => {
                    let mut core = self.shared.core.lock().await;
                    if let Some(answer) = core.take_app_answer(id) {
                        return Ok(answer);
                    }
                    core.forget_app_request(id);
                    return Err(AppError::Ended);
                }
            }
        }
    }
}

#[async_trait]
impl AppBridge for ClaudeAppBridge {
    async fn tools(&self) -> Result<Vec<AppTool>, AppError> {
        let deadline = Instant::now() + TOOLS_TIMEOUT;
        let answer = self
            .request(AppOp::Tools, Some(deadline))
            .await?
            .map_err(AppError::Answered)?;
        Ok(parse_tools(&answer))
    }

    async fn read_resource(&self, server: &str, uri: &str) -> Result<Value, AppError> {
        let op = AppOp::Read {
            server: server.to_owned(),
            uri: uri.to_owned(),
        };
        let deadline = Instant::now() + READ_TIMEOUT;
        self.request(op, Some(deadline))
            .await?
            .map_err(AppError::Answered)
    }

    /// Claude's success answer relays the server's result, so it proves that the
    /// tool handler returned; its timeouts, cancellations and lost transports
    /// are error answers, which prove nothing (fact 21).
    async fn call_tool(
        &self,
        server: &str,
        tool: &str,
        arguments: Value,
    ) -> Result<ToolAnswer, AppError> {
        let op = AppOp::Call {
            server: server.to_owned(),
            tool: tool.to_owned(),
            arguments,
        };
        let result = self.request(op, None).await?;
        result
            .map(|result| ToolAnswer {
                result,
                proves_end: true,
            })
            .map_err(AppError::Answered)
    }
}

#[cfg(test)]
#[path = "tests_apps.rs"]
mod tests;
#[cfg(test)]
#[path = "tests_apps_bridge.rs"]
mod tests_bridge;
