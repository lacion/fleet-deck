//! The reader and writer tasks of the Claude driver.

use std::sync::Arc;

use serde_json::Value;
use tokio::io::{AsyncRead, BufReader};
use tokio::sync::{mpsc, watch};

use super::Shared;
use crate::adapter::ProviderError;
use crate::claude::effort;
use crate::claude::frames::{CliFrame, parse_value};
use crate::host::ProcessControl;
use crate::rawlog::RawDir;
use crate::stdio::{Fed, LineRead, Write, is_queue_full, read_line_bounded, release_write};

/// Token deltas do not go to the raw-frame log (L12).
pub(super) fn is_token_delta(frame: &Value) -> bool {
    frame.get("type").and_then(Value::as_str) == Some("stream_event")
        && frame
            .get("event")
            .and_then(|e| e.get("type"))
            .and_then(Value::as_str)
            == Some("content_block_delta")
}

pub(super) async fn write_loop(
    shared: Arc<Shared>,
    control: Arc<dyn ProcessControl>,
    mut rx: mpsc::UnboundedReceiver<Write>,
) {
    while let Some(item) = rx.recv().await {
        let frame = match item {
            Write::Frame(frame) => {
                release_write(&shared.queued);
                frame
            }
            Write::Close => {
                control.close_stdin().await;
                break;
            }
            Write::Flush(ack) => {
                let _ = ack.send(());
                continue;
            }
        };
        shared.log(RawDir::In, &frame);
        let mut line = frame.to_string();
        line.push('\n');
        if let Err(e) = control.write(line.as_bytes()).await {
            tracing::debug!(error = %e, "claude stdin closed");
            break;
        }
    }
}

pub(super) async fn read_loop(
    shared: Arc<Shared>,
    control: Arc<dyn ProcessControl>,
    stdout: Box<dyn AsyncRead + Send + Unpin>,
    writes: mpsc::UnboundedSender<Write>,
    frames: watch::Sender<u64>,
    max_line: usize,
) {
    let mut reader = BufReader::new(stdout);
    let mut buf = Vec::new();
    let mut sink_open = true;
    loop {
        match read_line_bounded(&mut reader, &mut buf, max_line).await {
            Ok(LineRead::Line) => {}
            Ok(LineRead::Eof) => break,
            Ok(LineRead::TooLong) => {
                // The line is not read to its end: stop the session (NV-20).
                tracing::warn!("provider stdout line over the limit");
                shared.note(
                    RawDir::Stderr,
                    "a stdout line is over 16 MiB: the session is stopped",
                );
                control.kill().await;
                break;
            }
            Err(e) => {
                tracing::warn!(error = %e, "claude stdout read failed");
                break;
            }
        }
        let line = match std::str::from_utf8(&buf) {
            Ok(line) => line,
            Err(e) => {
                tracing::warn!(error = %e, "claude stdout read failed");
                break;
            }
        };
        let mut stop = false;
        if sink_open {
            match feed_line(&shared, line, &writes).await {
                Fed::Open => {}
                Fed::SinkClosed => {
                    // The station dropped the session's events: nobody owns it.
                    sink_open = false;
                    control.kill().await;
                }
                Fed::QueueFull => {
                    // The process does not read stdin: stop it (NV-19).
                    tracing::warn!("provider stdin queue is full");
                    shared.note(
                        RawDir::Stderr,
                        "the stdin queue is full: the session is stopped",
                    );
                    control.kill().await;
                    stop = true;
                }
            }
        }
        frames.send_modify(|n| *n += 1);
        if stop {
            break;
        }
    }
    let report = control.wait().await;
    if !report.stderr_tail.is_empty() {
        shared.note(RawDir::Stderr, &report.stderr_tail);
    }
    let mut core = shared.core.lock().await;
    let out = core.on_exit(report.code());
    if sink_open {
        let _ = shared.dispatch(out, None, &writes).await;
    }
}

/// Feeds one stdout line.
async fn feed_line(shared: &Shared, line: &str, writes: &mpsc::UnboundedSender<Write>) -> Fed {
    if line.trim().is_empty() {
        return Fed::Open;
    }
    let (frame, raw_ref) = match serde_json::from_str::<Value>(line) {
        Ok(v) => {
            let raw_ref = (!is_token_delta(&v))
                .then(|| shared.log(RawDir::Out, &effort::raw_view(&v)))
                .flatten();
            let frame = parse_value(v).unwrap_or(CliFrame::Unknown(Value::Null));
            (frame, raw_ref)
        }
        Err(_) => {
            shared.note(RawDir::Out, line);
            (CliFrame::Unknown(Value::from(line)), None)
        }
    };
    let mut core = shared.core.lock().await;
    let out = core.on_frame(frame);
    match shared.dispatch(out, raw_ref.as_ref(), writes).await {
        Err(ProviderError::SinkClosed) => Fed::SinkClosed,
        Err(e) if is_queue_full(&e) => Fed::QueueFull,
        _ => Fed::Open,
    }
}
