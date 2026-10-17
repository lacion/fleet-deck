//! Station-tagged stdin writes and `write_status` questions (ADR-005 §4.3;
//! slice 5, DL-105).
//!
//! The station claims the identity of every stdin frame (`write_id`,
//! `write_seq`) in a database transaction before the frame leaves
//! ([`WriteTag`]). [`ShimProcess::write_tagged`] sends a frame with that
//! identity and returns the shim's final record state.
//! [`ShimProcess::write_status`] asks the shim about a claimed identity
//! after a reattach; the answer feeds the §4.3 table ([`super::settle`]).

use std::sync::atomic::Ordering;
use std::time::Duration;

use tokio::sync::oneshot;

use super::host::ShimProcess;
use super::wire::{MAX_PAYLOAD, StatusAnswer, ToShim, WriteState};
use crate::host::HostError;

/// The durable identity of one stdin frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriteTag {
    pub write_id: String,
    pub write_seq: u64,
}

/// What the shim said about a tagged write frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WriteOutcome {
    /// The record reached this final state (`write_done`).
    Done(WriteState),
    /// The shim refused the frame at admission (`forgotten` or `mismatch`);
    /// no byte went to the pipe for this frame.
    Refused(StatusAnswer),
}

impl ShimProcess {
    /// Sends one stdin frame with the claimed `tag` and waits for its final
    /// state. A frame above [`MAX_PAYLOAD`] is refused before any I/O.
    ///
    /// # Errors
    /// [`HostError::StdinClosed`] when stdin was closed, the attachment
    /// ended before an answer, or the frame could not be sent;
    /// [`HostError::Write`] for an oversized frame.
    pub async fn write_tagged(
        &self,
        tag: &WriteTag,
        bytes: &[u8],
    ) -> Result<WriteOutcome, HostError> {
        if bytes.len() > MAX_PAYLOAD {
            return Err(HostError::Write(std::io::Error::other(format!(
                "a stdin frame holds at most {MAX_PAYLOAD} bytes"
            ))));
        }
        if self.stdin_closed.load(Ordering::SeqCst) {
            return Err(HostError::StdinClosed);
        }
        let (done_tx, done_rx) = oneshot::channel();
        self.shared.pending().insert(tag.write_id.clone(), done_tx);
        let frame = ToShim::Write {
            generation: self.generation,
            write_id: tag.write_id.clone(),
            write_seq: tag.write_seq,
            sent_at: 0,
        };
        if !self.send(frame, bytes.to_vec()).await || self.shared.ended() {
            self.shared.pending().remove(&tag.write_id);
            return Err(HostError::StdinClosed);
        }
        match done_rx.await {
            Ok(Ok(state)) => Ok(WriteOutcome::Done(state)),
            Ok(Err(answer)) => Ok(WriteOutcome::Refused(answer)),
            Err(_) => Err(HostError::StdinClosed),
        }
    }

    /// Asks the shim about `tag`. `None` is no answer within `wait` or an
    /// attachment that ended (the §4.3 table maps both to
    /// `outcome_unknown`).
    pub async fn write_status(&self, tag: &WriteTag, wait: Duration) -> Option<StatusAnswer> {
        let (tx, rx) = oneshot::channel();
        self.shared
            .statuses
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(tag.write_id.clone(), tx);
        let frame = ToShim::WriteStatus {
            generation: self.generation,
            write_id: tag.write_id.clone(),
            write_seq: tag.write_seq,
        };
        let answer = if self.send(frame, Vec::new()).await && !self.shared.ended() {
            tokio::time::timeout(wait, rx)
                .await
                .ok()
                .and_then(Result::ok)
        } else {
            None
        };
        self.shared
            .statuses
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&tag.write_id);
        answer
    }
}
