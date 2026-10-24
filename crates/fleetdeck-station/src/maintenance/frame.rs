//! The frames of the runner socket (`board-gaps.md` §2.9).
//!
//! A frame is a 4-byte big-endian length `n` (1 to 65,537) and `n` bytes:
//! one type byte and the payload (at most 64 KiB).

use std::io;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// The most payload bytes of one frame.
pub const MAX_PAYLOAD: usize = 64 * 1024;

const HELLO: u8 = 1;
const HELLO_OK: u8 = 2;
const OUTPUT: u8 = 3;
const SNAPSHOT_END: u8 = 4;
const INPUT: u8 = 5;
const RESIZE: u8 = 6;
const EXIT: u8 = 7;

/// One frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Frame {
    /// Client to runner: the `station_token` (ASCII).
    Hello(Vec<u8>),
    /// Runner to client: the `runner_token` (ASCII).
    HelloOk(Vec<u8>),
    /// Runner to client: PTY bytes.
    Output(Vec<u8>),
    /// Runner to client: the snapshot of the ring is complete.
    SnapshotEnd,
    /// Client to runner: input bytes.
    Input(Vec<u8>),
    /// Client to runner: a new terminal size.
    Resize { cols: u16, rows: u16 },
    /// Runner to client: the exit code of the child.
    Exit(i32),
}

fn invalid(text: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, text)
}

/// Reads one frame.
///
/// # Errors
/// `InvalidData` for an unknown type, a length of 0 or over 65,537, or a
/// payload size that the type does not allow; any read error.
pub async fn read_frame<R: AsyncRead + Unpin>(r: &mut R) -> io::Result<Frame> {
    let mut head = [0_u8; 4];
    r.read_exact(&mut head).await?;
    let n = usize::try_from(u32::from_be_bytes(head)).map_err(|_| invalid("frame length"))?;
    if n == 0 || n > MAX_PAYLOAD + 1 {
        return Err(invalid("frame length"));
    }
    let mut body = vec![0_u8; n];
    r.read_exact(&mut body).await?;
    let kind = body[0];
    let payload = &body[1..];
    match kind {
        HELLO => Ok(Frame::Hello(payload.to_vec())),
        HELLO_OK => Ok(Frame::HelloOk(payload.to_vec())),
        OUTPUT => Ok(Frame::Output(payload.to_vec())),
        INPUT => Ok(Frame::Input(payload.to_vec())),
        SNAPSHOT_END if payload.is_empty() => Ok(Frame::SnapshotEnd),
        RESIZE => match payload {
            [c0, c1, r0, r1] => Ok(Frame::Resize {
                cols: u16::from_be_bytes([*c0, *c1]),
                rows: u16::from_be_bytes([*r0, *r1]),
            }),
            _ => Err(invalid("resize payload")),
        },
        EXIT => match payload {
            [a, b, c, d] => Ok(Frame::Exit(i32::from_be_bytes([*a, *b, *c, *d]))),
            _ => Err(invalid("exit payload")),
        },
        SNAPSHOT_END => Err(invalid("snapshot end payload")),
        _ => Err(invalid("frame type")),
    }
}

async fn put<W: AsyncWrite + Unpin>(w: &mut W, kind: u8, payload: &[u8]) -> io::Result<()> {
    let n = u32::try_from(payload.len() + 1).map_err(|_| invalid("frame length"))?;
    let mut out = Vec::with_capacity(payload.len() + 5);
    out.extend_from_slice(&n.to_be_bytes());
    out.push(kind);
    out.extend_from_slice(payload);
    w.write_all(&out).await
}

/// Writes one frame. An `Output` or `Input` payload over 64 KiB goes out in
/// several frames.
///
/// # Errors
/// `InvalidInput` for another payload over 64 KiB; any write error.
pub async fn write_frame<W: AsyncWrite + Unpin>(w: &mut W, frame: &Frame) -> io::Result<()> {
    match frame {
        Frame::Output(bytes) => put_split(w, OUTPUT, bytes).await?,
        Frame::Input(bytes) => put_split(w, INPUT, bytes).await?,
        Frame::Hello(bytes) => put_small(w, HELLO, bytes).await?,
        Frame::HelloOk(bytes) => put_small(w, HELLO_OK, bytes).await?,
        Frame::SnapshotEnd => put(w, SNAPSHOT_END, &[]).await?,
        Frame::Resize { cols, rows } => {
            let c = cols.to_be_bytes();
            let r = rows.to_be_bytes();
            put(w, RESIZE, &[c[0], c[1], r[0], r[1]]).await?;
        }
        Frame::Exit(code) => put(w, EXIT, &code.to_be_bytes()).await?,
    }
    w.flush().await
}

async fn put_split<W: AsyncWrite + Unpin>(w: &mut W, kind: u8, bytes: &[u8]) -> io::Result<()> {
    if bytes.is_empty() {
        return put(w, kind, &[]).await;
    }
    for chunk in bytes.chunks(MAX_PAYLOAD) {
        put(w, kind, chunk).await?;
    }
    Ok(())
}

async fn put_small<W: AsyncWrite + Unpin>(w: &mut W, kind: u8, bytes: &[u8]) -> io::Result<()> {
    if bytes.len() > MAX_PAYLOAD {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "frame payload is too long",
        ));
    }
    put(w, kind, bytes).await
}
