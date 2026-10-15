//! The station socket of the MCP bridge (DL-194 §2.2, §9.2, §9.3).
//!
//! A shim-hosted provider cannot reach the loopback HTTP endpoint through
//! a stable URL, so it runs `fleetdeck mcp-bridge`, which relays each
//! JSON-RPC message to this Unix socket. One connection carries one
//! message: line 1 is `AUTH <token>`, line 2 is the JSON-RPC message. The
//! answer is one JSON line, `ERR <code>`, or nothing (a notification). The
//! authentication, the limits and the JSON-RPC code are those of the HTTP
//! ingress (`ingress::authenticate_message`, `ingress::answer_message`).
//! The token is never logged.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use fleetdeck_core::localsock;
use fleetdeck_providers::host::shim::journal::{SUN_PATH_MAX, prepare_dir};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::task::{JoinHandle, JoinSet};

use super::bindings::{Binding, Bindings};
use super::ingress::{
    FleetTools, Forward, Limits, MessageOutcome, Shared, answer_message, authenticate_message,
};

/// The longest `AUTH <token>` line.
const AUTH_LINE_MAX: usize = 1024;
/// A connection that sent no complete message in this time is closed.
const IDLE: Duration = Duration::from_secs(30);
/// Connections served at once; above it a connection is closed at once.
const MAX_CONNECTIONS: usize = 64;

const ERR_BAD_REQUEST: &str = "ERR bad_request";
const ERR_UNAUTHORIZED: &str = "ERR unauthorized";
const ERR_LIMITED: &str = "ERR limited";

/// The socket path (DL-194 §9.2): the first candidate that fits `sun_path`
/// and whose directory is created or accepted as 0700, owned by this user
/// and not a link: `<runtime_dir>/fleetdeck/<incarnation>/mcp.s` (only
/// with a runtime directory), `<station_dir>/run/mcp/mcp.s`,
/// `<tmp_dir>/fd-<uid>-<incarnation>/mcp.s`. `None` when none fits
/// (`socket_path_too_long`).
#[must_use]
pub fn mcp_socket_path(
    runtime_dir: Option<&Path>,
    station_dir: &Path,
    tmp_dir: &Path,
    incarnation: &str,
) -> Option<PathBuf> {
    let uid = rustix::process::getuid().as_raw();
    let candidates = runtime_dir
        .map(|run| run.join("fleetdeck").join(incarnation))
        .into_iter()
        .chain([
            station_dir.join("run").join("mcp"),
            tmp_dir.join(format!("fd-{uid}-{incarnation}")),
        ]);
    for dir in candidates {
        let path = dir.join("mcp.s");
        if path.as_os_str().len() >= SUN_PATH_MAX {
            continue;
        }
        match prepare_dir(&dir) {
            Ok(()) => return Some(path),
            Err(error) => {
                tracing::warn!(dir = %dir.display(), %error, "bridge socket directory refused");
            }
        }
    }
    None
}

/// One line read with a bound.
pub(super) enum Line {
    /// The end of the stream before any byte.
    Eof,
    /// A line with its newline.
    Text(Vec<u8>),
    /// The stream ended inside a line.
    Partial(Vec<u8>),
    /// More than `limit` bytes without a newline; the rest is unread.
    TooLong,
}

/// Reads one line of at most `limit` bytes (without the newline).
pub(super) async fn read_bounded<R: AsyncBufRead + Unpin>(
    reader: &mut R,
    limit: usize,
) -> io::Result<Line> {
    let mut buf = Vec::new();
    let cap = u64::try_from(limit).unwrap_or(u64::MAX).saturating_add(1);
    let n = (&mut *reader).take(cap).read_until(b'\n', &mut buf).await?;
    if n == 0 {
        return Ok(Line::Eof);
    }
    if buf.last() == Some(&b'\n') {
        buf.pop();
        Ok(Line::Text(buf))
    } else if buf.len() > limit {
        Ok(Line::TooLong)
    } else {
        Ok(Line::Partial(buf))
    }
}

/// Reads and drops the rest of the current line.
pub(super) async fn discard_line<R: AsyncBufRead + Unpin>(reader: &mut R) -> io::Result<()> {
    loop {
        let buf = reader.fill_buf().await?;
        if buf.is_empty() {
            return Ok(());
        }
        if let Some(at) = buf.iter().position(|b| *b == b'\n') {
            reader.consume(at + 1);
            return Ok(());
        }
        let all = buf.len();
        reader.consume(all);
    }
}

/// The running bridge socket. Dropping it stops the accept loop and
/// removes the socket file.
#[derive(Debug)]
pub struct BridgeSocket {
    path: PathBuf,
    task: JoinHandle<()>,
}

impl BridgeSocket {
    /// Binds `path` (mode 0600; a stale file is replaced) and serves it.
    ///
    /// # Errors
    /// The bind failed, or another process serves `path`.
    pub async fn start(
        path: &Path,
        bindings: Bindings,
        forward: Arc<dyn Forward>,
        fleet: Option<Arc<dyn FleetTools>>,
        limits: Limits,
    ) -> io::Result<Self> {
        let listener = localsock::bind(path).await.map_err(io::Error::other)?;
        // The host check belongs to the HTTP path only.
        let shared = Arc::new(Shared::new(0, bindings, forward, fleet, limits));
        let task = tokio::spawn(serve(listener, shared));
        Ok(Self {
            path: path.to_owned(),
            task,
        })
    }

    /// Starts the socket of the bridge that `bindings` names, if any. A
    /// failed start clears the bridge, so no provider gets arguments for
    /// a socket that does not listen (DL-194).
    pub async fn start_for(
        bindings: &Bindings,
        forward: Arc<dyn Forward>,
        fleet: Option<Arc<dyn FleetTools>>,
    ) -> Option<Self> {
        let bridge = bindings.bridge()?;
        match Self::start(
            &bridge.socket,
            bindings.clone(),
            forward,
            fleet,
            Limits::default(),
        )
        .await
        {
            Ok(socket) => Some(socket),
            Err(error) => {
                tracing::warn!(socket = %bridge.socket.display(), %error, "the bridge socket did not start");
                bindings.clear_bridge();
                None
            }
        }
    }
}

impl Drop for BridgeSocket {
    fn drop(&mut self) {
        self.task.abort();
        if let Err(error) = std::fs::remove_file(&self.path)
            && error.kind() != io::ErrorKind::NotFound
        {
            tracing::warn!(%error, "the bridge socket file was not removed");
        }
    }
}

async fn serve(listener: UnixListener, shared: Arc<Shared>) {
    let conns = Arc::new(Semaphore::new(MAX_CONNECTIONS));
    let mut tasks = JoinSet::new();
    loop {
        tokio::select! {
            accepted = localsock::accept_own(&listener) => {
                let stream = match accepted {
                    Ok(Some(stream)) => stream,
                    Ok(None) => continue,
                    Err(error) => {
                        tracing::warn!(%error, "the bridge socket did not accept");
                        tokio::time::sleep(Duration::from_millis(20)).await;
                        continue;
                    }
                };
                // Above the limit: close at once, no waiting task.
                let Ok(permit) = conns.clone().try_acquire_owned() else {
                    drop(stream);
                    continue;
                };
                let shared = shared.clone();
                tasks.spawn(async move {
                    let _permit = permit;
                    connection(stream, &shared).await;
                });
            }
            Some(_) = tasks.join_next(), if !tasks.is_empty() => {}
        }
    }
}

/// What the first two lines of a connection led to.
enum Intake {
    Close,
    Refuse(&'static str),
    Ready(Box<Binding>, OwnedSemaphorePermit, Vec<u8>),
}

/// Reads `AUTH <token>` and the message, and authenticates. The
/// unauthenticated permit is held between the two lines, like the HTTP
/// path holds it while it reads the body.
async fn intake<R: AsyncBufRead + Unpin>(reader: &mut R, shared: &Shared) -> io::Result<Intake> {
    let first = match read_bounded(reader, AUTH_LINE_MAX).await? {
        Line::Text(text) => text,
        Line::Eof | Line::Partial(_) => return Ok(Intake::Close),
        Line::TooLong => return Ok(Intake::Refuse(ERR_BAD_REQUEST)),
    };
    let Some(token) = first
        .strip_prefix(b"AUTH ")
        .and_then(|rest| std::str::from_utf8(rest).ok())
        .filter(|rest| !rest.is_empty())
    else {
        return Ok(Intake::Refuse(ERR_BAD_REQUEST));
    };
    let header = format!("Bearer {token}");
    let Ok(unauth) = shared.unauth.clone().try_acquire_owned() else {
        return Ok(Intake::Refuse(ERR_LIMITED));
    };
    let body = match read_bounded(reader, shared.limits.body_bytes).await? {
        Line::Text(text) => text,
        Line::Eof | Line::Partial(_) => return Ok(Intake::Close),
        Line::TooLong => return Ok(Intake::Refuse(ERR_BAD_REQUEST)),
    };
    match authenticate_message(shared, Some(&header)) {
        Ok((binding, permit)) => {
            drop(unauth);
            Ok(Intake::Ready(binding, permit, body))
        }
        Err(MessageOutcome::Limited) => Ok(Intake::Refuse(ERR_LIMITED)),
        Err(_) => Ok(Intake::Refuse(ERR_UNAUTHORIZED)),
    }
}

async fn connection(stream: UnixStream, shared: &Shared) {
    let (read, mut write) = stream.into_split();
    let mut reader = BufReader::new(read);
    let reply = match tokio::time::timeout(IDLE, intake(&mut reader, shared)).await {
        Err(_) => None,
        Ok(Err(error)) => {
            tracing::debug!(kind = ?error.kind(), "a bridge connection failed");
            None
        }
        Ok(Ok(Intake::Close)) => None,
        Ok(Ok(Intake::Refuse(line))) => Some(line.to_owned()),
        Ok(Ok(Intake::Ready(binding, permit, body))) => {
            let outcome = answer_message(shared, &binding, &body).await;
            drop(permit);
            match outcome {
                MessageOutcome::Json(value) => Some(value.to_string()),
                MessageOutcome::Accepted => None,
                MessageOutcome::ParseError | MessageOutcome::Batch => {
                    Some(ERR_BAD_REQUEST.to_owned())
                }
                MessageOutcome::Unauthorized => Some(ERR_UNAUTHORIZED.to_owned()),
                MessageOutcome::Limited => Some(ERR_LIMITED.to_owned()),
            }
        }
    };
    if let Some(mut line) = reply {
        line.push('\n');
        let sent = async {
            write.write_all(line.as_bytes()).await?;
            write.flush().await
        };
        if let Err(error) = sent.await {
            tracing::debug!(kind = ?error.kind(), "a bridge reply was not sent");
        }
    }
}

#[cfg(test)]
#[path = "socket_tests.rs"]
pub(super) mod tests;
