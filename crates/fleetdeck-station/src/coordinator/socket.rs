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
//!
//! The message goes to the `rmcp` service (`mcp_service.rs`) with the HTTP
//! headers that the station synthesizes for it. While the service runs, the
//! connection watches the socket: a full close by the peer drops the service
//! future, which cancels the call (MCP P1 §3.4).

use std::io;
use std::os::fd::{AsFd, OwnedFd};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use fleetdeck_core::localsock;
use fleetdeck_providers::host::shim::journal::{SUN_PATH_MAX, prepare_dir};
use serde_json::{Value, json};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::task::{JoinHandle, JoinSet};

use super::bindings::{Binding, Bindings};
use super::ingress::{
    FleetTools, Forward, Limits, MessageOutcome, Shared, answer_message, authenticate_message,
};
use super::mcp_service::bridge_headers;

/// The longest `AUTH <token>` line.
const AUTH_LINE_MAX: usize = 1024;
/// A connection that sent no complete message in this time is closed.
const IDLE: Duration = Duration::from_secs(30);
/// Connections served at once; above it a connection is closed at once.
const MAX_CONNECTIONS: usize = 64;

const ERR_BAD_REQUEST: &str = "ERR bad_request";
const ERR_UNAUTHORIZED: &str = "ERR unauthorized";
const ERR_LIMITED: &str = "ERR limited";
const ERR_STARTING: &str = "ERR starting";

/// The message of the correlated line for a call over the binding limit. The
/// same text as the bridge client uses for `ERR limited` (`bridge.rs`).
const LIMITED_TEXT: &str = "Too many calls; try again in a moment.";

/// How often the connection checks for a full close by the peer while a call
/// runs.
const HANGUP_POLL: Duration = Duration::from_millis(100);

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
    /// The binding is over its limit; the message line was read, so the
    /// refusal can carry its id.
    Limited(Vec<u8>),
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
        Err(MessageOutcome::Limited) => Ok(Intake::Limited(body)),
        Err(MessageOutcome::Starting) => Ok(Intake::Refuse(ERR_STARTING)),
        Err(_) => Ok(Intake::Refuse(ERR_UNAUTHORIZED)),
    }
}

/// The line for a message that the binding limit refused: a correlated
/// JSON-RPC error for an object with an `id`, no line for an object without
/// one (a notification), and `ERR bad_request` for anything else.
fn limited_line(body: &[u8]) -> Option<String> {
    match serde_json::from_slice::<Value>(body) {
        Ok(Value::Object(message)) => message.get("id").map(|id| {
            json!({"jsonrpc": "2.0", "id": id,
                   "error": {"code": -32000, "message": LIMITED_TEXT}})
            .to_string()
        }),
        _ => Some(ERR_BAD_REQUEST.to_owned()),
    }
}

/// True when the peer closed the socket in both directions (POLLHUP). A
/// half-close (POLLRDHUP without POLLHUP) does not count: the wire-v1 client
/// may close its write side and still wait for the reply.
///
/// Linux reports POLLHUP for a fully closed AF_UNIX peer. The behaviour on
/// macOS is a live-smoke item of MCP P1 (design §3.4).
fn peer_hung_up(fd: &OwnedFd) -> bool {
    use rustix::event::{PollFd, PollFlags};
    let mut fds = [PollFd::new(fd, PollFlags::HUP)];
    let now = rustix::time::Timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    match rustix::event::poll(&mut fds, Some(&now)) {
        Ok(_) => fds[0].revents().intersects(PollFlags::HUP),
        Err(error) => {
            tracing::debug!(kind = ?io::Error::from(error).kind(), "the bridge socket poll failed");
            false
        }
    }
}

/// Completes when the peer of `fd` closed the socket completely. Never
/// completes without a descriptor to watch.
async fn hung_up(fd: Option<&OwnedFd>) {
    let Some(fd) = fd else {
        return std::future::pending().await;
    };
    loop {
        tokio::time::sleep(HANGUP_POLL).await;
        if peer_hung_up(fd) {
            return;
        }
    }
}

/// The line for the outcome of one message, or `None` for no line.
fn outcome_line(outcome: MessageOutcome) -> Option<String> {
    match outcome {
        // A notification is accepted: 202, no body.
        MessageOutcome::Reply(reply) if reply.status == http::StatusCode::ACCEPTED => None,
        MessageOutcome::Reply(reply) => Some(
            serde_json::from_slice::<Value>(&reply.body)
                .map_or_else(|_| ERR_BAD_REQUEST.to_owned(), |value| value.to_string()),
        ),
        MessageOutcome::ParseError | MessageOutcome::Batch => Some(ERR_BAD_REQUEST.to_owned()),
        MessageOutcome::Unauthorized => Some(ERR_UNAUTHORIZED.to_owned()),
        MessageOutcome::Limited => Some(ERR_LIMITED.to_owned()),
        MessageOutcome::Starting => Some(ERR_STARTING.to_owned()),
    }
}

async fn connection(stream: UnixStream, shared: &Shared) {
    // A descriptor of the same socket, to watch for a full close while the
    // call runs (the halves below own the stream).
    let watch = match stream.as_fd().try_clone_to_owned() {
        Ok(fd) => Some(fd),
        Err(error) => {
            tracing::debug!(kind = ?error.kind(), "a bridge connection cannot be watched");
            None
        }
    };
    let (read, mut write) = stream.into_split();
    let mut reader = BufReader::new(read);
    // The binding permit lives until the reply line is written.
    let mut permit = None;
    let reply = match tokio::time::timeout(IDLE, intake(&mut reader, shared)).await {
        Err(_) => None,
        Ok(Err(error)) => {
            tracing::debug!(kind = ?error.kind(), "a bridge connection failed");
            None
        }
        Ok(Ok(Intake::Close)) => None,
        Ok(Ok(Intake::Refuse(line))) => Some(line.to_owned()),
        Ok(Ok(Intake::Limited(body))) => limited_line(&body),
        Ok(Ok(Intake::Ready(binding, held, body))) => {
            permit = Some(held);
            let legacy = shared.bindings.legacy_version(&binding.binding_id);
            let answered = answer_message(shared, &binding, body, |message| {
                bridge_headers(message, legacy.as_deref())
            });
            tokio::select! {
                outcome = answered => outcome_line(outcome),
                // The peer left: the call is dropped, which cancels it.
                () = hung_up(watch.as_ref()) => return,
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
    drop(permit);
}

#[cfg(test)]
#[path = "socket_tests.rs"]
pub(super) mod tests;
