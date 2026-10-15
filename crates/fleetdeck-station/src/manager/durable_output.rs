//! The end-of-stream commits of the durable driver (ADR-005 §4.2): gaps, an
//! unterminated last line, and the exit of the provider.

use fleetdeck_proto::Event;
use fleetdeck_proto::event::OutputGapReason;
use fleetdeck_providers::host::ProcessControl as _;
use fleetdeck_providers::host::shim::wire::GapReason;
use tokio::io::BufReader;

use super::{
    DurableCore, DurableDriver, EXIT_WAIT, REASON_OUTPUT_FRAME_TOO_LARGE, Steps, snapshot,
};
use crate::store::{NewEvent, RequestTime};

impl<C: DurableCore> DurableDriver<C> {
    /// Commits `session.output_gap` for every reported gap and an `error`,
    /// after the rest of the stream (the shim stops the provider); the
    /// cursor is the end of the delivered stream. `None` when the commit
    /// failed or was fenced.
    pub(super) async fn commit_gaps(
        &self,
        reader: &mut BufReader<tokio::io::DuplexStream>,
        offset: u64,
        gaps: &[(u64, u64, GapReason)],
    ) -> Option<u64> {
        // The shim stops the provider after an overlong line, so the
        // stream ends; its bytes are not parsed.
        let _drained = tokio::io::copy(reader, &mut tokio::io::sink()).await;
        let gap_end = gaps.iter().map(|(_, to, _)| *to).max().unwrap_or(offset);
        let cursor = offset.max(gap_end).max(self.process.delivered());
        // The shim reports each discarded buffer of one overlong line as its
        // own gap; touching gaps of one reason are one range here.
        let mut merged: Vec<(u64, u64, GapReason)> = Vec::new();
        let mut sorted = gaps.to_vec();
        sorted.sort_by_key(|(from, _, _)| *from);
        for (from, to, reason) in sorted {
            match merged.last_mut() {
                Some(last) if last.2 == reason && from <= last.1 => last.1 = last.1.max(to),
                _ => merged.push((from, to, reason)),
            }
        }
        let mut events: Vec<NewEvent> = merged
            .iter()
            .map(|(from, to, reason)| {
                Event::OutputGap {
                    from: *from,
                    to: *to,
                    reason: match reason {
                        GapReason::OutputFrameTooLarge => OutputGapReason::OutputFrameTooLarge,
                        GapReason::BelowFloor => OutputGapReason::BelowFloor,
                    },
                }
                .into()
            })
            .collect();
        events.push(
            Event::Error {
                code: REASON_OUTPUT_FRAME_TOO_LARGE.to_owned(),
                message: "a provider output line reached the shim buffer size without a \
                          newline; the shim stopped the provider"
                    .to_owned(),
                retry_at: None,
            }
            .into(),
        );
        self.commit_plain(cursor, events).await
    }

    /// Commits `session.output_gap { unterminated_line }` for the bytes
    /// after the last newline at EOF (ADR-005 §4.2).
    pub(super) async fn commit_unterminated(&self, from: u64, to: u64) -> Option<u64> {
        let gap = Event::OutputGap {
            from,
            to,
            reason: OutputGapReason::UnterminatedLine,
        };
        self.commit_plain(to, vec![gap.into()]).await
    }

    async fn commit_plain(&self, cursor: u64, events: Vec<NewEvent>) -> Option<u64> {
        let core = self.core.lock().await;
        let checkpoint = snapshot(&*core).ok()?;
        let steps = Steps {
            events,
            ..Steps::default()
        };
        match self
            .commit(cursor, steps, RequestTime::Now, checkpoint, false)
            .await
        {
            Ok(true) => self.process.ack(cursor).await.then_some(cursor),
            Ok(false) | Err(_) => None,
        }
    }

    /// After EOF: waits for the shim's exit state and commits the core's
    /// exit events with the final cursor, then acknowledges the exit. The
    /// commit ends the open bindings of the shim (`exited`, DL-194).
    pub(super) async fn commit_exit(&self, offset: u64) {
        let known = tokio::time::timeout(EXIT_WAIT, self.process.wait()).await;
        let Ok(report) = known else {
            return;
        };
        let code = match report.termination {
            fleetdeck_providers::host::Termination::Unknown => return,
            fleetdeck_providers::host::Termination::Code(c) => Some(c),
            _ => None,
        };
        let mut core = self.core.lock().await;
        let steps = Steps {
            frames: Vec::new(),
            ..core.exit(code)
        };
        if let Ok(checkpoint) = snapshot(&*core)
            && matches!(
                self.commit(offset, steps, RequestTime::Now, checkpoint, true)
                    .await,
                Ok(true)
            )
        {
            let _acked = self.process.ack_exit().await;
        }
    }
}
