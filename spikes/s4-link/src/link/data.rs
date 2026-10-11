//! The data channel task: [`Mux`] streams over binary WebSocket messages.
//!
//! A write waits until the peer granted enough credit; the application gives
//! credit back with [`Data::consumed`] after it used received bytes, so a slow
//! reader slows the sender instead of filling memory.

use std::collections::{BTreeMap, VecDeque};

use futures_util::{SinkExt, StreamExt};
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::tungstenite::Message;

use super::{LinkError, Ws};
use crate::hello::Side;
use crate::mux::{Mux, MuxEvent, MuxFrame};

enum Cmd {
    Open(String, oneshot::Sender<Result<u32, LinkError>>),
    Write(u32, Vec<u8>, oneshot::Sender<Result<(), LinkError>>),
    Close(u32),
    Consumed(u32, usize),
}

/// The handle of a data channel. Cheap to clone.
#[derive(Clone)]
pub struct Data {
    tx: mpsc::Sender<Cmd>,
}

impl Data {
    /// Opens a stream with a purpose text, for example `artifact:<id>`.
    pub async fn open(&self, purpose: &str) -> Result<u32, LinkError> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(Cmd::Open(purpose.to_owned(), tx))
            .await
            .map_err(|_| LinkError::gone())?;
        rx.await.map_err(|_| LinkError::gone())?
    }

    /// Writes all of `bytes`; returns when the last chunk was sent.
    pub async fn write(&self, stream: u32, bytes: Vec<u8>) -> Result<(), LinkError> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(Cmd::Write(stream, bytes, tx))
            .await
            .map_err(|_| LinkError::gone())?;
        rx.await.map_err(|_| LinkError::gone())?
    }

    /// Ends the sending half of `stream`.
    pub async fn close(&self, stream: u32) -> Result<(), LinkError> {
        self.tx
            .send(Cmd::Close(stream))
            .await
            .map_err(|_| LinkError::gone())
    }

    /// Gives `n` consumed bytes of `stream` back to the peer as credit.
    pub async fn consumed(&self, stream: u32, n: usize) -> Result<(), LinkError> {
        self.tx
            .send(Cmd::Consumed(stream, n))
            .await
            .map_err(|_| LinkError::gone())
    }
}

/// What the application receives from the data channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DataEvent {
    Opened {
        stream: u32,
        purpose: String,
    },
    Data {
        stream: u32,
        bytes: Vec<u8>,
    },
    Closed {
        stream: u32,
    },
    Reset {
        stream: u32,
        code: String,
    },
    /// The data channel ended.
    Ended {
        reason: String,
    },
}

type Pending = VecDeque<(Vec<u8>, usize, oneshot::Sender<Result<(), LinkError>>)>;

/// The two ties between the data channel and its control channel: `stop`
/// ends the data channel when the control channel ends, and `control` ends
/// the control channel when the data channel is lost.
pub(crate) struct Ties {
    pub stop: oneshot::Receiver<(String, String)>,
    pub control: mpsc::Sender<(String, String)>,
}

pub(crate) fn spawn(ws: Ws, side: Side, ties: Ties) -> (Data, mpsc::UnboundedReceiver<DataEvent>) {
    let (tx, rx) = mpsc::channel(64);
    // Unbounded is safe: the peer cannot send more than the credit window
    // of each open stream.
    let (ev_tx, ev_rx) = mpsc::unbounded_channel();
    tokio::spawn(run(ws, Mux::new(side), rx, ev_tx, ties));
    (Data { tx }, ev_rx)
}

/// Frames go to the writer task, so the reader never waits for the socket:
/// both sides may write a full credit window at the same time, and a socket
/// buffer smaller than the window (FIFOs, small TCP buffers) must not
/// deadlock two writers that both wait for the other to read. Memory stays
/// bounded: data frames never exceed the peer's credit, other frames are
/// small and caused by the peer's own frames.
type Out = mpsc::UnboundedSender<Message>;

fn send(out: &Out, frames: Vec<MuxFrame>) -> Result<(), String> {
    for f in frames {
        out.send(Message::binary(f.encode()))
            .map_err(|_| "the data channel writer ended".to_owned())?;
    }
    Ok(())
}

fn flush(
    out: &Out,
    mux: &mut Mux,
    pending: &mut BTreeMap<u32, Pending>,
    stream: u32,
) -> Result<(), String> {
    let Some(queue) = pending.get_mut(&stream) else {
        return Ok(());
    };
    while let Some((buf, off, _)) = queue.front_mut() {
        let (frames, taken) = match mux.send(stream, &buf[*off..]) {
            Ok(r) => r,
            Err(e) => {
                if let Some((_, _, reply)) = queue.pop_front() {
                    let _ = reply.send(Err(LinkError::Protocol(e.to_string())));
                }
                continue;
            }
        };
        send(out, frames)?;
        *off += taken;
        if *off < buf.len() {
            break; // wait for credit
        }
        if let Some((_, _, reply)) = queue.pop_front() {
            let _ = reply.send(Ok(()));
        }
    }
    Ok(())
}

async fn run(
    ws: Ws,
    mut mux: Mux,
    mut cmds: mpsc::Receiver<Cmd>,
    events: mpsc::UnboundedSender<DataEvent>,
    mut ties: Ties,
) {
    let (mut sink, mut ws) = ws.split();
    let (out, mut out_rx) = mpsc::unbounded_channel::<Message>();
    let (write_err_tx, mut write_err) = oneshot::channel::<String>();
    let mut writer = tokio::spawn(async move {
        while let Some(m) = out_rx.recv().await {
            if let Err(e) = sink.send(m).await {
                let _ = write_err_tx.send(e.to_string());
                return;
            }
        }
        // Every sender is gone: the channel ends with a WebSocket close.
        let _ = sink.close().await;
    });
    let mut pending: BTreeMap<u32, Pending> = BTreeMap::new();
    let mut by_control = false;
    let mut cmds_open = true;
    let reason = loop {
        tokio::select! {
            stop = &mut ties.stop => {
                by_control = true;
                break match stop {
                    Ok((reason, _)) => reason,
                    Err(_) => "closed".to_owned(),
                };
            }
            e = &mut write_err => {
                break e.unwrap_or_else(|_| "the data channel writer ended".to_owned());
            }
            cmd = cmds.recv(), if cmds_open => {
                let r = match cmd {
                    // The application dropped every handle; keep receiving
                    // until the control channel ends the link.
                    None => {
                        cmds_open = false;
                        Ok(())
                    }
                    Some(Cmd::Open(purpose, reply)) => match mux.open(&purpose) {
                        Ok((stream, frame)) => {
                            let r = send(&out, vec![frame]);
                            let _ = reply.send(Ok(stream));
                            r
                        }
                        Err(e) => {
                            let _ = reply.send(Err(LinkError::Protocol(e.to_string())));
                            Ok(())
                        }
                    },
                    Some(Cmd::Write(stream, bytes, reply)) => {
                        pending.entry(stream).or_default().push_back((bytes, 0, reply));
                        flush(&out, &mut mux, &mut pending, stream)
                    }
                    Some(Cmd::Close(stream)) => match mux.close(stream) {
                        Ok(f) => send(&out, vec![f]),
                        Err(_) => Ok(()),
                    },
                    Some(Cmd::Consumed(stream, n)) => match mux.consumed(stream, n) {
                        Some(f) => send(&out, vec![f]),
                        None => Ok(()),
                    },
                };
                if let Err(e) = r {
                    break e;
                }
            }
            msg = ws.next() => {
                let bytes = match msg {
                    None | Some(Ok(Message::Close(_))) => break "eof".to_owned(),
                    Some(Err(e)) => break e.to_string(),
                    Some(Ok(Message::Binary(b))) => b,
                    Some(Ok(Message::Ping(_) | Message::Pong(_) | Message::Frame(_))) => continue,
                    Some(Ok(Message::Text(_))) => break "protocol_error: text on the data channel".to_owned(),
                };
                let event = match MuxFrame::decode(&bytes).and_then(|f| mux.receive(f)) {
                    Ok(e) => e,
                    Err(e) => break format!("protocol_error: {e}"),
                };
                let ev = match event {
                    None => None,
                    Some(MuxEvent::Credit { stream }) => {
                        if let Err(e) = flush(&out, &mut mux, &mut pending, stream) {
                            break e;
                        }
                        None
                    }
                    Some(MuxEvent::Opened { stream, purpose }) => Some(DataEvent::Opened { stream, purpose }),
                    Some(MuxEvent::Data { stream, bytes }) => Some(DataEvent::Data { stream, bytes }),
                    Some(MuxEvent::Closed { stream }) => Some(DataEvent::Closed { stream }),
                    Some(MuxEvent::Reset { stream, code }) => Some(DataEvent::Reset { stream, code }),
                };
                if let Some(ev) = ev {
                    let _ = events.send(ev);
                }
            }
        }
    };
    // The writer sends what is queued, then the close; a peer that reads
    // nothing cannot hold this task longer than the cleanup limit.
    drop(out);
    if tokio::time::timeout(super::handshake::CLEANUP_LIMIT, &mut writer)
        .await
        .is_err()
    {
        writer.abort();
    }
    drop(ws);
    // Pending writes fail now instead of waiting for credit that never comes.
    for (_, queue) in std::mem::take(&mut pending) {
        for (_, _, reply) in queue {
            let _ = reply.send(Err(LinkError::closed(&reason, "the data channel ended")));
        }
    }
    if !by_control {
        let _ = ties
            .control
            .try_send((super::control::DATA_CHANNEL_LOST.to_owned(), reason.clone()));
    }
    let _ = events.send(DataEvent::Ended { reason });
}
