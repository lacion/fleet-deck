//! The control channel of a link (ADR-004 §2.6, §9.4, §9.6, GATE-M1 G6–G8).
//!
//! Three tasks serve one channel:
//! - the **reader** decodes frames, checks their direction, feature and
//!   method (with the scopes that the pairing granted *now*), answers refused
//!   requests itself and delivers the rest to the application;
//! - the **writer** owns the socket sink and writes each frame within
//!   `write_limit`;
//! - the **supervisor** waits for the end of the link (local close, the link
//!   table, a task error, `idle_limit` without a frame) and sends the
//!   heartbeat ping. It never waits for queue capacity or for the socket, so
//!   a full application queue or a blackholed peer cannot block shutdown or
//!   supersession (G8). The final `close` frame gets [`CLEANUP_LIMIT`].

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use fleetdeck_core::identity::Fingerprint;
use fleetdeck_proto::CommandId;
use fleetdeck_proto::link::{ControlFrame, MAX_CONTROL_FRAME_BYTES, Outcome, WireError};
use fleetdeck_proto::version::Negotiated;
use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;

pub use crate::grants::{GrantSource, Grants};
use crate::methods::{self, MethodSpec, code};
use crate::table::{Closing, CurrentCheck, TableGuard};
use crate::tls::Role;
use crate::ws::{CLEANUP_LIMIT, LinkError};

/// A heartbeat `ping` goes out at this interval (G8).
pub const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(15);
/// The link closes (`timeout`) when no frame arrived for this long (G8).
pub const IDLE_LIMIT: Duration = Duration::from_secs(45);
/// Requests open on one link, both directions together (G8, DL-044).
pub const MAX_OUTSTANDING: usize = 128;
/// Capacity of the outbound frame queue and of the application queue.
pub const QUEUE_CAPACITY: usize = 256;
/// A single socket write that takes longer ends the link.
pub const WRITE_LIMIT: Duration = Duration::from_secs(10);

/// Close reasons of the control channel itself.
pub mod reason {
    /// The peer closed the socket without a `close` frame.
    pub const EOF: &str = "eof";
    /// A socket error.
    pub const IO: &str = "io";
    /// The application closed the link, or dropped every handle.
    pub const CLOSED: &str = "closed";
}

/// Limits of one control channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ControlConfig {
    pub heartbeat: Duration,
    pub idle_limit: Duration,
    pub write_limit: Duration,
    pub max_outstanding: usize,
    pub queue: usize,
}

impl Default for ControlConfig {
    fn default() -> Self {
        Self {
            heartbeat: HEARTBEAT_INTERVAL,
            idle_limit: IDLE_LIMIT,
            write_limit: WRITE_LIMIT,
            max_outstanding: MAX_OUTSTANDING,
            queue: QUEUE_CAPACITY,
        }
    }
}

/// What the application receives from the control channel. When the
/// receiver returns `None`, the link ended: [`Control::ended`] names why.
#[derive(Debug, Clone, PartialEq)]
pub enum Inbound {
    /// An authorized request; answer it with [`Control::respond`].
    Request(InboundRequest),
    /// An `event`, `ack` or snapshot frame that passed the direction and
    /// feature checks (boxed: an event frame is large).
    Frame(Box<ControlFrame>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct InboundRequest {
    pub id: u64,
    pub command_id: Option<CommandId>,
    pub method: String,
    pub params: Value,
    pub spec: &'static MethodSpec,
}

/// The roles, features and grants that every frame of one channel is
/// checked against.
#[derive(Debug, Clone)]
pub struct ChannelParams {
    /// This node's role, bound at pairing.
    pub local: Role,
    /// The peer's role, bound at pairing (the role of its pinned key).
    pub peer_role: Role,
    pub peer: Fingerprint,
    pub negotiated: Negotiated,
    /// The `desktop_epoch` that this node sent in its `Hello` of this
    /// connection (a station; R13 §5G item 1).
    pub local_desktop_epoch: Option<String>,
    /// The `desktop_epoch` that the peer sent in its `Hello` of this
    /// connection. A deck puts it into the authority of a desktop action.
    pub peer_desktop_epoch: Option<String>,
    pub grants: Arc<dyn GrantSource>,
    pub config: ControlConfig,
}

#[derive(Default)]
struct Pending {
    next_id: u64,
    out: HashMap<u64, oneshot::Sender<Outcome>>,
    /// Inbound request ids that wait for the application's answer.
    open_in: HashSet<u64>,
}

impl Pending {
    /// Requests open on the link in both directions: own requests that wait
    /// for a response plus inbound requests that wait for this node's answer.
    /// ADR-004 §9.7 G8 bounds this sum, not each direction (DL-044).
    fn open(&self) -> usize {
        self.out.len() + self.open_in.len()
    }
}

struct Shared {
    params: ChannelParams,
    out: mpsc::Sender<String>,
    pending: Mutex<Pending>,
    stop: watch::Sender<Option<Closing>>,
    ended: watch::Sender<Option<Closing>>,
    last_rx: Mutex<Instant>,
}

/// See [`Control::watcher`].
pub(crate) struct ControlWatch(Arc<Shared>);

impl ControlWatch {
    pub(crate) fn close(&self, reason: &str, detail: &str) {
        self.0.close(reason, detail);
    }

    pub(crate) fn ended(&self) -> Option<Closing> {
        self.0.ended.borrow().clone()
    }

    pub(crate) async fn closed(&self) -> Closing {
        self.0.closed().await
    }
}

impl Shared {
    fn close(&self, reason: &str, detail: &str) {
        self.stop.send_if_modified(|s| {
            if s.is_some() {
                return false;
            }
            *s = Some(Closing {
                reason: reason.to_owned(),
                detail: detail.to_owned(),
            });
            true
        });
    }

    async fn closed(&self) -> Closing {
        let mut rx = self.ended.subscribe();
        match rx.wait_for(Option::is_some).await {
            Ok(c) => c.clone().unwrap_or_else(unknown_end),
            Err(_) => unknown_end(),
        }
    }

    fn pending(&self) -> std::sync::MutexGuard<'_, Pending> {
        self.pending.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn touch(&self) {
        *self.last_rx.lock().unwrap_or_else(PoisonError::into_inner) = Instant::now();
    }

    fn last_rx(&self) -> Instant {
        *self.last_rx.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn gone(&self) -> LinkError {
        match self.ended.borrow().clone() {
            Some(c) => LinkError::Closed {
                reason: c.reason,
                detail: c.detail,
            },
            None => LinkError::closed(reason::CLOSED, "the control channel ended"),
        }
    }

    async fn enqueue(&self, frame: &ControlFrame) -> Result<(), LinkError> {
        let text = encode(frame)?;
        self.out.send(text).await.map_err(|_| self.gone())
    }
}

/// Removes the pending entry of a request whose frame is not queued, when
/// `Control::request` fails or is dropped before `enqueue` returned.
struct Unqueued<'a> {
    shared: &'a Shared,
    id: u64,
    armed: bool,
}

impl Drop for Unqueued<'_> {
    fn drop(&mut self) {
        if self.armed {
            self.shared.pending().out.remove(&self.id);
        }
    }
}

fn encode(frame: &ControlFrame) -> Result<String, LinkError> {
    let text = serde_json::to_string(frame).map_err(|e| LinkError::Protocol(e.to_string()))?;
    if text.len() > MAX_CONTROL_FRAME_BYTES {
        return Err(LinkError::Protocol(format!(
            "a `{}` frame of {} bytes is above the limit of {MAX_CONTROL_FRAME_BYTES}",
            frame.type_name(),
            text.len()
        )));
    }
    Ok(text)
}

fn refusal(code: &str, message: impl Into<String>) -> Outcome {
    Outcome::Err(WireError::new(code, message))
}

/// The handle of a control channel. Cheap to clone; the channel ends when
/// [`Control::close`] is called, the link ends, or every handle is dropped.
#[derive(Clone)]
pub struct Control {
    shared: Arc<Shared>,
    _alive: Arc<watch::Sender<()>>,
    /// Whether the link is still the current link of its peer (NV-12).
    current: Option<CurrentCheck>,
}

impl fmt::Debug for Control {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Control")
            .field("local", &self.shared.params.local)
            .field("ended", &*self.shared.ended.borrow())
            .finish_non_exhaustive()
    }
}

impl Control {
    /// Sends a request and waits for its response. A request that this node
    /// may not send (method, role, feature, `command_id`, more than
    /// `max_outstanding` open on the link in both directions together) is
    /// answered locally and never sent.
    pub async fn request(
        &self,
        method: &str,
        command_id: Option<&CommandId>,
        params: Value,
    ) -> Result<Outcome, LinkError> {
        let p = &self.shared.params;
        let cid = command_id.map(CommandId::as_str);
        if let Err(e) = methods::authorize(method, p.local, p.peer_role, cid, &p.negotiated, None) {
            return Ok(Outcome::Err(e));
        }
        if self.shared.ended.borrow().is_some() {
            return Err(self.shared.gone());
        }
        let (reply, rx) = oneshot::channel();
        let id = {
            let mut pending = self.shared.pending();
            if pending.open() >= p.config.max_outstanding {
                return Ok(refusal(
                    code::TOO_MANY_REQUESTS,
                    format!("{} requests are already open on this link", pending.open()),
                ));
            }
            pending.next_id += 1;
            let id = pending.next_id;
            pending.out.insert(id, reply);
            id
        };
        let frame = ControlFrame::Request {
            id,
            command_id: command_id.cloned(),
            method: method.to_owned(),
            params,
        };
        // `Shared::enqueue` is cancel-safe: it encodes the frame before its
        // only await point, and that await is `mpsc::Sender::send`, which
        // takes the text only after it has a queue slot. A future that is
        // dropped while it waits for a slot has not queued the frame, so
        // the peer never answers this id. The guard removes the entry in
        // that case, and when `enqueue` fails. After `enqueue` returned `Ok`
        // the frame is queued: the entry stays until its reply arrives (a
        // reply to a dropped receiver is ignored by the reader) or the link
        // ends.
        let mut unqueued = Unqueued {
            shared: &self.shared,
            id,
            armed: true,
        };
        self.shared.enqueue(&frame).await?;
        unqueued.armed = false;
        rx.await.map_err(|_| self.shared.gone())
    }

    /// Answers an inbound request.
    pub async fn respond(&self, id: u64, outcome: Outcome) -> Result<(), LinkError> {
        if !self.shared.pending().open_in.remove(&id) {
            return Err(LinkError::Protocol(format!("no open inbound request {id}")));
        }
        self.shared
            .enqueue(&ControlFrame::Response { id, outcome })
            .await
    }

    /// Sends an `event`, `ack` or snapshot frame after the direction and
    /// feature checks.
    pub async fn send(&self, frame: ControlFrame) -> Result<(), LinkError> {
        match frame {
            ControlFrame::Event { .. }
            | ControlFrame::Ack { .. }
            | ControlFrame::SnapshotBegin { .. }
            | ControlFrame::SnapshotSession { .. }
            | ControlFrame::SnapshotEnd { .. } => {}
            other => {
                return Err(LinkError::Protocol(format!(
                    "`{}` frames are not sent with `send`",
                    other.type_name()
                )));
            }
        }
        let p = &self.shared.params;
        methods::check_frame(&frame, p.local, &p.negotiated)
            .map_err(|e| LinkError::Protocol(e.to_string()))?;
        self.shared.enqueue(&frame).await
    }

    /// Ends the link with a `close` frame. Never waits: the supervisor sends
    /// the frame within [`CLEANUP_LIMIT`].
    pub fn close(&self, reason: &str, detail: &str) {
        self.shared.close(reason, detail);
    }

    /// A handle that sees and ends the link but does not keep it alive.
    pub(crate) fn watcher(&self) -> ControlWatch {
        ControlWatch(Arc::clone(&self.shared))
    }

    /// Why the link ended; `None` while it runs.
    #[must_use]
    pub fn ended(&self) -> Option<Closing> {
        self.shared.ended.borrow().clone()
    }

    /// Whether this link is still the current link of its peer. A later
    /// winner makes it `false` (NV-12). A control without a link table entry
    /// is always current.
    #[must_use]
    pub fn is_current(&self) -> bool {
        self.current.as_ref().is_none_or(CurrentCheck::is_current)
    }

    /// Waits until the link ended.
    pub async fn closed(&self) -> Closing {
        self.shared.closed().await
    }

    /// Own requests that wait for a response.
    #[must_use]
    pub fn outstanding(&self) -> usize {
        self.shared.pending().out.len()
    }

    /// How long ago the last message came from the peer (a frame, a
    /// WebSocket ping or pong). After the end of the link it keeps growing
    /// from the last message.
    #[must_use]
    pub fn since_last_frame(&self) -> Duration {
        self.shared.last_rx().elapsed()
    }

    /// Inbound requests that wait for this node's answer.
    #[must_use]
    pub fn open_inbound(&self) -> usize {
        self.shared.pending().open_in.len()
    }

    /// Requests open on the link in both directions, as this node sees them.
    /// It never exceeds `max_outstanding`.
    #[must_use]
    pub fn open_requests(&self) -> usize {
        self.shared.pending().open()
    }

    #[must_use]
    pub fn params(&self) -> &ChannelParams {
        &self.shared.params
    }
}

fn unknown_end() -> Closing {
    Closing {
        reason: reason::CLOSED.to_owned(),
        detail: "the control channel ended".to_owned(),
    }
}

fn end(reason: &str, detail: impl Into<String>) -> Closing {
    Closing {
        reason: reason.to_owned(),
        detail: detail.into(),
    }
}

/// Starts the control channel on `ws`. `closing` is the link table's signal
/// (superseded, revoked); `guard` is dropped when the channel ended.
pub fn spawn<S>(
    ws: WebSocketStream<S>,
    params: ChannelParams,
    closing: watch::Receiver<Option<Closing>>,
    guard: Option<TableGuard>,
) -> (Control, mpsc::Receiver<Inbound>)
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let config = params.config;
    let (out_tx, out_rx) = mpsc::channel(config.queue);
    let (in_tx, in_rx) = mpsc::channel(config.queue);
    let (stop, stop_rx) = watch::channel(None);
    let (ended, _) = watch::channel(None);
    let (alive, alive_rx) = watch::channel(());
    let shared = Arc::new(Shared {
        params,
        out: out_tx,
        pending: Mutex::new(Pending::default()),
        stop,
        ended,
        last_rx: Mutex::new(Instant::now()),
    });
    // Taken before the guard moves into the supervisor.
    let current = guard.as_ref().map(TableGuard::check);
    let (sink, stream) = ws.split();
    let (final_tx, final_rx) = oneshot::channel();
    let writer = tokio::spawn(write_loop(sink, out_rx, final_rx, config.write_limit));
    let reader = tokio::spawn(read_loop(stream, Arc::clone(&shared), in_tx));
    let tasks = Tasks {
        reader,
        writer,
        writer_done: false,
        final_tx: Some(final_tx),
    };
    tokio::spawn(supervise(
        Arc::clone(&shared),
        tasks,
        stop_rx,
        closing,
        alive_rx,
        guard,
    ));
    let control = Control {
        shared,
        _alive: Arc::new(alive),
        current,
    };
    (control, in_rx)
}

struct Tasks {
    reader: JoinHandle<End>,
    writer: JoinHandle<Closing>,
    /// A finished `JoinHandle` must not be polled again.
    writer_done: bool,
    final_tx: Option<oneshot::Sender<String>>,
}

/// The end of the link and whether the peer already said goodbye.
struct End {
    closing: Closing,
    send_close: bool,
}

async fn supervise(
    shared: Arc<Shared>,
    mut tasks: Tasks,
    mut stop: watch::Receiver<Option<Closing>>,
    mut closing: watch::Receiver<Option<Closing>>,
    mut alive: watch::Receiver<()>,
    guard: Option<TableGuard>,
) {
    let config = shared.params.config;
    let mut ping = tokio::time::interval_at(Instant::now() + config.heartbeat, config.heartbeat);
    ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut nonce = 0u64;
    let mut table_open = true;
    let done = loop {
        let deadline = shared.last_rx() + config.idle_limit;
        tokio::select! {
            biased;
            r = &mut tasks.reader => {
                break r.unwrap_or_else(|e| End {
                    closing: end(reason::IO, e.to_string()),
                    send_close: true,
                });
            }
            w = &mut tasks.writer => {
                tasks.writer_done = true;
                let closing = w.unwrap_or_else(|e| end(reason::IO, e.to_string()));
                break End { closing, send_close: false };
            }
            r = stop.changed() => {
                if let (Ok(()), Some(c)) = (r, stop.borrow().clone()) {
                    break End { closing: c, send_close: true };
                }
            }
            r = closing.changed(), if table_open => match r {
                Ok(()) => if let Some(c) = closing.borrow().clone() {
                    break End { closing: c, send_close: true };
                },
                Err(_) => table_open = false,
            },
            r = alive.changed() => if r.is_err() {
                break End {
                    closing: end(reason::CLOSED, "the application dropped every handle"),
                    send_close: true,
                };
            },
            () = tokio::time::sleep_until(deadline) => {
                if shared.last_rx() + config.idle_limit <= Instant::now() {
                    break End {
                        closing: end(code::TIMEOUT, format!(
                            "no frame from the peer for {:?}", config.idle_limit
                        )),
                        send_close: true,
                    };
                }
            }
            _ = ping.tick() => {
                nonce += 1;
                // Never waits: a full queue skips the ping, the idle limit
                // still applies.
                if let Ok(text) = encode(&ControlFrame::Ping { nonce }) {
                    let _ = shared.out.try_send(text);
                }
            }
        }
    };
    finish(&shared, tasks, done).await;
    drop(guard);
}

async fn finish(shared: &Shared, mut tasks: Tasks, done: End) {
    tasks.reader.abort();
    if let Some(final_tx) = tasks.final_tx.take() {
        let frame = ControlFrame::Close {
            reason: done.closing.reason.clone(),
            detail: done.closing.detail.clone(),
        };
        let text = if done.send_close {
            encode(&frame).unwrap_or_default()
        } else {
            String::new()
        };
        let _ = final_tx.send(text);
    }
    if !tasks.writer_done
        && tokio::time::timeout(CLEANUP_LIMIT, &mut tasks.writer)
            .await
            .is_err()
    {
        tasks.writer.abort();
    }
    shared.ended.send_replace(Some(done.closing));
    let mut pending = shared.pending();
    pending.out.clear();
    pending.open_in.clear();
}

type Sink<S> = SplitSink<WebSocketStream<S>, Message>;

async fn write_loop<S>(
    mut sink: Sink<S>,
    mut out: mpsc::Receiver<String>,
    mut final_rx: oneshot::Receiver<String>,
    limit: Duration,
) -> Closing
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    loop {
        tokio::select! {
            biased;
            last = &mut final_rx => {
                // The supervisor bounds this branch with CLEANUP_LIMIT.
                if let Ok(text) = last
                    && !text.is_empty()
                {
                    let _ = sink.send(Message::text(text)).await;
                }
                let _ = sink.close().await;
                return end(reason::CLOSED, "the control channel closed");
            }
            next = out.recv() => {
                let Some(text) = next else {
                    return end(reason::CLOSED, "no sender left");
                };
                match tokio::time::timeout(limit, sink.send(Message::text(text))).await {
                    Ok(Ok(())) => {}
                    Ok(Err(e)) => return end(reason::IO, e.to_string()),
                    Err(_) => return end(code::TIMEOUT, format!(
                        "a control frame write took longer than {limit:?}"
                    )),
                }
            }
        }
    }
}

async fn read_loop<S>(
    mut stream: SplitStream<WebSocketStream<S>>,
    shared: Arc<Shared>,
    inbound: mpsc::Sender<Inbound>,
) -> End
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    // The peer already left: no `close` frame back.
    let left = |closing| End {
        closing,
        send_close: false,
    };
    let refuse = |closing| End {
        closing,
        send_close: true,
    };
    loop {
        let msg = stream.next().await;
        // Only a message from the peer counts as heard; an EOF or a read
        // error is the end of the link, not a frame (B22).
        if matches!(msg, Some(Ok(_))) {
            shared.touch();
        }
        let text = match msg {
            None | Some(Ok(Message::Close(_))) => {
                return left(end(reason::EOF, "the peer closed the control channel"));
            }
            Some(Err(e)) => return left(end(reason::IO, e.to_string())),
            Some(Ok(Message::Text(t))) => t,
            Some(Ok(Message::Ping(_) | Message::Pong(_) | Message::Frame(_))) => continue,
            Some(Ok(Message::Binary(_))) => {
                return refuse(end(
                    code::PROTOCOL_ERROR,
                    "a binary message on the control channel",
                ));
            }
        };
        let frame: ControlFrame = match serde_json::from_str(text.as_str()) {
            Ok(f) => f,
            Err(e) => {
                return refuse(end(
                    code::PROTOCOL_ERROR,
                    format!("a bad control frame: {e}"),
                ));
            }
        };
        if let ControlFrame::Close { reason, detail } = frame {
            return left(Closing { reason, detail });
        }
        if let Err(stop) = on_frame(&shared, &inbound, frame).await {
            return refuse(stop);
        }
    }
}

async fn on_frame(
    shared: &Shared,
    inbound: &mpsc::Sender<Inbound>,
    frame: ControlFrame,
) -> Result<(), Closing> {
    let p = &shared.params;
    if let Err(e) = methods::check_frame(&frame, p.peer_role, &p.negotiated) {
        return Err(end(e.reason, e.detail));
    }
    let lost = |_| end(reason::CLOSED, "the application stopped reading the link");
    let io = |e: LinkError| end(reason::IO, e.to_string());
    match frame {
        ControlFrame::Request {
            id,
            command_id,
            method,
            params,
        } => {
            let refused = {
                let mut pending = shared.pending();
                if pending.open_in.contains(&id) {
                    return Err(end(
                        code::PROTOCOL_ERROR,
                        format!("request id {id} is already open"),
                    ));
                }
                let granted = p.grants.scopes(&p.peer);
                let cid = command_id.as_ref().map(CommandId::as_str);
                match methods::authorize(
                    &method,
                    p.peer_role,
                    p.local,
                    cid,
                    &p.negotiated,
                    Some(&granted),
                ) {
                    Err(e) => Err(Outcome::Err(crate::grants::explain(
                        &*p.grants, &p.peer, &method, &params, e,
                    ))),
                    Ok(_) if pending.open() >= p.config.max_outstanding => Err(refusal(
                        code::TOO_MANY_REQUESTS,
                        format!("{} requests are already open on this link", pending.open()),
                    )),
                    Ok(spec) => {
                        pending.open_in.insert(id);
                        Ok(spec)
                    }
                }
            };
            match refused {
                Err(outcome) => shared
                    .enqueue(&ControlFrame::Response { id, outcome })
                    .await
                    .map_err(io),
                Ok(spec) => inbound
                    .send(Inbound::Request(InboundRequest {
                        id,
                        command_id,
                        method,
                        params,
                        spec,
                    }))
                    .await
                    .map_err(lost),
            }
        }
        ControlFrame::Response { id, outcome } => match shared.pending().out.remove(&id) {
            Some(reply) => {
                let _ = reply.send(outcome);
                Ok(())
            }
            None => Err(end(
                code::PROTOCOL_ERROR,
                format!("a response to unknown request {id}"),
            )),
        },
        ControlFrame::Ping { nonce } => shared
            .enqueue(&ControlFrame::Pong { nonce })
            .await
            .map_err(io),
        ControlFrame::Pong { .. } | ControlFrame::Close { .. } => Ok(()),
        f @ (ControlFrame::Event { .. }
        | ControlFrame::Ack { .. }
        | ControlFrame::SnapshotBegin { .. }
        | ControlFrame::SnapshotSession { .. }
        | ControlFrame::SnapshotEnd { .. }) => inbound
            .send(Inbound::Frame(Box::new(f)))
            .await
            .map_err(lost),
    }
}

#[cfg(test)]
#[path = "control_tests.rs"]
mod tests;
