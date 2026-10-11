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

pub(crate) fn spawn(ws: Ws, side: Side) -> (Data, mpsc::UnboundedReceiver<DataEvent>) {
    let (tx, rx) = mpsc::channel(64);
    // Unbounded is safe: the peer cannot send more than the credit window
    // of each open stream.
    let (ev_tx, ev_rx) = mpsc::unbounded_channel();
    tokio::spawn(run(ws, Mux::new(side), rx, ev_tx));
    (Data { tx }, ev_rx)
}

async fn send(ws: &mut Ws, frames: Vec<MuxFrame>) -> Result<(), String> {
    for f in frames {
        ws.send(Message::binary(f.encode()))
            .await
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

async fn flush(
    ws: &mut Ws,
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
        send(ws, frames).await?;
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
    mut ws: Ws,
    mut mux: Mux,
    mut cmds: mpsc::Receiver<Cmd>,
    events: mpsc::UnboundedSender<DataEvent>,
) {
    let mut pending: BTreeMap<u32, Pending> = BTreeMap::new();
    let reason = loop {
        tokio::select! {
            cmd = cmds.recv() => {
                let r = match cmd {
                    None => break "closed".to_owned(),
                    Some(Cmd::Open(purpose, reply)) => match mux.open(&purpose) {
                        Ok((stream, frame)) => {
                            let r = send(&mut ws, vec![frame]).await;
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
                        flush(&mut ws, &mut mux, &mut pending, stream).await
                    }
                    Some(Cmd::Close(stream)) => match mux.close(stream) {
                        Ok(f) => send(&mut ws, vec![f]).await,
                        Err(_) => Ok(()),
                    },
                    Some(Cmd::Consumed(stream, n)) => match mux.consumed(stream, n) {
                        Some(f) => send(&mut ws, vec![f]).await,
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
                let out = match event {
                    None => None,
                    Some(MuxEvent::Credit { stream }) => {
                        if let Err(e) = flush(&mut ws, &mut mux, &mut pending, stream).await {
                            break e;
                        }
                        None
                    }
                    Some(MuxEvent::Opened { stream, purpose }) => Some(DataEvent::Opened { stream, purpose }),
                    Some(MuxEvent::Data { stream, bytes }) => Some(DataEvent::Data { stream, bytes }),
                    Some(MuxEvent::Closed { stream }) => Some(DataEvent::Closed { stream }),
                    Some(MuxEvent::Reset { stream, code }) => Some(DataEvent::Reset { stream, code }),
                };
                if let Some(ev) = out {
                    let _ = events.send(ev);
                }
            }
        }
    };
    let _ = ws.close(None).await;
    let _ = events.send(DataEvent::Ended { reason });
}
