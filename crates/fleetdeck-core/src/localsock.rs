//! The owner-only local control socket of a node (PLAN §6.5; M2 P2-7,
//! P3-6b).
//!
//! The station (`station.sock`) and the deck (`deck.sock`) each serve a
//! Unix socket in their private data directory (0700). The socket file is
//! 0600, and the server accepts only peers with the node's own user id.
//! The local CLI commands are its clients.
//!
//! Wire: one JSON object per line in each direction; one response per
//! request; a line is at most [`MAX_LINE_BYTES`].

use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde::de::DeserializeOwned;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};

/// The longest request or response line.
pub const MAX_LINE_BYTES: usize = fleetdeck_proto::link::MAX_CONTROL_FRAME_BYTES;

/// Why the local socket cannot start.
#[derive(Debug, thiserror::Error)]
pub enum BindError {
    #[error("another process already serves `{0}`")]
    InUse(PathBuf),
    #[error("local socket `{path}`: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

/// Binds the socket with mode 0600. A stale socket file (no server
/// answers) is replaced; a live one is an error.
///
/// # Errors
/// [`BindError`].
pub async fn bind(path: &Path) -> Result<UnixListener, BindError> {
    let io_err = |source| BindError::Io {
        path: path.to_owned(),
        source,
    };
    if path.exists() {
        if UnixStream::connect(path).await.is_ok() {
            return Err(BindError::InUse(path.to_owned()));
        }
        std::fs::remove_file(path).map_err(io_err)?;
    }
    let listener = UnixListener::bind(path).map_err(io_err)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).map_err(io_err)?;
    Ok(listener)
}

/// Accepts the next connection of the node's own user. A peer of another
/// user is logged and dropped; then the result is `Ok(None)`.
///
/// # Errors
/// The accept failed.
pub async fn accept_own(listener: &UnixListener) -> io::Result<Option<UnixStream>> {
    let (stream, _) = listener.accept().await?;
    let uid = rustix::process::getuid().as_raw();
    let peer = stream.peer_cred().map(|c| c.uid()).ok();
    if peer == Some(uid) {
        Ok(Some(stream))
    } else {
        tracing::warn!(?peer, "local socket: a peer of another user was refused");
        Ok(None)
    }
}

/// Reads one line without its newline; `None` at a clean end.
///
/// # Errors
/// An I/O error, or a line longer than [`MAX_LINE_BYTES`].
pub async fn read_line<R: AsyncBufReadExt + Unpin>(reader: &mut R) -> io::Result<Option<Vec<u8>>> {
    let mut line = Vec::new();
    let limit = u64::try_from(MAX_LINE_BYTES).unwrap_or(u64::MAX) + 1;
    let n = (&mut *reader)
        .take(limit)
        .read_until(b'\n', &mut line)
        .await?;
    if n == 0 {
        return Ok(None);
    }
    if line.last() == Some(&b'\n') {
        line.pop();
    } else if line.len() > MAX_LINE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("a line is longer than {MAX_LINE_BYTES} bytes"),
        ));
    }
    Ok(Some(line))
}

/// Writes `value` as one JSON line.
///
/// # Errors
/// An I/O or serialization error.
pub async fn send<W, T>(write: &mut W, value: &T) -> io::Result<()>
where
    W: AsyncWriteExt + Unpin,
    T: Serialize,
{
    let mut line = serde_json::to_vec(value).map_err(io::Error::other)?;
    line.push(b'\n');
    write.write_all(&line).await?;
    write.flush().await
}

/// The message of a request line that does not parse. serde's message can
/// quote the input; it never echoes a line that may carry an invitation
/// (G12).
#[must_use]
pub fn bad_request(e: &serde_json::Error) -> String {
    format!("bad request at line {} column {}", e.line(), e.column())
}

/// An invitation on a local socket. `Debug` never shows it (G12); the
/// socket is 0600 and accepts only the node's own user.
#[derive(Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct InvitationText(pub String);

impl std::fmt::Debug for InvitationText {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("InvitationText(<redacted>)")
    }
}

/// The client side, for the CLI.
#[derive(Debug)]
pub struct LineClient {
    reader: BufReader<tokio::net::unix::OwnedReadHalf>,
    writer: tokio::net::unix::OwnedWriteHalf,
    node: &'static str,
    /// The credentials of the peer, read from the connected stream.
    peer: Result<(u32, Option<u32>), io::ErrorKind>,
}

impl LineClient {
    /// Connects to the socket of a `node` (`station` or `deck`).
    ///
    /// # Errors
    /// No process serves `path`.
    pub async fn connect(path: &Path, node: &'static str) -> io::Result<Self> {
        let stream = UnixStream::connect(path).await.map_err(|e| {
            io::Error::new(
                e.kind(),
                format!(
                    "no {node} answers on `{}` (is `fleetdeck {node} serve` running?): {e}",
                    path.display()
                ),
            )
        })?;
        let peer = stream
            .peer_cred()
            .map(|cred| {
                (
                    cred.uid(),
                    cred.pid().and_then(|pid| u32::try_from(pid).ok()),
                )
            })
            .map_err(|e| e.kind());
        let (read, writer) = stream.into_split();
        Ok(Self {
            reader: BufReader::new(read),
            writer,
            node,
            peer,
        })
    }

    /// The user id and the process id (when the system gives one) of the
    /// process on the other end of the socket.
    ///
    /// # Errors
    /// The system gave no credentials for the connection.
    pub fn peer_cred(&self) -> io::Result<(u32, Option<u32>)> {
        self.peer.map_err(io::Error::from)
    }

    /// Sends one request and reads its response.
    ///
    /// # Errors
    /// The connection failed or the response is not valid.
    pub async fn call<Q: Serialize, R: DeserializeOwned>(&mut self, request: &Q) -> io::Result<R> {
        send(&mut self.writer, request).await?;
        let line = read_line(&mut self.reader).await?.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::UnexpectedEof,
                format!("the {} closed the socket", self.node),
            )
        })?;
        serde_json::from_slice(&line).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[tokio::test]
    async fn the_client_reports_the_credentials_of_the_peer() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("peer.sock");
        let listener = bind(&path).await.unwrap();
        let accept = tokio::spawn(async move { listener.accept().await.map(|(s, _)| s) });
        let client = LineClient::connect(&path, "station").await.unwrap();
        let _server = accept.await.unwrap().unwrap();
        let (uid, pid) = client.peer_cred().unwrap();
        assert_eq!(uid, rustix::process::getuid().as_raw());
        assert_eq!(pid, Some(std::process::id()));
    }
}
