//! The shell streams of one link's data channel (PLAN §6.8, A7).
//!
//! The deck opens a stream with the token of `shell.attach` (purpose
//! `shell:<id>`). The station writes the scrollback, then the live output;
//! the bytes that the deck writes are keystrokes for the PTY. A stream ends
//! when either side closes or resets it, when the shell ends, or when the
//! link ends; the shell itself keeps running.

use std::collections::HashMap;
use std::sync::Arc;

use fleetdeck_link::data::{Data, DataEvent};
use fleetdeck_proto::shell::STREAM_PURPOSE;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use super::Shells;

/// The stream reset reason for a shell that does not exist.
pub const UNKNOWN_SHELL: &str = "unknown_shell";

/// Serves the shell streams of one data channel until it ends.
pub async fn serve(
    shells: Arc<Shells>,
    handle: Data,
    mut events: mpsc::UnboundedReceiver<DataEvent>,
) {
    let mut streams: HashMap<u32, (String, JoinHandle<()>)> = HashMap::new();
    while let Some(event) = events.recv().await {
        match event {
            DataEvent::Opened { stream, purpose } => {
                let Some(id) = purpose.strip_prefix(STREAM_PURPOSE).map(str::to_owned) else {
                    let _ = handle.reset(stream, "unknown_purpose").await;
                    continue;
                };
                let Some((backlog, live)) = shells.attach(&id) else {
                    let _ = handle.reset(stream, UNKNOWN_SHELL).await;
                    continue;
                };
                let task = tokio::spawn(forward(handle.clone(), stream, backlog, live));
                streams.insert(stream, (id, task));
            }
            DataEvent::Data { stream, bytes } => {
                let n = bytes.len();
                if let Some((id, _)) = streams.get(&stream)
                    && !shells.input(id, &bytes)
                {
                    let _ = handle.reset(stream, UNKNOWN_SHELL).await;
                }
                let _ = handle.consumed(stream, n).await;
            }
            DataEvent::Closed { stream } | DataEvent::Reset { stream, .. } => {
                if let Some((_, task)) = streams.remove(&stream) {
                    task.abort();
                }
            }
        }
    }
    for (_, (_, task)) in streams {
        task.abort();
    }
}

async fn forward(handle: Data, stream: u32, backlog: Vec<u8>, mut live: mpsc::Receiver<Vec<u8>>) {
    if !backlog.is_empty() && handle.write(stream, backlog).await.is_err() {
        return;
    }
    while let Some(bytes) = live.recv().await {
        if handle.write(stream, bytes).await.is_err() {
            return;
        }
    }
    // The shell ended (or was closed).
    let _ = handle.close(stream).await;
}
