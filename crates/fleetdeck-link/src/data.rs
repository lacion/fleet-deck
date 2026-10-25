//! The data channel task: [`Mux`] streams over binary WebSocket messages
//! (ADR-004 §2.7, §9.2; G2, G6, G9).
//!
//! A write waits until the peer granted enough credit; the application
//! gives credit back with [`Data::consumed`] after it used received bytes, so
//! a slow reader slows the sender instead of filling memory. The reader
//! never waits for the socket or the application: frames go to a writer
//! task, events go to an unbounded queue whose size the mux budgets bound
//! (64 streams, 4 MiB unconsumed bytes). A stream that the peer opens must
//! carry a valid single-use token ([`StreamTokens`]); otherwise the stream is
//! reset with `bad_stream_token` and the application sees nothing. A reset
//! fails the pending writes of its stream at once.

use std::collections::{BTreeMap, VecDeque};
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use futures_util::stream::SplitSink;
use futures_util::{SinkExt, StreamExt};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{mpsc, oneshot, watch};
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;

use crate::hello::Side;
use crate::live::DATA_LOSS_GRACE;
use crate::mux::{Mux, MuxError, MuxEvent, MuxFrame, StreamError};
use crate::table::Closing;
use crate::tokens::{BAD_STREAM_TOKEN, StreamToken, StreamTokens, TokenError};
use crate::ws::{CLEANUP_LIMIT, LinkError};

/// Commands that wait for the data task.
const COMMAND_QUEUE: usize = 64;

/// End reasons of the data channel itself.
pub mod reason {
    pub const EOF: &str = "eof";
    pub const IO: &str = "io";
    pub const TIMEOUT: &str = "timeout";
    pub const CLOSED: &str = "closed";
}

/// What the data channel needs from its link.
#[derive(Debug, Clone)]
pub struct DataParams {
    pub side: Side,
    pub attempt_id: String,
    pub tokens: StreamTokens,
    /// A single socket write that takes longer ends the channel.
    pub write_limit: Duration,
}

/// What the application receives from the data channel. The queue ends
/// (`None`) when the channel ended; [`Data::ended`] says why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DataEvent {
    /// The peer opened a stream with a valid token issued for `purpose`.
    Opened {
        stream: u32,
        purpose: String,
    },
    /// Received bytes; give them back with [`Data::consumed`] once used.
    Data {
        stream: u32,
        bytes: Vec<u8>,
    },
    /// The peer sends no more data on the stream.
    Closed {
        stream: u32,
    },
    Reset {
        stream: u32,
        reason: String,
    },
}

type Reply<T> = oneshot::Sender<Result<T, LinkError>>;

enum Cmd {
    Open(String, Reply<u32>),
    Write(u32, Vec<u8>, Reply<()>),
    Close(u32, Reply<()>),
    Reset(u32, String),
    Consumed(u32, usize),
    Fail(String),
}

struct Shared {
    params: DataParams,
    ended: watch::Sender<Option<Closing>>,
}

/// The handle of a data channel. Cheap to clone; dropping every handle does
/// not end the channel (the link ends it).
#[derive(Clone)]
pub struct Data {
    tx: mpsc::Sender<Cmd>,
    shared: Arc<Shared>,
}

impl fmt::Debug for Data {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Data")
            .field("side", &self.shared.params.side)
            .field("ended", &*self.shared.ended.borrow())
            .finish_non_exhaustive()
    }
}

fn closing(reason: &str, detail: impl Into<String>) -> Closing {
    Closing {
        reason: reason.to_owned(),
        detail: detail.into(),
    }
}

fn stream_error(e: &StreamError) -> LinkError {
    LinkError::Protocol(e.to_string())
}

impl Data {
    fn gone(&self) -> LinkError {
        let c = self
            .ended()
            .unwrap_or_else(|| closing(reason::CLOSED, "the data channel ended"));
        LinkError::closed(&c.reason, c.detail)
    }

    async fn call<T>(&self, cmd: impl FnOnce(Reply<T>) -> Cmd) -> Result<T, LinkError> {
        let (reply, rx) = oneshot::channel();
        self.tx.send(cmd(reply)).await.map_err(|_| self.gone())?;
        rx.await.map_err(|_| self.gone())?
    }

    async fn post(&self, cmd: Cmd) -> Result<(), LinkError> {
        self.tx.send(cmd).await.map_err(|_| self.gone())
    }

    /// Issues a token for one stream that the peer may open on this link.
    pub fn issue_token(&self, purpose: &str) -> Result<StreamToken, TokenError> {
        let p = &self.shared.params;
        p.tokens.issue(&p.attempt_id, purpose)
    }

    /// Opens a stream with a token that the peer issued.
    pub async fn open(&self, token: &str) -> Result<u32, LinkError> {
        let token = token.to_owned();
        self.call(|r| Cmd::Open(token, r)).await
    }

    /// Writes all of `bytes`; returns when the last chunk is queued for the
    /// socket. A reset of the stream or the end of the channel fails it.
    pub async fn write(&self, stream: u32, bytes: Vec<u8>) -> Result<(), LinkError> {
        self.call(|r| Cmd::Write(stream, bytes, r)).await
    }

    /// Ends the sending half of `stream` after its queued writes.
    pub async fn close(&self, stream: u32) -> Result<(), LinkError> {
        self.call(|r| Cmd::Close(stream, r)).await
    }

    /// Aborts both halves of `stream`; its pending writes fail.
    pub async fn reset(&self, stream: u32, reason: &str) -> Result<(), LinkError> {
        self.post(Cmd::Reset(stream, reason.to_owned())).await
    }

    /// Gives `n` consumed bytes of `stream` back to the peer.
    pub async fn consumed(&self, stream: u32, n: usize) -> Result<(), LinkError> {
        self.post(Cmd::Consumed(stream, n)).await
    }

    /// Why the channel ended; `None` while it runs.
    #[must_use]
    pub fn ended(&self) -> Option<Closing> {
        self.shared.ended.borrow().clone()
    }

    /// Waits until the channel ended.
    pub async fn closed(&self) -> Closing {
        let mut rx = self.shared.ended.subscribe();
        match rx.wait_for(Option::is_some).await {
            Ok(c) => c
                .clone()
                .unwrap_or_else(|| closing(reason::CLOSED, "the data channel ended")),
            Err(_) => closing(reason::CLOSED, "the data channel ended"),
        }
    }

    /// Test hook: ends the channel as a lost connection would, without the
    /// control channel.
    #[doc(hidden)]
    pub async fn fail_for_test(&self, detail: &str) {
        let _ = self.post(Cmd::Fail(detail.to_owned())).await;
    }
}

/// Starts the data channel. `stop` ends it with the given reason (the
/// control channel ended).
pub fn spawn<S>(
    ws: WebSocketStream<S>,
    params: DataParams,
    stop: watch::Receiver<Option<Closing>>,
) -> (Data, mpsc::UnboundedReceiver<DataEvent>)
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let (tx, rx) = mpsc::channel(COMMAND_QUEUE);
    let (ev_tx, ev_rx) = mpsc::unbounded_channel();
    let shared = Arc::new(Shared {
        params,
        ended: watch::channel(None).0,
    });
    tokio::spawn(run(ws, Arc::clone(&shared), rx, ev_tx, stop));
    (Data { tx, shared }, ev_rx)
}

type Writes = VecDeque<(Vec<u8>, usize, Reply<()>)>;

/// The state of the data task between events.
struct Task {
    mux: Mux,
    out: mpsc::UnboundedSender<Message>,
    pending: BTreeMap<u32, Writes>,
    /// Closes requested while writes of the stream were still pending.
    close_after: BTreeMap<u32, Reply<()>>,
    events: mpsc::UnboundedSender<DataEvent>,
    shared: Arc<Shared>,
}

type Step = Result<(), Closing>;

impl Task {
    fn send(&self, frames: impl IntoIterator<Item = MuxFrame>) -> Step {
        for f in frames {
            self.out
                .send(Message::binary(f.encode()))
                .map_err(|_| closing(reason::IO, "the data channel writer ended"))?;
        }
        Ok(())
    }

    fn deliver(&self, ev: DataEvent) {
        // An application that dropped its queue gets nothing; the link stays.
        let _ = self.events.send(ev);
    }

    /// Sends what the credit of `stream` allows from its queued writes.
    fn flush(&mut self, stream: u32) -> Step {
        let Some(queue) = self.pending.get_mut(&stream) else {
            return Ok(());
        };
        let mut frames = Vec::new();
        while let Some((buf, off, _)) = queue.front_mut() {
            match self.mux.send(stream, &buf[*off..]) {
                Ok((f, taken)) => {
                    frames.extend(f);
                    *off += taken;
                    if *off < buf.len() {
                        break; // wait for credit
                    }
                    if let Some((_, _, reply)) = queue.pop_front() {
                        let _ = reply.send(Ok(()));
                    }
                }
                Err(e) => {
                    if let Some((_, _, reply)) = queue.pop_front() {
                        let _ = reply.send(Err(stream_error(&e)));
                    }
                }
            }
        }
        if queue.is_empty() {
            self.pending.remove(&stream);
            if let Some(reply) = self.close_after.remove(&stream) {
                let r = self.mux.close(stream).map_err(|e| stream_error(&e));
                if let Ok(f) = &r {
                    frames.push(f.clone());
                }
                let _ = reply.send(r.map(|_| ()));
            }
        }
        self.send(frames)
    }

    /// Fails every pending write and close of `stream` at once (G9).
    fn fail_stream(&mut self, stream: u32, reason: &str) {
        let err = || LinkError::closed(reason, format!("stream {stream} was reset"));
        for (_, _, reply) in self.pending.remove(&stream).unwrap_or_default() {
            let _ = reply.send(Err(err()));
        }
        if let Some(reply) = self.close_after.remove(&stream) {
            let _ = reply.send(Err(err()));
        }
    }

    fn command(&mut self, cmd: Cmd) -> Step {
        match cmd {
            Cmd::Open(token, reply) => match self.mux.open(&token) {
                Ok((stream, frame)) => {
                    let r = self.send([frame]);
                    let _ = reply.send(Ok(stream));
                    r
                }
                Err(e) => {
                    let _ = reply.send(Err(stream_error(&e)));
                    Ok(())
                }
            },
            Cmd::Write(stream, bytes, reply) => {
                if !self.mux.is_open(stream) {
                    let _ = reply.send(Err(stream_error(&StreamError::NotOpen(stream))));
                    return Ok(());
                }
                self.pending
                    .entry(stream)
                    .or_default()
                    .push_back((bytes, 0, reply));
                self.flush(stream)
            }
            Cmd::Close(stream, reply) => {
                if self.pending.contains_key(&stream) {
                    self.close_after.insert(stream, reply);
                    return Ok(());
                }
                match self.mux.close(stream) {
                    Ok(f) => {
                        let _ = reply.send(Ok(()));
                        self.send([f])
                    }
                    Err(e) => {
                        let _ = reply.send(Err(stream_error(&e)));
                        Ok(())
                    }
                }
            }
            Cmd::Reset(stream, why) => {
                self.fail_stream(stream, &why);
                let frame = self.mux.reset(stream, &why);
                self.send(frame)
            }
            Cmd::Consumed(stream, n) => {
                let frame = self.mux.consumed(stream, n);
                self.send(frame)
            }
            Cmd::Fail(detail) => Err(closing(reason::IO, detail)),
        }
    }

    fn received(&mut self, bytes: &[u8]) -> Step {
        let violation = |e: MuxError| closing(e.reason(), e.to_string());
        let frame = MuxFrame::decode(bytes).map_err(violation)?;
        match self.mux.receive(frame).map_err(violation)? {
            None => Ok(()),
            Some(MuxEvent::Credit { stream }) => self.flush(stream),
            Some(MuxEvent::Refused { reset }) => self.send([reset]),
            Some(MuxEvent::Opened { stream, token }) => {
                let p = &self.shared.params;
                match p.tokens.redeem(&p.attempt_id, &token) {
                    Ok(purpose) => {
                        self.deliver(DataEvent::Opened { stream, purpose });
                        Ok(())
                    }
                    Err(refusal) => {
                        tracing::debug!(stream, ?refusal, "stream token refused");
                        let frame = self.mux.reset(stream, BAD_STREAM_TOKEN);
                        self.send(frame)
                    }
                }
            }
            Some(MuxEvent::Data { stream, bytes }) => {
                self.deliver(DataEvent::Data { stream, bytes });
                Ok(())
            }
            Some(MuxEvent::Closed { stream }) => {
                self.deliver(DataEvent::Closed { stream });
                Ok(())
            }
            Some(MuxEvent::Reset { stream, reason }) => {
                self.fail_stream(stream, &reason);
                self.deliver(DataEvent::Reset { stream, reason });
                Ok(())
            }
        }
    }

    fn message(&mut self, msg: Option<Result<Message, impl fmt::Display>>) -> Step {
        match msg {
            None | Some(Ok(Message::Close(_))) => {
                Err(closing(reason::EOF, "the peer closed the data channel"))
            }
            Some(Err(e)) => Err(closing(reason::IO, e.to_string())),
            Some(Ok(Message::Binary(b))) => self.received(&b),
            Some(Ok(Message::Text(_))) => Err(closing(
                crate::mux::reason::PROTOCOL_ERROR,
                "text message on the data channel",
            )),
            Some(Ok(_)) => Ok(()),
        }
    }
}

async fn run<S>(
    ws: WebSocketStream<S>,
    shared: Arc<Shared>,
    mut cmds: mpsc::Receiver<Cmd>,
    events: mpsc::UnboundedSender<DataEvent>,
    mut stop: watch::Receiver<Option<Closing>>,
) where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let (sink, mut stream) = ws.split();
    let (out, out_rx) = mpsc::unbounded_channel();
    let (final_tx, final_rx) = oneshot::channel();
    let limit = shared.params.write_limit;
    let mut writer = tokio::spawn(write_loop(sink, out_rx, final_rx, limit));
    let mut task = Task {
        mux: Mux::new(shared.params.side),
        out,
        pending: BTreeMap::new(),
        close_after: BTreeMap::new(),
        events,
        shared: Arc::clone(&shared),
    };
    let mut cmds_open = true;
    let mut writer_done = false;
    let mut end = loop {
        let step = tokio::select! {
            biased;
            s = stop.wait_for(Option::is_some) => match s {
                Ok(c) => Err(c.clone().unwrap_or_else(|| closing(reason::CLOSED, "the link ended"))),
                Err(_) => Err(closing(reason::CLOSED, "the link ended")),
            },
            w = &mut writer => {
                writer_done = true;
                Err(w.unwrap_or_else(|_| closing(reason::IO, "the data channel writer failed")))
            }
            msg = stream.next() => task.message(msg),
            cmd = cmds.recv(), if cmds_open => match cmd {
                Some(cmd) => task.command(cmd),
                None => {
                    cmds_open = false;
                    Ok(())
                }
            },
        };
        if let Err(c) = step {
            break c;
        }
    };
    // The peer closed the socket: its control `close` frame may still be on
    // the way. Wait briefly for it so that pending writes fail with the
    // link's reason, not with `eof` (the connections end in either order).
    if end.reason == reason::EOF
        && let Ok(Ok(c)) =
            tokio::time::timeout(DATA_LOSS_GRACE, stop.wait_for(Option::is_some)).await
        && let Some(c) = c.clone()
    {
        end = c;
    }
    // Bounded cleanup: the writer sends a WebSocket close and stops; a peer
    // that reads nothing cannot hold it longer than CLEANUP_LIMIT.
    let _ = final_tx.send(());
    if !writer_done
        && tokio::time::timeout(CLEANUP_LIMIT, &mut writer)
            .await
            .is_err()
    {
        writer.abort();
    }
    drop(stream);
    for stream in task.pending.keys().copied().collect::<Vec<_>>() {
        task.fail_stream(stream, &end.reason);
    }
    for (_, reply) in std::mem::take(&mut task.close_after) {
        let _ = reply.send(Err(LinkError::closed(&end.reason, end.detail.clone())));
    }
    shared.ended.send_replace(Some(end));
}

async fn write_loop<S>(
    mut sink: SplitSink<WebSocketStream<S>, Message>,
    mut out: mpsc::UnboundedReceiver<Message>,
    mut final_rx: oneshot::Receiver<()>,
    limit: Duration,
) -> Closing
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    loop {
        tokio::select! {
            biased;
            _ = &mut final_rx => {
                let _ = sink.close().await;
                return closing(reason::CLOSED, "the data channel closed");
            }
            next = out.recv() => {
                let Some(m) = next else {
                    return closing(reason::CLOSED, "no sender left");
                };
                match tokio::time::timeout(limit, sink.send(m)).await {
                    Ok(Ok(())) => {}
                    Ok(Err(e)) => return closing(reason::IO, e.to_string()),
                    Err(_) => return closing(
                        reason::TIMEOUT,
                        format!("a data frame write took longer than {limit:?}"),
                    ),
                }
            }
        }
    }
}

#[cfg(test)]
#[path = "data_tests.rs"]
mod tests;
