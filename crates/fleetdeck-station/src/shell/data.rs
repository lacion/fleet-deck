//! The data streams of one link's data channel: the shell streams (PLAN §6.8,
//! A7) and the move streams (FG-9, design §12.3).
//!
//! The deck opens a stream with the token of `shell.attach` (purpose
//! `shell:<id>`). The station writes the scrollback, then the live output;
//! the bytes that the deck writes are keystrokes for the PTY. A stream ends
//! when either side closes or resets it, when the shell ends, or when the
//! link ends; the shell itself keeps running.
//!
//! A stream with the purpose `move-export:<move_id>` (the token of
//! `session.move_export`) carries the transcript files of a move from the
//! station to the deck (`workspace/fleet_git/move_stream.rs`). A stream with
//! the purpose `move-import:<move_id>` (the token of `session.move_prepare`)
//! carries them from the deck to the station
//! (`workspace/fleet_git/move_upload.rs`). A stream with the purpose
//! `maintenance:<id>` (the token of `maintenance.attach`) is a relay to the
//! runner socket of a desktop tool installation (`maintenance/relay.rs`).

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use fleetdeck_link::data::{Data, DataEvent};
use fleetdeck_proto::desktop_tools::{STREAM_PURPOSE as MAINTENANCE_PURPOSE, is_hex_id};
use fleetdeck_proto::fleet_git::{MOVE_EXPORT_PURPOSE, MOVE_IMPORT_PURPOSE};
use fleetdeck_proto::shell::STREAM_PURPOSE;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use super::Shells;
use crate::maintenance::relay;
use crate::manager::SessionManager;
use crate::workspace::fleet_git::{UploadEvent, UploadPort, serve_upload};

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
    // The uploads of moves: each stream has a task that reads its events.
    let mut imports: HashMap<u32, mpsc::UnboundedSender<UploadEvent>> = HashMap::new();
    // The maintenance relays (BG-1): the bytes of the board go to the relay.
    let mut relays: HashMap<u32, (mpsc::UnboundedSender<Vec<u8>>, JoinHandle<()>)> = HashMap::new();
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
                if let Some(move_id) = purpose.strip_prefix(MOVE_IMPORT_PURPOSE) {
                    let (tx, rx) = mpsc::unbounded_channel();
                    tokio::spawn(import_stream(
                        Arc::clone(&manager),
                        handle.clone(),
                        stream,
                        move_id.to_owned(),
                        rx,
                    ));
                    imports.insert(stream, tx);
                    continue;
                }
                if let Some(id) = purpose.strip_prefix(MAINTENANCE_PURPOSE) {
                    if !is_hex_id(id) {
                        let _ = handle.reset(stream, "unknown_purpose").await;
                        continue;
                    }
                    let (tx, rx) = mpsc::unbounded_channel();
                    let task =
                        tokio::spawn(relay::serve(handle.clone(), stream, id.to_owned(), rx));
                    relays.insert(stream, (tx, task));
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
                // An upload task gives the credit back after it wrote.
                if let Some(upload) = imports.get(&stream) {
                    let _ = upload.send(UploadEvent::Data(bytes));
                    continue;
                }
                if let Some((relay, _)) = relays.get(&stream) {
                    // A relay that ended gives no credit back: do it here.
                    if let Err(mpsc::error::SendError(bytes)) = relay.send(bytes) {
                        let _ = handle.consumed(stream, bytes.len()).await;
                    }
                    continue;
                }
                let n = bytes.len();
                if let Some((id, _)) = streams.get(&stream)
                    && !shells.input(id, &bytes)
                {
                    let _ = handle.reset(stream, UNKNOWN_SHELL).await;
                }
                let _ = handle.consumed(stream, n).await;
            }
            DataEvent::Closed { stream } => {
                if let Some(upload) = imports.remove(&stream) {
                    let _ = upload.send(UploadEvent::Closed);
                    continue;
                }
                // The end of the board stream ends the relay only.
                if let Some((_, task)) = relays.remove(&stream) {
                    task.abort();
                    continue;
                }
                // The deck's half of an export ends nothing: the station
                // goes on writing.
                if let Some((_, task)) = streams.remove(&stream) {
                    task.abort();
                }
            }
            DataEvent::Reset { stream, .. } => {
                if let Some(upload) = imports.remove(&stream) {
                    let _ = upload.send(UploadEvent::Reset);
                }
                if let Some((_, task)) = streams.remove(&stream) {
                    task.abort();
                }
                if let Some(task) = exports.remove(&stream) {
                    task.abort();
                }
                if let Some((_, task)) = relays.remove(&stream) {
                    task.abort();
                }
            }
        }
    }
    for (_, (_, task)) in relays {
        task.abort();
    }
    for (_, (_, task)) in streams {
        task.abort();
    }
    for (_, task) in exports {
        task.abort();
    }
    // The upload tasks see their channel end and delete their folder.
    drop(imports);
}

async fn export_stream(manager: Arc<SessionManager>, handle: Data, stream: u32, move_id: String) {
    crate::workspace::fleet_git::serve_export(&manager, handle, stream, &move_id).await;
}

/// The stream of one upload: its data channel half, for the upload task.
struct DataPort {
    handle: Data,
    stream: u32,
}

#[async_trait]
impl UploadPort for DataPort {
    async fn consumed(&self, n: usize) {
        let _ = self.handle.consumed(self.stream, n).await;
    }

    async fn close(&self) {
        let _ = self.handle.close(self.stream).await;
    }

    async fn reset(&self, reason: &str) {
        let _ = self.handle.reset(self.stream, reason).await;
    }
}

async fn import_stream(
    manager: Arc<SessionManager>,
    handle: Data,
    stream: u32,
    move_id: String,
    events: mpsc::UnboundedReceiver<UploadEvent>,
) {
    let port = DataPort {
        handle: handle.clone(),
        stream,
    };
    // The discard resets the stream through this closure.
    let reset: Arc<dyn Fn(&str) + Send + Sync> = Arc::new(move |reason: &str| {
        let (handle, reason) = (handle.clone(), reason.to_owned());
        tokio::spawn(async move {
            let _ = handle.reset(stream, &reason).await;
        });
    });
    serve_upload(
        manager.store(),
        manager.move_uploads(),
        &port,
        reset,
        &move_id,
        events,
    )
    .await;
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
