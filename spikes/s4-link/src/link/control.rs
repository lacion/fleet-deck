//! The control channel task: requests in both directions, events, acks,
//! snapshots, ping/pong, and the `Close` frame (PLAN §8.3).
//!
//! Inbound requests are authorized here (method table, negotiated features,
//! the scopes granted to this peer now) before the application sees them, so
//! an unknown method or a missing scope is answered without application code.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::tungstenite::Message;

use super::handshake::CLEANUP_LIMIT;
use super::{Grants, LinkError, Ws};
use crate::frames::{ControlFrame, ErrorBody, Outcome, Scope, authorize};
use crate::hello::Negotiated;
use crate::identity::PeerKey;

/// What the application receives from the control channel.
#[derive(Debug, Clone, PartialEq)]
pub enum Inbound {
    /// An authorized request; answer it with [`Control::respond`].
    Request {
        id: u64,
        command_id: Option<String>,
        method: String,
        params: Value,
        scope: Scope,
    },
    /// Event, Ack or Snapshot.
    Frame(ControlFrame),
    /// The link ended; `reason` is a stable code (`superseded`, `eof`, ...).
    Closed { reason: String, detail: String },
}

enum Cmd {
    Request {
        method: String,
        command_id: Option<String>,
        params: Value,
        reply: oneshot::Sender<Outcome>,
    },
    Send(ControlFrame),
    Ping(oneshot::Sender<()>),
    Close {
        reason: String,
        detail: String,
    },
}

/// The handle of a control channel. Cheap to clone.
#[derive(Clone)]
pub struct Control {
    tx: mpsc::Sender<Cmd>,
}

impl Control {
    /// Sends a request and waits for its response.
    pub async fn request(
        &self,
        method: &str,
        command_id: Option<&str>,
        params: Value,
    ) -> Result<Outcome, LinkError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(Cmd::Request {
                method: method.to_owned(),
                command_id: command_id.map(str::to_owned),
                params,
                reply,
            })
            .await
            .map_err(|_| LinkError::gone())?;
        rx.await.map_err(|_| LinkError::gone())
    }

    /// Answers an inbound request.
    pub async fn respond(&self, id: u64, outcome: Outcome) -> Result<(), LinkError> {
        self.send(ControlFrame::Response { id, outcome }).await
    }

    /// Sends an Event, Ack or Snapshot frame.
    pub async fn send(&self, frame: ControlFrame) -> Result<(), LinkError> {
        self.tx
            .send(Cmd::Send(frame))
            .await
            .map_err(|_| LinkError::gone())
    }

    /// Round trip time of a `Ping`.
    pub async fn ping(&self) -> Result<Duration, LinkError> {
        let start = Instant::now();
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(Cmd::Ping(tx))
            .await
            .map_err(|_| LinkError::gone())?;
        rx.await.map_err(|_| LinkError::gone())?;
        Ok(start.elapsed())
    }

    /// Sends `Close` and ends the link.
    pub async fn close(&self, reason: &str, detail: &str) {
        let _ = self
            .tx
            .send(Cmd::Close {
                reason: reason.to_owned(),
                detail: detail.to_owned(),
            })
            .await;
    }

    /// Waits until the control task ended (the link is down).
    pub async fn closed(&self) {
        self.tx.closed().await;
    }

    /// True when the task ended.
    pub fn is_closed(&self) -> bool {
        self.tx.is_closed()
    }
}

pub(crate) struct Params {
    pub negotiated: Negotiated,
    pub grants: Arc<Grants>,
    pub peer: PeerKey,
    /// The link table (a superseded link) and the data channel (lost) end the
    /// control channel through this channel.
    pub shutdown: mpsc::Receiver<(String, String)>,
    /// Ends the data channel of the same link when the control channel ends.
    pub data_stop: Option<oneshot::Sender<(String, String)>>,
    pub on_exit: Box<dyn FnOnce() + Send + Sync>,
}

pub(crate) fn spawn(ws: Ws, p: Params) -> (Control, mpsc::Receiver<Inbound>) {
    let (tx, rx) = mpsc::channel(64);
    let (in_tx, in_rx) = mpsc::channel(256);
    tokio::spawn(run(ws, rx, in_tx, p));
    (Control { tx }, in_rx)
}

struct State {
    next_id: AtomicU64,
    pending: HashMap<u64, oneshot::Sender<Outcome>>,
    pings: HashMap<u64, oneshot::Sender<()>>,
}

async fn write(ws: &mut Ws, frame: &ControlFrame) -> Result<(), LinkError> {
    let text = frame
        .encode()
        .map_err(|e| LinkError::Protocol(e.to_string()))?;
    ws.send(Message::text(text)).await.map_err(LinkError::ws)
}

async fn run(
    mut ws: Ws,
    mut cmds: mpsc::Receiver<Cmd>,
    inbound: mpsc::Sender<Inbound>,
    mut p: Params,
) {
    let mut st = State {
        next_id: AtomicU64::new(1),
        pending: HashMap::new(),
        pings: HashMap::new(),
    };
    let mut shutdown_open = true;
    let (reason, detail) = loop {
        tokio::select! {
            cmd = cmds.recv() => match cmd {
                None => break ("closed".to_owned(), "the application dropped the link".to_owned()),
                Some(Cmd::Close { reason, detail }) => {
                    let _ = write(&mut ws, &ControlFrame::Close { reason: reason.clone(), detail: detail.clone() }).await;
                    break (reason, detail);
                }
                Some(cmd) => {
                    if let Err(e) = on_cmd(&mut ws, &mut st, cmd).await {
                        break ("io".to_owned(), e.to_string());
                    }
                }
            },
            msg = ws.next() => match on_message(&mut ws, &mut st, &inbound, &p, msg).await {
                Ok(()) => {}
                Err(end) => break end,
            },
            sd = p.shutdown.recv(), if shutdown_open => {
                // No sender left (no link table, no data channel): nothing can
                // end the link from outside any more.
                let Some((reason, detail)) = sd else {
                    shutdown_open = false;
                    continue;
                };
                // The peer closes the control channel first and the data channel
                // second, but the two TCP connections race: give a `close` frame
                // that is already on its way the chance to name the real reason.
                if reason == DATA_CHANNEL_LOST
                    && let Some(end) = peer_close_within(&mut ws, &mut st, &inbound, &p).await
                {
                    break end;
                }
                let close = ControlFrame::Close { reason: reason.clone(), detail: detail.clone() };
                let _ = tokio::time::timeout(CLEANUP_LIMIT, write(&mut ws, &close)).await;
                break (reason, detail);
            }
        }
    };
    let _ = tokio::time::timeout(CLEANUP_LIMIT, ws.close(None)).await;
    // The link is one unit: the data channel never outlives its control channel.
    if let Some(stop) = p.data_stop.take() {
        let _ = stop.send((reason.clone(), detail.clone()));
    }
    (p.on_exit)();
    let _ = inbound.send(Inbound::Closed { reason, detail }).await;
}

/// Reason that the data task reports when its channel ends on its own.
pub(crate) const DATA_CHANNEL_LOST: &str = "data_channel_lost";

/// How long the control task waits for a peer `close` frame after its data
/// channel was lost.
const CLOSE_GRACE: Duration = Duration::from_millis(500);

/// Processes control frames for at most [`CLOSE_GRACE`]; returns the end that
/// a peer `close` frame (or a channel end) names.
async fn peer_close_within(
    ws: &mut Ws,
    st: &mut State,
    inbound: &mpsc::Sender<Inbound>,
    p: &Params,
) -> Option<End> {
    let deadline = tokio::time::Instant::now() + CLOSE_GRACE;
    loop {
        let msg = tokio::time::timeout_at(deadline, ws.next()).await.ok()?;
        if let Err(end) = on_message(ws, st, inbound, p, msg).await {
            return Some(end);
        }
    }
}

async fn on_cmd(ws: &mut Ws, st: &mut State, cmd: Cmd) -> Result<(), LinkError> {
    match cmd {
        Cmd::Request {
            method,
            command_id,
            params,
            reply,
        } => {
            let id = st.next_id.fetch_add(1, Ordering::Relaxed);
            st.pending.insert(id, reply);
            write(
                ws,
                &ControlFrame::Request {
                    id,
                    command_id,
                    method,
                    params,
                },
            )
            .await
        }
        Cmd::Send(frame) => write(ws, &frame).await,
        Cmd::Ping(reply) => {
            let nonce = st.next_id.fetch_add(1, Ordering::Relaxed);
            st.pings.insert(nonce, reply);
            write(ws, &ControlFrame::Ping { nonce }).await
        }
        Cmd::Close { .. } => Ok(()),
    }
}

type End = (String, String);

fn protocol_end(detail: impl Into<String>) -> End {
    ("protocol_error".to_owned(), detail.into())
}

async fn on_message(
    ws: &mut Ws,
    st: &mut State,
    inbound: &mpsc::Sender<Inbound>,
    p: &Params,
    msg: Option<Result<Message, tokio_tungstenite::tungstenite::Error>>,
) -> Result<(), End> {
    let text = match msg {
        None | Some(Ok(Message::Close(_))) => {
            return Err(("eof".into(), "the peer closed the control channel".into()));
        }
        Some(Err(e)) => return Err(("io".into(), e.to_string())),
        Some(Ok(Message::Text(t))) => t,
        Some(Ok(Message::Ping(_) | Message::Pong(_) | Message::Frame(_))) => return Ok(()),
        Some(Ok(Message::Binary(_))) => {
            return Err(protocol_end("binary message on the control channel"));
        }
    };
    let frame = match ControlFrame::decode(text.as_str()) {
        Ok(f) => f,
        Err(e) => {
            let _ = write(
                ws,
                &ControlFrame::Close {
                    reason: "protocol_error".into(),
                    detail: e.to_string(),
                },
            )
            .await;
            return Err(protocol_end(e.to_string()));
        }
    };
    let deliver = |i: Inbound| async move {
        inbound
            .send(i)
            .await
            .map_err(|_| ("closed".to_owned(), "nobody reads the link".to_owned()))
    };
    match frame {
        ControlFrame::Request {
            id,
            command_id,
            method,
            params,
        } => {
            let granted = p.grants.of(&p.peer);
            match authorize(&method, command_id.as_deref(), &p.negotiated, &granted) {
                Ok(spec) => {
                    deliver(Inbound::Request {
                        id,
                        command_id,
                        method,
                        params,
                        scope: spec.scope,
                    })
                    .await
                }
                Err(err) => write(
                    ws,
                    &ControlFrame::Response {
                        id,
                        outcome: Outcome::Err(err),
                    },
                )
                .await
                .map_err(|e| ("io".to_owned(), e.to_string())),
            }
        }
        ControlFrame::Response { id, outcome } => match st.pending.remove(&id) {
            Some(reply) => {
                let _ = reply.send(outcome);
                Ok(())
            }
            None => Err(protocol_end(format!("response to unknown request {id}"))),
        },
        ControlFrame::Ping { nonce } => write(ws, &ControlFrame::Pong { nonce })
            .await
            .map_err(|e| ("io".to_owned(), e.to_string())),
        ControlFrame::Pong { nonce } => {
            if let Some(reply) = st.pings.remove(&nonce) {
                let _ = reply.send(());
            }
            Ok(())
        }
        ControlFrame::Close { reason, detail } => Err((reason, detail)),
        f @ (ControlFrame::Event { .. }
        | ControlFrame::Ack { .. }
        | ControlFrame::Snapshot { .. }) => deliver(Inbound::Frame(f)).await,
    }
}

/// The error body for a request that the application cannot serve.
pub fn not_implemented(method: &str) -> Outcome {
    Outcome::Err(ErrorBody::new(
        "not_implemented",
        format!("{method} is not implemented by this node"),
    ))
}
