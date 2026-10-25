//! Data channel multiplexer (PLAN §8.1), sans-IO.
//!
//! The data channel carries many byte streams (artifact reads, terminal
//! output) in binary WebSocket messages. Each message is one frame:
//!
//! ```text
//! [kind: u8][stream: u32 big endian][payload]
//! kind 1 Open    payload = UTF-8 purpose (for example "artifact:<id>")
//! kind 2 Data    payload = at most 64 KiB of bytes
//! kind 3 Credit  payload = u32 big endian, more bytes the peer may send
//! kind 4 Close   payload = empty, the sender sends no more data
//! kind 5 Reset   payload = UTF-8 error code, the stream ends at once
//! ```
//!
//! Flow control is per stream and credit based: a sender may send at most the
//! credit it holds; each side starts with [`INITIAL_CREDIT`] per stream and
//! gives credit back when its application consumed the bytes. A peer that
//! sends more than its credit, uses an unknown stream, or opens a stream with
//! the wrong parity breaks the protocol, and the link closes.

use std::collections::BTreeMap;

use crate::hello::Side;

/// The largest payload of one `Data` frame.
pub const MAX_CHUNK: usize = 64 * 1024;
/// The receive window of a new stream, in bytes.
pub const INITIAL_CREDIT: u32 = 256 * 1024;
/// The largest number of streams that the peer may hold open at once.
pub const MAX_STREAMS: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MuxFrame {
    Open { stream: u32, purpose: String },
    Data { stream: u32, bytes: Vec<u8> },
    Credit { stream: u32, amount: u32 },
    Close { stream: u32 },
    Reset { stream: u32, code: String },
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MuxError {
    #[error("malformed data frame: {0}")]
    Malformed(&'static str),
    #[error("stream {0} is unknown")]
    UnknownStream(u32),
    #[error("stream {0} has the wrong parity or is already in use")]
    BadStreamId(u32),
    #[error("stream {stream}: {len} bytes exceed the credit {credit}")]
    FlowControl {
        stream: u32,
        len: usize,
        credit: u64,
    },
    #[error("stream {0}: data after close")]
    DataAfterClose(u32),
    #[error("more than {MAX_STREAMS} open streams")]
    TooManyStreams,
    #[error("the credit of stream {0} overflowed")]
    CreditOverflow(u32),
}

impl MuxFrame {
    pub fn stream(&self) -> u32 {
        match self {
            Self::Open { stream, .. }
            | Self::Data { stream, .. }
            | Self::Credit { stream, .. }
            | Self::Close { stream }
            | Self::Reset { stream, .. } => *stream,
        }
    }

    pub fn encode(&self) -> Vec<u8> {
        let (kind, payload): (u8, Vec<u8>) = match self {
            Self::Open { purpose, .. } => (1, purpose.as_bytes().to_vec()),
            Self::Data { bytes, .. } => (2, bytes.clone()),
            Self::Credit { amount, .. } => (3, amount.to_be_bytes().to_vec()),
            Self::Close { .. } => (4, Vec::new()),
            Self::Reset { code, .. } => (5, code.as_bytes().to_vec()),
        };
        let mut out = Vec::with_capacity(5 + payload.len());
        out.push(kind);
        out.extend_from_slice(&self.stream().to_be_bytes());
        out.extend_from_slice(&payload);
        out
    }

    pub fn decode(buf: &[u8]) -> Result<Self, MuxError> {
        if buf.len() < 5 {
            return Err(MuxError::Malformed("shorter than the 5 byte header"));
        }
        let stream = u32::from_be_bytes([buf[1], buf[2], buf[3], buf[4]]);
        let payload = &buf[5..];
        let text = || {
            String::from_utf8(payload.to_vec())
                .map_err(|_| MuxError::Malformed("text payload is not UTF-8"))
        };
        Ok(match buf[0] {
            1 => Self::Open {
                stream,
                purpose: text()?,
            },
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
                code: text()?,
            },
            _ => return Err(MuxError::Malformed("unknown frame kind")),
        })
    }
}

/// What the receiver tells its application.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MuxEvent {
    Opened {
        stream: u32,
        purpose: String,
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
        code: String,
    },
    /// The peer granted credit; blocked writes can continue.
    Credit {
        stream: u32,
    },
}

#[derive(Debug, Default)]
struct StreamState {
    /// Bytes this side may still send.
    send_credit: u64,
    /// Bytes the peer may still send.
    recv_window: u64,
    /// Received and consumed bytes that were not yet given back as credit.
    consumed_unacked: u64,
    local_closed: bool,
    remote_closed: bool,
}

/// One side of the multiplexer. The dialer opens odd stream ids, the
/// listener even ones, so both sides can open streams without a race.
#[derive(Debug)]
pub struct Mux {
    side: Side,
    next_id: u32,
    streams: BTreeMap<u32, StreamState>,
}

fn new_state() -> StreamState {
    StreamState {
        send_credit: u64::from(INITIAL_CREDIT),
        recv_window: u64::from(INITIAL_CREDIT),
        ..StreamState::default()
    }
}

impl Mux {
    pub fn new(side: Side) -> Self {
        Self {
            side,
            next_id: match side {
                Side::Dialer => 1,
                Side::Listener => 2,
            },
            streams: BTreeMap::new(),
        }
    }

    fn opened_by_peer(&self, stream: u32) -> bool {
        let odd = stream % 2 == 1;
        match self.side {
            Side::Dialer => !odd,
            Side::Listener => odd,
        }
    }

    /// Opens a stream; send the returned frame.
    pub fn open(&mut self, purpose: &str) -> Result<(u32, MuxFrame), MuxError> {
        if self.streams.len() >= MAX_STREAMS {
            return Err(MuxError::TooManyStreams);
        }
        let stream = self.next_id;
        self.next_id = self
            .next_id
            .checked_add(2)
            .ok_or(MuxError::TooManyStreams)?;
        self.streams.insert(stream, new_state());
        Ok((
            stream,
            MuxFrame::Open {
                stream,
                purpose: purpose.to_owned(),
            },
        ))
    }

    /// The bytes that this side may send now on `stream`.
    pub fn send_credit(&self, stream: u32) -> u64 {
        self.streams.get(&stream).map_or(0, |s| s.send_credit)
    }

    /// Takes as much of `data` as the credit allows, in chunks of at most
    /// [`MAX_CHUNK`]. Returns the frames and the number of bytes taken.
    pub fn send(&mut self, stream: u32, data: &[u8]) -> Result<(Vec<MuxFrame>, usize), MuxError> {
        let s = self
            .streams
            .get_mut(&stream)
            .ok_or(MuxError::UnknownStream(stream))?;
        if s.local_closed {
            return Err(MuxError::DataAfterClose(stream));
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
    pub fn close(&mut self, stream: u32) -> Result<MuxFrame, MuxError> {
        let s = self
            .streams
            .get_mut(&stream)
            .ok_or(MuxError::UnknownStream(stream))?;
        s.local_closed = true;
        let done = s.remote_closed;
        if done {
            self.streams.remove(&stream);
        }
        Ok(MuxFrame::Close { stream })
    }

    /// The application consumed `n` received bytes. Returns a `Credit` frame
    /// when at least half of the window was consumed.
    pub fn consumed(&mut self, stream: u32, n: usize) -> Option<MuxFrame> {
        let s = self.streams.get_mut(&stream)?;
        s.consumed_unacked += n as u64;
        if s.remote_closed || s.consumed_unacked < u64::from(INITIAL_CREDIT / 2) {
            return None;
        }
        let amount = u32::try_from(s.consumed_unacked).unwrap_or(u32::MAX);
        s.consumed_unacked -= u64::from(amount);
        s.recv_window += u64::from(amount);
        Some(MuxFrame::Credit { stream, amount })
    }

    /// Applies a frame from the peer.
    pub fn receive(&mut self, frame: MuxFrame) -> Result<Option<MuxEvent>, MuxError> {
        match frame {
            MuxFrame::Open { stream, purpose } => {
                if !self.opened_by_peer(stream) || self.streams.contains_key(&stream) {
                    return Err(MuxError::BadStreamId(stream));
                }
                if self.streams.len() >= MAX_STREAMS {
                    return Err(MuxError::TooManyStreams);
                }
                self.streams.insert(stream, new_state());
                Ok(Some(MuxEvent::Opened { stream, purpose }))
            }
            MuxFrame::Data { stream, bytes } => {
                let s = self
                    .streams
                    .get_mut(&stream)
                    .ok_or(MuxError::UnknownStream(stream))?;
                if s.remote_closed {
                    return Err(MuxError::DataAfterClose(stream));
                }
                if bytes.len() as u64 > s.recv_window {
                    return Err(MuxError::FlowControl {
                        stream,
                        len: bytes.len(),
                        credit: s.recv_window,
                    });
                }
                s.recv_window -= bytes.len() as u64;
                Ok(Some(MuxEvent::Data { stream, bytes }))
            }
            MuxFrame::Credit { stream, amount } => {
                // Credit for a stream that this side already finished is late, not an error.
                let Some(s) = self.streams.get_mut(&stream) else {
                    return Ok(None);
                };
                s.send_credit = s
                    .send_credit
                    .checked_add(u64::from(amount))
                    .filter(|c| *c <= u64::from(u32::MAX))
                    .ok_or(MuxError::CreditOverflow(stream))?;
                Ok(Some(MuxEvent::Credit { stream }))
            }
            MuxFrame::Close { stream } => {
                let s = self
                    .streams
                    .get_mut(&stream)
                    .ok_or(MuxError::UnknownStream(stream))?;
                s.remote_closed = true;
                if s.local_closed {
                    self.streams.remove(&stream);
                }
                Ok(Some(MuxEvent::Closed { stream }))
            }
            MuxFrame::Reset { stream, code } => {
                self.streams.remove(&stream);
                Ok(Some(MuxEvent::Reset { stream, code }))
            }
        }
    }

    pub fn open_streams(&self) -> usize {
        self.streams.len()
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    fn pump(to: &mut Mux, frames: Vec<MuxFrame>) -> Vec<MuxEvent> {
        frames
            .into_iter()
            .filter_map(|f| to.receive(MuxFrame::decode(&f.encode()).unwrap()).unwrap())
            .collect()
    }

    #[test]
    fn frames_round_trip() {
        for f in [
            MuxFrame::Open {
                stream: 1,
                purpose: "artifact:a".into(),
            },
            MuxFrame::Data {
                stream: 3,
                bytes: vec![1, 2, 3],
            },
            MuxFrame::Credit {
                stream: 2,
                amount: 7,
            },
            MuxFrame::Close { stream: 2 },
            MuxFrame::Reset {
                stream: 5,
                code: "cancelled".into(),
            },
        ] {
            assert_eq!(MuxFrame::decode(&f.encode()).unwrap(), f);
        }
        assert!(MuxFrame::decode(&[2, 0, 0, 0]).is_err());
        assert!(MuxFrame::decode(&[9, 0, 0, 0, 1]).is_err());
        let mut big = vec![2, 0, 0, 0, 1];
        big.extend(vec![0; MAX_CHUNK + 1]);
        assert!(MuxFrame::decode(&big).is_err());
    }

    #[test]
    fn a_sender_stops_at_the_credit_and_continues_after_a_grant() {
        let (mut d, mut l) = (Mux::new(Side::Dialer), Mux::new(Side::Listener));
        let (s, open) = d.open("artifact:x").unwrap();
        assert_eq!(s, 1);
        assert_eq!(
            pump(&mut l, vec![open]),
            vec![MuxEvent::Opened {
                stream: 1,
                purpose: "artifact:x".into()
            }]
        );
        let data = vec![7u8; 600 * 1024];
        let (frames, taken) = d.send(s, &data).unwrap();
        assert_eq!(taken, INITIAL_CREDIT as usize);
        assert!(
            frames
                .iter()
                .all(|f| matches!(f, MuxFrame::Data { bytes, .. } if bytes.len() <= MAX_CHUNK))
        );
        assert_eq!(pump(&mut l, frames).len(), 4);
        assert_eq!(
            d.send(s, &data[taken..]).unwrap().1,
            0,
            "no credit, nothing taken"
        );
        assert_eq!(
            l.consumed(s, 1000),
            None,
            "less than half a window gives no credit"
        );
        let credit = l.consumed(s, taken - 1000).unwrap();
        assert_eq!(
            credit,
            MuxFrame::Credit {
                stream: 1,
                amount: INITIAL_CREDIT
            }
        );
        d.receive(credit).unwrap();
        assert_eq!(d.send_credit(s), u64::from(INITIAL_CREDIT));
    }

    #[test]
    fn protocol_violations_are_errors() {
        let mut l = Mux::new(Side::Listener);
        assert_eq!(
            l.receive(MuxFrame::Open {
                stream: 2,
                purpose: String::new()
            }),
            Err(MuxError::BadStreamId(2)),
            "the dialer must use odd ids"
        );
        l.receive(MuxFrame::Open {
            stream: 1,
            purpose: String::new(),
        })
        .unwrap();
        assert_eq!(
            l.receive(MuxFrame::Open {
                stream: 1,
                purpose: String::new()
            }),
            Err(MuxError::BadStreamId(1))
        );
        assert_eq!(
            l.receive(MuxFrame::Data {
                stream: 9,
                bytes: vec![1]
            }),
            Err(MuxError::UnknownStream(9))
        );
        for _ in 0..4 {
            l.receive(MuxFrame::Data {
                stream: 1,
                bytes: vec![0; MAX_CHUNK],
            })
            .unwrap();
        }
        assert!(matches!(
            l.receive(MuxFrame::Data {
                stream: 1,
                bytes: vec![0]
            }),
            Err(MuxError::FlowControl { .. })
        ));
        l.receive(MuxFrame::Close { stream: 1 }).unwrap();
        assert_eq!(
            l.receive(MuxFrame::Data {
                stream: 1,
                bytes: vec![]
            }),
            Err(MuxError::DataAfterClose(1))
        );
        assert_eq!(
            l.receive(MuxFrame::Credit {
                stream: 1,
                amount: u32::MAX
            }),
            Err(MuxError::CreditOverflow(1))
        );
    }

    #[test]
    fn both_halves_must_close_before_the_stream_is_gone() {
        let (mut d, mut l) = (Mux::new(Side::Dialer), Mux::new(Side::Listener));
        let (s, open) = d.open("p").unwrap();
        l.receive(open).unwrap();
        l.receive(d.close(s).unwrap()).unwrap();
        assert_eq!(l.open_streams(), 1);
        d.receive(l.close(s).unwrap()).unwrap();
        assert_eq!((d.open_streams(), l.open_streams()), (0, 0));
    }

    proptest! {
        #[test]
        fn decode_never_panics(buf in proptest::collection::vec(any::<u8>(), 0..80)) {
            let _ = MuxFrame::decode(&buf);
        }
    }
}
