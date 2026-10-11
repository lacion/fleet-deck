//! The station-local control socket (PLAN §6.5, §13 Phase 2; M2 P2-7).
//!
//! `fleetdeck station sessions|run|approve|interrupt` talk to the running
//! station over a Unix socket in the station's private data directory
//! (0700). The socket file is 0600, and the server accepts only peers with
//! the station's own user id. This is the emergency path that works
//! without the deck (PLAN §6.5): its commands are `CommandOrigin::Local`
//! with the caller `local`, and they go through the same decision and
//! receipt path as deck commands.
//!
//! Wire: one JSON object per line in each direction; one response per
//! request; a line is at most [`MAX_LINE_BYTES`].

use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use fleetdeck_proto::command::{
    ApprovalDecision, CommandEnvelope, CommandOrigin, RETRY_WINDOW_MS, Receipt, SessionCommand,
};
use fleetdeck_proto::{CommandId, RequestId, SessionSummary};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::watch;

use crate::manager::SessionManager;
use crate::store::Caller;

/// The longest request or response line.
pub const MAX_LINE_BYTES: usize = fleetdeck_proto::link::MAX_CONTROL_FRAME_BYTES;
/// The socket's file name in the station directory.
pub const SOCKET_NAME: &str = "station.sock";

/// The socket path of a station directory.
#[must_use]
pub fn socket_path(station_dir: &Path) -> PathBuf {
    station_dir.join(SOCKET_NAME)
}

/// One request of the local CLI.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum LocalRequest {
    /// The projection of every session.
    Sessions,
    /// One session command. Without `command_id` the station mints one.
    Command {
        command_id: Option<CommandId>,
        command: SessionCommand,
    },
    /// Answers an open request; the station finds its session.
    Approve {
        command_id: Option<CommandId>,
        request_id: RequestId,
        decision: ApprovalDecision,
    },
}

/// The answer to one [`LocalRequest`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum LocalResponse {
    Sessions {
        sessions: Vec<SessionSummary>,
    },
    /// The command's receipt (accepted or rejected).
    Receipt {
        receipt: Receipt,
    },
    /// The station could not decide the request (bad line, database error).
    Error {
        message: String,
    },
}

/// Why the local socket cannot start.
#[derive(Debug, thiserror::Error)]
pub enum LocalError {
    #[error("another station already serves `{0}`")]
    InUse(PathBuf),
    #[error("local socket `{path}`: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

/// Binds the socket. A stale socket file (no server answers) is replaced;
/// a live one is an error.
///
/// # Errors
/// [`LocalError`].
pub async fn bind(path: &Path) -> Result<UnixListener, LocalError> {
    let io_err = |source| LocalError::Io {
        path: path.to_owned(),
        source,
    };
    if path.exists() {
        if UnixStream::connect(path).await.is_ok() {
            return Err(LocalError::InUse(path.to_owned()));
        }
        std::fs::remove_file(path).map_err(io_err)?;
    }
    let listener = UnixListener::bind(path).map_err(io_err)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).map_err(io_err)?;
    Ok(listener)
}

/// Serves the local socket until `shutdown` turns true.
pub async fn serve(
    listener: UnixListener,
    manager: Arc<SessionManager>,
    mut shutdown: watch::Receiver<bool>,
) {
    let uid = rustix::process::getuid().as_raw();
    loop {
        tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) => {
                    let peer = stream.peer_cred().map(|c| c.uid()).ok();
                    if peer != Some(uid) {
                        tracing::warn!(?peer, "local socket: a peer of another user was refused");
                        continue;
                    }
                    tokio::spawn(connection(stream, Arc::clone(&manager)));
                }
                Err(e) => tracing::warn!(error = %e, "local socket accept"),
            },
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    return;
                }
            }
        }
    }
}

async fn connection(stream: UnixStream, manager: Arc<SessionManager>) {
    let (read, mut write) = stream.into_split();
    let mut reader = BufReader::new(read);
    loop {
        let line = match read_line(&mut reader).await {
            Ok(Some(line)) => line,
            Ok(None) => return,
            Err(e) => {
                let _ = send(&mut write, &error(e.to_string())).await;
                return;
            }
        };
        let response = match serde_json::from_slice::<LocalRequest>(&line) {
            Ok(request) => handle(&manager, request).await,
            Err(e) => error(format!("bad request: {e}")),
        };
        if send(&mut write, &response).await.is_err() {
            return;
        }
    }
}

fn error(message: String) -> LocalResponse {
    LocalResponse::Error { message }
}

async fn handle(manager: &SessionManager, request: LocalRequest) -> LocalResponse {
    let (command_id, command) = match request {
        LocalRequest::Sessions => {
            return match manager.store().snapshot().await {
                Ok(snapshot) => LocalResponse::Sessions {
                    sessions: snapshot.sessions,
                },
                Err(e) => error(e.to_string()),
            };
        }
        LocalRequest::Command {
            command_id,
            command,
        } => (command_id, command),
        LocalRequest::Approve {
            command_id,
            request_id,
            decision,
        } => match manager.store().runtime_request(request_id.clone()).await {
            Ok(Some(request)) => (
                command_id,
                SessionCommand::Answer {
                    session_id: request.session_id,
                    request_id,
                    decision,
                },
            ),
            Ok(None) => return error(format!("no request `{request_id}` on this station")),
            Err(e) => return error(e.to_string()),
        },
    };
    let command_id = command_id.unwrap_or_else(|| manager.new_id());
    let envelope = CommandEnvelope {
        command_id,
        target_incarnation: None,
        retry_until: manager.store().now_ms().saturating_add(RETRY_WINDOW_MS),
        origin: CommandOrigin::Local,
        command,
    };
    match manager.submit(envelope, Caller::local()).await {
        Ok(decided) => LocalResponse::Receipt {
            receipt: decided.receipt,
        },
        Err(e) => error(e.to_string()),
    }
}

/// Reads one line without its newline; `None` at a clean end.
async fn read_line<R: AsyncBufReadExt + Unpin>(reader: &mut R) -> io::Result<Option<Vec<u8>>> {
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

async fn send<W, T>(write: &mut W, value: &T) -> io::Result<()>
where
    W: AsyncWriteExt + Unpin,
    T: Serialize,
{
    let mut line = serde_json::to_vec(value).map_err(io::Error::other)?;
    line.push(b'\n');
    write.write_all(&line).await?;
    write.flush().await
}

/// The client side, for the CLI.
#[derive(Debug)]
pub struct LocalClient {
    reader: BufReader<tokio::net::unix::OwnedReadHalf>,
    writer: tokio::net::unix::OwnedWriteHalf,
}

impl LocalClient {
    /// Connects to the station's socket.
    ///
    /// # Errors
    /// No station serves `path`.
    pub async fn connect(path: &Path) -> io::Result<Self> {
        let stream = UnixStream::connect(path).await.map_err(|e| {
            io::Error::new(
                e.kind(),
                format!(
                    "no station answers on `{}` (is `fleetdeck station serve` running?): {e}",
                    path.display()
                ),
            )
        })?;
        let (read, writer) = stream.into_split();
        Ok(Self {
            reader: BufReader::new(read),
            writer,
        })
    }

    /// Sends one request and reads its response.
    ///
    /// # Errors
    /// The connection failed or the response is not valid.
    pub async fn call(&mut self, request: &LocalRequest) -> io::Result<LocalResponse> {
        send(&mut self.writer, request).await?;
        let line = read_line(&mut self.reader).await?.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "the station closed the socket",
            )
        })?;
        serde_json::from_slice(&line).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
    }
}

#[cfg(test)]
#[path = "local_tests.rs"]
mod tests;
