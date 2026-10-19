//! The station's side of a connection to a runner (`board-gaps.md` §2.9).
//! The runner's side is `client.rs`.
//!
//! The station checks the UID of the server, sends the `station_token` and
//! accepts the runner only when its answer holds the `runner_token`. No
//! input byte is sent before that check.

use std::io;
use std::time::Duration;

use fleetdeck_core::secret::ct_eq;
use tokio::net::UnixStream;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::time::timeout;

use super::files::{Spec, socket_path};
use super::frame::{Frame, read_frame, write_frame};

/// The longest time of the handshake.
const HELLO_WAIT: Duration = Duration::from_secs(5);

fn invalid(text: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, text)
}

/// The read half of a connection.
pub struct Reader(OwnedReadHalf);

impl Reader {
    /// The next frame of the runner.
    ///
    /// # Errors
    /// Any read error, and `InvalidData` for a frame that is not valid.
    pub async fn next(&mut self) -> io::Result<Frame> {
        read_frame(&mut self.0).await
    }
}

/// The write half of a connection.
pub struct Writer(OwnedWriteHalf);

impl Writer {
    /// Sends a new terminal size.
    ///
    /// # Errors
    /// Any write error.
    pub async fn resize(&mut self, cols: u16, rows: u16) -> io::Result<()> {
        write_frame(&mut self.0, &Frame::Resize { cols, rows }).await
    }

    /// Sends input bytes (frames of at most 64 KiB).
    ///
    /// # Errors
    /// Any write error.
    pub async fn input(&mut self, bytes: &[u8]) -> io::Result<()> {
        write_frame(&mut self.0, &Frame::Input(bytes.to_vec())).await
    }
}

/// A connection to a runner after the handshake.
pub struct Client {
    reader: Reader,
    writer: Writer,
}

impl Client {
    /// The next frame of the runner.
    ///
    /// # Errors
    /// See [`Reader::next`].
    pub async fn next(&mut self) -> io::Result<Frame> {
        self.reader.next().await
    }

    /// Sends a new terminal size.
    ///
    /// # Errors
    /// See [`Writer::resize`].
    pub async fn resize(&mut self, cols: u16, rows: u16) -> io::Result<()> {
        self.writer.resize(cols, rows).await
    }

    /// Sends input bytes.
    ///
    /// # Errors
    /// See [`Writer::input`].
    pub async fn input(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.writer.input(bytes).await
    }

    /// The two halves, so that one task reads while another writes.
    #[must_use]
    pub fn split(self) -> (Reader, Writer) {
        (self.reader, self.writer)
    }
}

async fn hello(stream: &mut UnixStream, spec: &Spec) -> io::Result<()> {
    let euid = rustix::process::geteuid().as_raw();
    if stream.peer_cred()?.uid() != euid {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "the runner socket belongs to another user",
        ));
    }
    write_frame(
        stream,
        &Frame::Hello(spec.station_token.as_bytes().to_vec()),
    )
    .await?;
    match read_frame(stream).await? {
        Frame::HelloOk(token) if ct_eq(&token, spec.runner_token.as_bytes()) => Ok(()),
        _ => Err(invalid("the runner did not answer with its token")),
    }
}

/// Connects to the socket of the run of `spec` in `dir` and does the
/// handshake.
///
/// # Errors
/// A connect error; `PermissionDenied` when the server runs as another user;
/// `InvalidData` when the answer is not `HelloOk` with the `runner_token`;
/// `TimedOut` after 5 s.
pub async fn connect(dir: &std::path::Path, spec: &Spec) -> io::Result<Client> {
    let mut stream = UnixStream::connect(socket_path(dir, &spec.maintenance_id)).await?;
    timeout(HELLO_WAIT, hello(&mut stream, spec))
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "the runner did not answer"))??;
    let (read, write) = stream.into_split();
    Ok(Client {
        reader: Reader(read),
        writer: Writer(write),
    })
}
