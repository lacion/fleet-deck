//! The relay of the data stream `maintenance:<id>` to the runner socket
//! (`board-gaps.md` §2.9, Attach).
//!
//! The relay never stops a runner: the end of the board stream, or of the
//! link, only drops the connection to the runner. Input goes to the runner
//! while no `Exit` was seen; after it, board bytes are dropped (never queued
//! and never replayed).

use async_trait::async_trait;
use fleetdeck_link::data::Data;
use fleetdeck_proto::desktop_tools::MaintenanceState;
use tokio::sync::mpsc;

use super::files::{Spec, read_output, read_spec};
use super::frame::Frame;
use super::records::state_of;
use super::runner_client::{Reader, Writer, connect};
use super::service::{MaintenanceService, service};

/// The stream reset reason for an ID that has no spec.
const UNKNOWN_MAINTENANCE: &str = "unknown_maintenance";
/// The stream reset reason when no maintenance service is installed.
const UNAVAILABLE: &str = "unavailable";

/// A command for the live relay of a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelayCmd {
    /// Send a new terminal size to the runner.
    Resize { cols: u16, rows: u16 },
}

/// The board side of one relay: the data stream. Tests replace it.
#[async_trait]
pub trait RelayPort: Send + Sync {
    /// Sends bytes to the board. `false`: the stream is gone.
    async fn write(&self, bytes: Vec<u8>) -> bool;
    /// Ends the sending half of the stream.
    async fn close(&self);
    /// Aborts the stream.
    async fn reset(&self, reason: &str);
    /// Gives `n` consumed bytes back to the board.
    async fn consumed(&self, n: usize);
}

/// The data stream of one link.
struct DataPort {
    handle: Data,
    stream: u32,
}

#[async_trait]
impl RelayPort for DataPort {
    async fn write(&self, bytes: Vec<u8>) -> bool {
        self.handle.write(self.stream, bytes).await.is_ok()
    }

    async fn close(&self) {
        let _ = self.handle.close(self.stream).await;
    }

    async fn reset(&self, reason: &str) {
        let _ = self.handle.reset(self.stream, reason).await;
    }

    async fn consumed(&self, n: usize) {
        let _ = self.handle.consumed(self.stream, n).await;
    }
}

/// Serves the stream `stream` of the run `id`. `board` holds the bytes that
/// the board writes; its end is the end of the board stream.
pub async fn serve(handle: Data, stream: u32, id: String, board: mpsc::UnboundedReceiver<Vec<u8>>) {
    let port = DataPort { handle, stream };
    let Some(svc) = service() else {
        port.reset(UNAVAILABLE).await;
        return;
    };
    serve_port(&svc, &port, &id, board).await;
}

/// Drops the bytes of the board until its stream ends.
async fn drain(port: &dyn RelayPort, board: &mut mpsc::UnboundedReceiver<Vec<u8>>) {
    while let Some(bytes) = board.recv().await {
        port.consumed(bytes.len()).await;
    }
}

/// The relay of the run `id` on `port`.
pub(super) async fn serve_port(
    svc: &MaintenanceService,
    port: &dyn RelayPort,
    id: &str,
    mut board: mpsc::UnboundedReceiver<Vec<u8>>,
) {
    let Ok(spec) = read_spec(&svc.dir, id) else {
        port.reset(UNKNOWN_MAINTENANCE).await;
        return;
    };
    let (state, _, _) = state_of(svc.host.as_ref(), &svc.dir, &spec).await;
    if matches!(
        state,
        MaintenanceState::Admitted | MaintenanceState::Running
    ) {
        match connect(&svc.dir, &spec).await {
            Ok(client) => {
                let (reader, writer) = client.split();
                live(svc, port, &spec, reader, writer, &mut board).await;
                port.close().await;
                drain(port, &mut board).await;
                return;
            }
            Err(e) => tracing::debug!(error = %e, "a maintenance relay found no runner"),
        }
    }
    ended(svc, port, id).await;
    drain(port, &mut board).await;
}

/// A run that ended: its output, when it exists, and the end of the stream.
async fn ended(svc: &MaintenanceService, port: &dyn RelayPort, id: &str) {
    match read_output(&svc.dir, id) {
        Ok(Some(bytes)) if !bytes.is_empty() => {
            let _ = port.write(bytes).await;
        }
        Ok(_) => {}
        Err(e) => tracing::warn!(error = %e, "the maintenance output is not readable"),
    }
    port.close().await;
}

/// Removes the relay from the registry at drop.
struct Registration<'a> {
    svc: &'a MaintenanceService,
    id: &'a str,
    ticket: u64,
}

impl Drop for Registration<'_> {
    fn drop(&mut self) {
        self.svc.unregister_relay(self.id, self.ticket);
    }
}

/// The relay of a live run: the output of the runner to the board, and the
/// bytes and the resizes of the board to the runner, until the exit, the
/// end of the board stream, or the end of the connection.
async fn live(
    svc: &MaintenanceService,
    port: &dyn RelayPort,
    spec: &Spec,
    mut reader: Reader,
    mut writer: Writer,
    board: &mut mpsc::UnboundedReceiver<Vec<u8>>,
) {
    let (ticket, mut cmds) = svc.register_relay(&spec.maintenance_id);
    let _registration = Registration {
        svc,
        id: &spec.maintenance_id,
        ticket,
    };
    let output = async {
        loop {
            match reader.next().await {
                Ok(Frame::Output(bytes)) => {
                    if !port.write(bytes).await {
                        return;
                    }
                }
                Ok(Frame::SnapshotEnd) => {}
                // `Exit`, a frame that does not belong, or an error.
                _ => return,
            }
        }
    };
    let input = async {
        loop {
            tokio::select! {
                bytes = board.recv() => {
                    let Some(bytes) = bytes else { return };
                    let n = bytes.len();
                    let sent = writer.input(&bytes).await;
                    port.consumed(n).await;
                    if sent.is_err() {
                        return;
                    }
                }
                cmd = cmds.recv() => match cmd {
                    Some(RelayCmd::Resize { cols, rows }) => {
                        if writer.resize(cols, rows).await.is_err() {
                            return;
                        }
                    }
                    // A newer relay took over: this one ends.
                    None => return,
                },
            }
        }
    };
    // The end of either side ends the relay; the runner goes on.
    tokio::select! {
        () = output => {}
        () = input => {}
    }
}
