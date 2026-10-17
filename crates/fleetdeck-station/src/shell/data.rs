//! The data streams of one link's data channel: the shell streams (PLAN §6.8,
//! A7) and the move export streams (FG-9, design §12.3).
//!
//! The deck opens a stream with the token of `shell.attach` (purpose
//! `shell:<id>`). The station writes the scrollback, then the live output;
//! the bytes that the deck writes are keystrokes for the PTY. A stream ends
//! when either side closes or resets it, when the shell ends, or when the
//! link ends; the shell itself keeps running.
//!
//! A stream with the purpose `move-export:<move_id>` (the token of
//! `session.move_export`) carries the transcript files of a move from the
//! station to the deck (`workspace/fleet_git/move_stream.rs`).

use std::collections::HashMap;
use std::sync::Arc;

use fleetdeck_link::data::{Data, DataEvent};
use fleetdeck_proto::fleet_git::MOVE_EXPORT_PURPOSE;
use fleetdeck_proto::shell::STREAM_PURPOSE;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use super::Shells;
use crate::manager::SessionManager;

/// The stream reset reason for a shell that does not exist.
pub const UNKNOWN_SHELL: &str = "unknown_shell";

/// Serves the streams of one data channel until it ends.
pub async fn serve(
    shells: Arc<Shells>,
    manager: Arc<SessionManager>,
    handle: Data,
    mut events: mpsc::UnboundedReceiver<DataEvent>,
) {
    let mut streams: HashMap<u32, (String, JoinHandle<()>)> = HashMap::new();
    let mut exports: HashMap<u32, JoinHandle<()>> = HashMap::new();
    while let Some(event) = events.recv().await {
        match event {
            DataEvent::Opened { stream, purpose } => {
                if let Some(move_id) = purpose.strip_prefix(MOVE_EXPORT_PURPOSE) {
                    let task = tokio::spawn(export_stream(
                        Arc::clone(&manager),
                        handle.clone(),
                        stream,
                        move_id.to_owned(),
                    ));
                    exports.insert(stream, task);
                    continue;
                }
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
            DataEvent::Closed { stream } => {
                // The deck's half of an export ends nothing: the station
                // goes on writing.
                if let Some((_, task)) = streams.remove(&stream) {
                    task.abort();
                }
            }
            DataEvent::Reset { stream, .. } => {
                if let Some((_, task)) = streams.remove(&stream) {
                    task.abort();
                }
                if let Some(task) = exports.remove(&stream) {
                    task.abort();
                }
            }
        }
    }
    for (_, (_, task)) in streams {
        task.abort();
    }
    for (_, task) in exports {
        task.abort();
    }
}

async fn export_stream(manager: Arc<SessionManager>, handle: Data, stream: u32, move_id: String) {
    crate::workspace::fleet_git::serve_export(&manager, handle, stream, &move_id).await;
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
