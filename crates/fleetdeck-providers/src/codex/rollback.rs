//! Native conversation rollback with `thread/rollback` (PLAN §6.6, DL-087).
//!
//! The rollback drops whole turns from the end of the thread. It never
//! touches the workspace: the station restores the files from its own
//! checkpoints after this call confirmed the rollback.

use std::time::Duration;

use serde_json::{Value, json};

use super::core::{CodexCore, HostRequest, Output};
use super::driver::CodexSession;
use super::rpc;
use crate::adapter::{ProviderError, Result};

/// How long a rollback waits for the app-server's answer.
pub const ROLLBACK_TIMEOUT: Duration = Duration::from_secs(30);

/// The last `thread/rollback` of a core.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(super) enum RollbackState {
    /// On the wire; no answer yet.
    Sent,
    /// The answer: `Ok`, or the provider's refusal.
    Done(std::result::Result<(), String>),
}

impl CodexCore {
    /// Checks that a rollback of `turns` can go out now; nothing is written.
    ///
    /// # Errors
    /// [`ProviderError::Refused`] when a turn runs, input waits, or another
    /// rollback is on its way; the thread errors of [`CodexCore::thread`].
    pub fn check_rollback(&self, turns: u32) -> Result<()> {
        self.thread()?;
        if turns == 0 {
            return Err(ProviderError::Refused("no turn to roll back".to_owned()));
        }
        if !self.is_idle() {
            return Err(ProviderError::Refused(
                "a turn runs or input waits".to_owned(),
            ));
        }
        if matches!(self.rollback, Some(RollbackState::Sent)) {
            return Err(ProviderError::Refused(
                "another rollback is on its way".to_owned(),
            ));
        }
        Ok(())
    }

    /// Refuses conversation input while a rollback has no answer: a late
    /// answer could still drop turns, so nothing may enter the thread
    /// until the answer arrives or the process ends (review sa-56b1636a).
    ///
    /// # Errors
    /// [`ProviderError::Refused`] while a rollback is on its way.
    pub(super) fn no_rollback_outstanding(&self) -> Result<()> {
        if matches!(self.rollback, Some(RollbackState::Sent)) {
            return Err(ProviderError::Refused(
                "a conversation rollback has no answer yet".to_owned(),
            ));
        }
        Ok(())
    }

    /// The `thread/rollback` frame for the last `turns` turns.
    ///
    /// # Errors
    /// The errors of [`CodexCore::check_rollback`].
    pub fn rollback(&mut self, turns: u32) -> Result<Vec<Output>> {
        self.check_rollback(turns)?;
        let thread = self.thread()?;
        let id = self.host_id(HostRequest::Rollback);
        self.rollback = Some(RollbackState::Sent);
        Ok(vec![Output::Write(rpc::request(
            &id,
            "thread/rollback",
            json!({"threadId": thread, "numTurns": turns}),
        ))])
    }

    pub(super) fn on_rollback(
        &mut self,
        result: std::result::Result<Value, (i64, String)>,
    ) -> Vec<Output> {
        let done = result
            .map(|_| ())
            .map_err(|(code, message)| format!("rpc_{code}: {message}"));
        self.rollback = Some(RollbackState::Done(done));
        Vec::new()
    }

    /// The answer of the last `thread/rollback`: `None` while none was sent
    /// or it has no answer yet; `Some(Err(refusal))` for the provider's
    /// refusal (used by the station's durable driver, DL-116).
    #[must_use]
    pub fn rollback_answer(&self) -> Option<std::result::Result<(), String>> {
        match &self.rollback {
            Some(RollbackState::Done(done)) => Some(done.clone()),
            Some(RollbackState::Sent) | None => None,
        }
    }

    pub(super) fn rollback_answered(&self) -> bool {
        matches!(self.rollback, Some(RollbackState::Done(_)))
    }
}

impl CodexSession {
    /// Sends `thread/rollback` and waits for its answer. An error before
    /// the write is definite; every failure after it is
    /// [`ProviderError::Uncertain`], except the provider's own refusal.
    pub(super) async fn roll_back(&mut self, turns: u32) -> Result<()> {
        self.with_core(|c| c.check_rollback(turns)).await?;
        let uncertain = |e: ProviderError| ProviderError::Uncertain(e.to_string());
        self.run(|c| c.rollback(turns)).await.map_err(uncertain)?;
        let deadline = tokio::time::Instant::now() + self.rollback_timeout;
        self.wait_until(deadline, CodexCore::rollback_answered)
            .await
            .map_err(|e| match e {
                ProviderError::NotReady(_) => {
                    ProviderError::Uncertain("no rollback answer before the deadline".to_owned())
                }
                other => uncertain(other),
            })?;
        let answer = self.with_core(|c| c.rollback.clone()).await;
        match answer {
            Some(RollbackState::Done(Ok(()))) => Ok(()),
            Some(RollbackState::Done(Err(refusal))) => Err(ProviderError::Refused(refusal)),
            _ => Err(ProviderError::Uncertain(
                "the rollback answer went missing".to_owned(),
            )),
        }
    }
}
