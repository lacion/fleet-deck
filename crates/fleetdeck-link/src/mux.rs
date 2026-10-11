//! Data channel multiplexer, sans-IO (ADR-004 §2.7 with the Revision 2
//! budgets of §9.2: G2, G6, G9).
//!
//! Each binary WebSocket message is one frame `[kind: u8][stream: u32 BE][payload]`:
//! 1 open (UTF-8 stream token), 2 data (1 to 64 KiB), 3 credit (`u32` BE),
//! 4 close (empty, half close), 5 reset (UTF-8 reason, both halves).
//!
//! Budgets per link: [`MAX_STREAMS`] open streams (both sides together),
//! [`WINDOW`] bytes of credit per stream and direction, [`MAX_BUFFERED`]
//! received bytes that the application did not consume yet (also after a
//! reset). A peer that breaks a budget ends the link with
//! `flow_control_violation`; any other malformed or impossible frame ends it
//! with `protocol_error`. Stream ids grow and are never reused within one
//! link attempt: the dialer opens odd ids, the listener even ids.

use std::collections::BTreeMap;

use crate::hello::Side;

/// The largest payload of one data frame.
pub const MAX_CHUNK: usize = 64 * 1024;
/// The credit window of each stream and direction.
pub const WINDOW: u32 = 256 * 1024;
/// Open streams on one link, both sides together.
pub const MAX_STREAMS: usize = 64;
/// Received bytes that the application did not consume yet, whole link.
pub const MAX_BUFFERED: u64 = 4 * 1024 * 1024;

/// Link close reasons of mux violations (ADR-004 §9.8).
pub mod reason {
    pub const FLOW_CONTROL_VIOLATION: &str = "flow_control_violation";
    pub const PROTOCOL_ERROR: &str = "protocol_error";
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MuxFrame {
    Open { stream: u32, token: String },
    Data { stream: u32, bytes: Vec<u8> },
    Credit { stream: u32, amount: u32 },
    Close { stream: u32 },
    Reset { stream: u32, reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MuxError {
    #[error("malformed data frame: {0}")]
    Malformed(&'static str),
    #[error("stream {0} was never opened")]
    UnknownStream(u32),
    #[error("stream id {0} has the wrong parity or is not new")]
    BadStreamId(u32),
    #[error("stream {stream}: {len} bytes exceed the credit {credit}")]
    Window {
        stream: u32,
        len: usize,
        credit: u64,
    },
    #[error("stream {0}: data after close")]
    DataAfterClose(u32),
    #[error("the peer opened more than {MAX_STREAMS} streams")]
    TooManyStreams,
    #[error("{0} unconsumed bytes exceed the {MAX_BUFFERED} byte budget")]
    Buffered(u64),
    /// A grant that would let this side send more than one window of bytes
    /// that the peer did not consume yet (ADR-004 §2.7, §9.2).
    #[error(
        "stream {stream}: a credit grant of {amount} bytes exceeds the {WINDOW} byte window ({credit} bytes of credit left)"
    )]
    CreditExceedsWindow {
        stream: u32,
        amount: u32,
        credit: u64,
    },
}

impl MuxError {
    /// The reason of the link close that this violation causes.
    #[must_use]
    pub fn reason(&self) -> &'static str {
        match self {
            Self::Window { .. }
            | Self::CreditExceedsWindow { .. }
            | Self::TooManyStreams
            | Self::Buffered(_) => reason::FLOW_CONTROL_VIOLATION,
            _ => reason::PROTOCOL_ERROR,
        }
    }
}

/// A local operation that the multiplexer refuses; the link stays up.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum StreamError {
    #[error("{MAX_STREAMS} streams are already open on this link")]
    TooManyStreams,
    #[error("stream {0} is not open")]
    NotOpen(u32),
    #[error("stream {0} is closed for writing")]
    Closed(u32),
}

impl MuxFrame {
    #[must_use]
    pub fn stream(&self) -> u32 {
        match self {
            Self::Open { stream, .. }
            | Self::Data { stream, .. }
            | Self::Credit { stream, .. }
            | Self::Close { stream }
            | Self::Reset { stream, .. } => *stream,
        }
    }

    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let credit;
        let (kind, payload): (u8, &[u8]) = match self {
            Self::Open { token, .. } => (1, token.as_bytes()),
            Self::Data { bytes, .. } => (2, bytes),
            Self::Credit { amount, .. } => {
                credit = amount.to_be_bytes();
                (3, &credit)
            }
            Self::Close { .. } => (4, &[]),
            Self::Reset { reason, .. } => (5, reason.as_bytes()),
        };
        let mut out = Vec::with_capacity(5 + payload.len());
        out.push(kind);
        out.extend_from_slice(&self.stream().to_be_bytes());
        out.extend_from_slice(payload);
        out
    }

    pub fn decode(buf: &[u8]) -> Result<Self, MuxError> {
        let (head, payload) = buf
            .split_first_chunk::<5>()
            .ok_or(MuxError::Malformed("shorter than the 5 byte header"))?;
        let [kind, s0, s1, s2, s3] = *head;
        let stream = u32::from_be_bytes([s0, s1, s2, s3]);
        let text = || {
            String::from_utf8(payload.to_vec())
                .map_err(|_| MuxError::Malformed("text payload is not UTF-8"))
        };
        Ok(match kind {
            1 => Self::Open {
                stream,
                token: text()?,
            },
            2 if payload.is_empty() => return Err(MuxError::Malformed("empty data frame")),
            2 if payload.len() > MAX_CHUNK => {
                return Err(MuxError::Malformed("data chunk larger than 64 KiB"));
            }
            2 => Self::Data {
                stream,
                bytes: payload.to_vec(),
            },
            3 => {
                let amount: [u8; 4] = payload
                    .try_into()
                    .map_err(|_| MuxError::Malformed("credit payload is not 4 bytes"))?;
                Self::Credit {
                    stream,
                    amount: u32::from_be_bytes(amount),
                }
            }
            4 if payload.is_empty() => Self::Close { stream },
            4 => return Err(MuxError::Malformed("close frame with a payload")),
            5 => Self::Reset {
                stream,
                reason: text()?,
            },
            _ => return Err(MuxError::Malformed("unknown frame kind")),
        })
    }
}

/// What a received frame means for the application.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MuxEvent {
    /// The peer opened a stream; redeem the token before it serves anything.
    Opened {
        stream: u32,
        token: String,
    },
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
    /// The peer granted credit; blocked writes can continue.
    Credit {
        stream: u32,
    },
    /// Both sides opened a stream at the same moment and the link is full:
    /// the peer's stream is refused; send `reset`.
    Refused {
        reset: MuxFrame,
    },
}

#[derive(Debug)]
struct StreamState {
    /// Bytes this side may still send.
    send_credit: u64,
    /// Bytes the peer may still send.
    recv_window: u64,
    /// Received bytes that the application did not consume yet.
    buffered: u64,
    /// Consumed bytes that were not given back as credit yet.
    consumed_unacked: u64,
    local_closed: bool,
    remote_closed: bool,
}

impl StreamState {
    fn new() -> Self {
        Self {
            send_credit: u64::from(WINDOW),
            recv_window: u64::from(WINDOW),
            buffered: 0,
            consumed_unacked: 0,
            local_closed: false,
            remote_closed: false,
        }
    }
}

/// One side of the multiplexer.
#[derive(Debug)]
pub struct Mux {
    side: Side,
    /// The next id this side opens.
    next_id: u32,
    /// The highest id the peer opened (0: none).
    peer_max: u32,
    /// Streams the peer opened and that are still open.
    peer_open: usize,
    streams: BTreeMap<u32, StreamState>,
    /// Unconsumed bytes of streams that ended.
    ended_buffered: BTreeMap<u32, u64>,
    buffered: u64,
}

impl Mux {
    #[must_use]
    pub fn new(side: Side) -> Self {
        Self {
            side,
            next_id: match side {
                Side::Dialer => 1,
                Side::Listener => 2,
            },
            peer_max: 0,
            peer_open: 0,
            streams: BTreeMap::new(),
            ended_buffered: BTreeMap::new(),
            buffered: 0,
        }
    }

    fn by_peer(&self, stream: u32) -> bool {
        (stream % 2 == 1) == matches!(self.side, Side::Listener)
    }

    /// An id that was opened once (by either side) and is not open now.
    fn finished(&self, stream: u32) -> bool {
        let opened = if self.by_peer(stream) {
            stream != 0 && stream <= self.peer_max
        } else {
            stream < self.next_id
        };
        opened && !self.streams.contains_key(&stream)
    }

    fn remove(&mut self, stream: u32) {
        if let Some(s) = self.streams.remove(&stream) {
            if self.by_peer(stream) {
                self.peer_open -= 1;
            }
            if s.buffered > 0 {
                self.ended_buffered.insert(stream, s.buffered);
            }
        }
    }

    /// Opens a stream with the peer's `token`; send the returned frame.
    pub fn open(&mut self, token: &str) -> Result<(u32, MuxFrame), StreamError> {
        if self.streams.len() >= MAX_STREAMS {
            return Err(StreamError::TooManyStreams);
        }
        let stream = self.next_id;
        self.next_id = stream.checked_add(2).ok_or(StreamError::TooManyStreams)?;
        self.streams.insert(stream, StreamState::new());
        let token = token.to_owned();
        Ok((stream, MuxFrame::Open { stream, token }))
    }

    /// Whether `stream` is open (at least one half).
    #[must_use]
    pub fn is_open(&self, stream: u32) -> bool {
        self.streams.contains_key(&stream)
    }

    /// The bytes this side may send now on `stream`.
    #[must_use]
    pub fn send_credit(&self, stream: u32) -> u64 {
        self.streams.get(&stream).map_or(0, |s| s.send_credit)
    }

    /// Takes as much of `data` as the credit allows, in chunks of at most
    /// [`MAX_CHUNK`]. Returns the frames and the number of bytes taken.
    pub fn send(
        &mut self,
        stream: u32,
        data: &[u8],
    ) -> Result<(Vec<MuxFrame>, usize), StreamError> {
        let s = self
            .streams
            .get_mut(&stream)
            .ok_or(StreamError::NotOpen(stream))?;
        if s.local_closed {
            return Err(StreamError::Closed(stream));
        }
        let take = data
            .len()
            .min(usize::try_from(s.send_credit).unwrap_or(usize::MAX));
        s.send_credit -= take as u64;
        let frames = data[..take]
            .chunks(MAX_CHUNK)
            .map(|c| MuxFrame::Data {
                stream,
                bytes: c.to_vec(),
            })
            .collect();
        Ok((frames, take))
    }

    /// Ends the sending half of `stream`.
    pub fn close(&mut self, stream: u32) -> Result<MuxFrame, StreamError> {
        let s = self
            .streams
            .get_mut(&stream)
            .ok_or(StreamError::NotOpen(stream))?;
        if s.local_closed {
            return Err(StreamError::Closed(stream));
        }
        s.local_closed = true;
        if s.remote_closed {
            self.remove(stream);
        }
        Ok(MuxFrame::Close { stream })
    }

    /// Aborts both halves of `stream`; `None` when it is not open.
    pub fn reset(&mut self, stream: u32, reason: &str) -> Option<MuxFrame> {
        self.streams.contains_key(&stream).then(|| {
            self.remove(stream);
            MuxFrame::Reset {
                stream,
                reason: reason.to_owned(),
            }
        })
    }

    /// The application consumed `n` received bytes of `stream` (also after
    /// the stream ended). Returns a credit frame when at least half of the
    /// window was consumed.
    pub fn consumed(&mut self, stream: u32, n: usize) -> Option<MuxFrame> {
        let n = n as u64;
        if let Some(left) = self.ended_buffered.get_mut(&stream) {
            let n = n.min(*left);
            *left -= n;
            self.buffered -= n;
            if *left == 0 {
                self.ended_buffered.remove(&stream);
            }
            return None;
        }
        let s = self.streams.get_mut(&stream)?;
        let n = n.min(s.buffered);
        s.buffered -= n;
        self.buffered -= n;
        s.consumed_unacked += n;
        if s.remote_closed || s.consumed_unacked < u64::from(WINDOW / 2) {
            return None;
        }
        let amount = u32::try_from(s.consumed_unacked).unwrap_or(u32::MAX);
        s.consumed_unacked -= u64::from(amount);
        s.recv_window += u64::from(amount);
        Some(MuxFrame::Credit { stream, amount })
    }

    /// Applies a frame from the peer. `Ok(None)`: nothing for the
    /// application (a late frame of a finished stream).
    pub fn receive(&mut self, frame: MuxFrame) -> Result<Option<MuxEvent>, MuxError> {
        let stream = frame.stream();
        if !matches!(frame, MuxFrame::Open { .. }) && !self.streams.contains_key(&stream) {
            // Frames in flight when this side reset or finished the stream,
            // and resets of streams this side never saw, are dropped.
            return if self.finished(stream) || matches!(frame, MuxFrame::Reset { .. }) {
                Ok(None)
            } else {
                Err(MuxError::UnknownStream(stream))
            };
        }
        match frame {
            MuxFrame::Open { stream, token } => self.peer_opened(stream, token),
            MuxFrame::Data { stream, bytes } => {
                let total = self.buffered;
                let s = self
                    .streams
                    .get_mut(&stream)
                    .ok_or(MuxError::UnknownStream(stream))?;
                if s.remote_closed {
                    return Err(MuxError::DataAfterClose(stream));
                }
                let len = bytes.len() as u64;
                if len > s.recv_window {
                    return Err(MuxError::Window {
                        stream,
                        len: bytes.len(),
                        credit: s.recv_window,
                    });
                }
                if total + len > MAX_BUFFERED {
                    return Err(MuxError::Buffered(total + len));
                }
                s.recv_window -= len;
                s.buffered += len;
                self.buffered += len;
                Ok(Some(MuxEvent::Data { stream, bytes }))
            }
            MuxFrame::Credit { stream, amount } => {
                let s = self
                    .streams
                    .get_mut(&stream)
                    .ok_or(MuxError::UnknownStream(stream))?;
                // A peer grants only bytes it consumed, so this side's credit
                // never exceeds one window: send_credit + in flight + the
                // peer's unconsumed bytes = WINDOW.
                let credit = s.send_credit + u64::from(amount);
                if credit > u64::from(WINDOW) {
                    return Err(MuxError::CreditExceedsWindow {
                        stream,
                        amount,
                        credit: s.send_credit,
                    });
                }
                s.send_credit = credit;
                Ok(Some(MuxEvent::Credit { stream }))
            }
            MuxFrame::Close { stream } => {
                let s = self
                    .streams
                    .get_mut(&stream)
                    .ok_or(MuxError::UnknownStream(stream))?;
                if s.remote_closed {
                    return Err(MuxError::DataAfterClose(stream));
                }
                s.remote_closed = true;
                if s.local_closed {
                    self.remove(stream);
                }
                Ok(Some(MuxEvent::Closed { stream }))
            }
            MuxFrame::Reset { stream, reason } => {
                self.remove(stream);
                Ok(Some(MuxEvent::Reset { stream, reason }))
            }
        }
    }

    fn peer_opened(&mut self, stream: u32, token: String) -> Result<Option<MuxEvent>, MuxError> {
        if !self.by_peer(stream) || stream <= self.peer_max {
            return Err(MuxError::BadStreamId(stream));
        }
        self.peer_max = stream;
        if self.peer_open >= MAX_STREAMS {
            return Err(MuxError::TooManyStreams);
        }
        if self.streams.len() >= MAX_STREAMS {
            // Our own open crossed the peer's on the wire; the peer counted
            // correctly, so its stream is refused, not the link.
            let reset = MuxFrame::Reset {
                stream,
                reason: reason::FLOW_CONTROL_VIOLATION.to_owned(),
            };
            return Ok(Some(MuxEvent::Refused { reset }));
        }
        self.streams.insert(stream, StreamState::new());
        self.peer_open += 1;
        Ok(Some(MuxEvent::Opened { stream, token }))
    }

    #[must_use]
    pub fn open_streams(&self) -> usize {
        self.streams.len()
    }

    /// Received bytes that the application did not consume yet.
    #[must_use]
    pub fn buffered(&self) -> u64 {
        self.buffered
    }
}

#[cfg(test)]
#[path = "mux_tests.rs"]
mod tests;
