//! Offline replay of a recorded fixture through the sans-IO adapter core.
//!
//! The replay needs no network and no `codex` binary. It rebuilds the session from
//! the fixture records:
//! - `spawn` starts a fresh [`CodexCore`] (a new process of the same session),
//! - `host` applies the recorded host command,
//! - `in` must equal the next frame that the core wrote (byte-for-byte as JSON),
//! - `out` feeds the recorded app-server frame to the core,
//! - `exit` tells the core that the process ended.
//!
//! The replay fails when the core writes a different frame than the live run, writes
//! a frame that the live run did not write, or rejects a recorded host command.

use std::collections::VecDeque;

use serde_json::Value;
use spike_canon::CanonEvent;
use spike_canon::fixture::{Fixture, Record};

use crate::adapter::{CodexCore, HostCmd, Output};
use crate::rpc::parse_value;

/// The result of a successful replay.
#[derive(Debug, Default)]
pub struct ReplayReport {
    /// Canonical events in order, over all processes of the fixture.
    pub events: Vec<CanonEvent>,
    /// Frames that the parser or the core did not know.
    pub unknown_frames: usize,
    /// Number of host writes compared with the recording.
    pub writes_checked: usize,
    /// Number of processes (`spawn` records).
    pub processes: usize,
}

/// A replay mismatch.
#[derive(Debug, thiserror::Error)]
pub enum ReplayError {
    #[error("record {index}: the core wrote {actual}, the recording has {expected}")]
    WriteMismatch {
        index: usize,
        expected: Value,
        actual: Value,
    },
    #[error("record {index}: the recording has a write ({expected}) that the core did not make")]
    MissingWrite { index: usize, expected: Value },
    #[error("record {index}: the core made writes that the recording does not have: {extra:?}")]
    ExtraWrites { index: usize, extra: Vec<Value> },
    #[error("record {index}: host command rejected: {message}")]
    HostRejected { index: usize, message: String },
    #[error("record {index}: a frame or command before the first spawn")]
    NoProcess { index: usize },
    #[error("record {index}: bad record: {message}")]
    BadRecord { index: usize, message: String },
}

/// Replays `fx` through a fresh core per process.
///
/// # Errors
/// See [`ReplayError`].
pub fn replay(fx: &Fixture) -> Result<ReplayReport, ReplayError> {
    let mut report = ReplayReport::default();
    let mut core: Option<CodexCore> = None;
    let mut queue: VecDeque<Value> = VecDeque::new();
    for (index, record) in fx.records.iter().enumerate() {
        match record {
            Record::Spawn { .. } => {
                finish_process(&mut core, &mut report);
                ensure_empty(&mut queue, index)?;
                core = Some(CodexCore::new());
                report.processes += 1;
            }
            Record::Host { cmd } => {
                let c = core.as_mut().ok_or(ReplayError::NoProcess { index })?;
                let cmd: HostCmd =
                    serde_json::from_value(cmd.clone()).map_err(|e| ReplayError::BadRecord {
                        index,
                        message: e.to_string(),
                    })?;
                let outs = c.apply(&cmd).map_err(|e| ReplayError::HostRejected {
                    index,
                    message: e.to_string(),
                })?;
                collect(outs, &mut report, &mut queue);
            }
            Record::In { frame } => {
                let actual = queue.pop_front().ok_or_else(|| ReplayError::MissingWrite {
                    index,
                    expected: frame.clone(),
                })?;
                if &actual != frame {
                    return Err(ReplayError::WriteMismatch {
                        index,
                        expected: frame.clone(),
                        actual,
                    });
                }
                report.writes_checked += 1;
            }
            Record::Out { frame } => {
                let c = core.as_mut().ok_or(ReplayError::NoProcess { index })?;
                let outs = c.on_frame(parse_value(frame.clone()));
                collect(outs, &mut report, &mut queue);
            }
            Record::Exit { code } => {
                let c = core.as_mut().ok_or(ReplayError::NoProcess { index })?;
                // Every write made while the process lived must be in the recording.
                ensure_empty(&mut queue, index)?;
                let outs = c.on_exit(*code);
                for o in outs {
                    match o {
                        Output::Emit(e) => report.events.push(e),
                        // A write after the exit has no reader; the live driver cannot
                        // write it either, so the recording cannot contain it.
                        Output::Write(_) => {}
                    }
                }
            }
            Record::Raw { .. } | Record::Note { .. } => {}
        }
    }
    finish_process(&mut core, &mut report);
    ensure_empty(&mut queue, fx.records.len())?;
    Ok(report)
}

fn collect(outs: Vec<Output>, report: &mut ReplayReport, queue: &mut VecDeque<Value>) {
    for o in outs {
        match o {
            Output::Emit(e) => report.events.push(e),
            Output::Write(v) => queue.push_back(v),
        }
    }
}

fn finish_process(core: &mut Option<CodexCore>, report: &mut ReplayReport) {
    if let Some(c) = core.take() {
        report.unknown_frames += c.unknown_frames();
    }
}

fn ensure_empty(queue: &mut VecDeque<Value>, index: usize) -> Result<(), ReplayError> {
    if queue.is_empty() {
        return Ok(());
    }
    Err(ReplayError::ExtraWrites {
        index,
        extra: queue.drain(..).collect(),
    })
}
