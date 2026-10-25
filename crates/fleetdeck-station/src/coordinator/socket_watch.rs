//! The `WATCH` push connection of the bridge socket (MCP P4b §8.3).
//!
//! A first line `WATCH <token>` turns the connection into a stream. The
//! station answers one line (`OK watch.v1`, or `ERR <code>` and a close), then
//! writes the notification line at once (a refresh for every change before
//! the registration) and once per coalesced change of the credential. It
//! closes when the watch is closed (the credential ended), when the peer
//! closes or sends any byte, when a write stalls, and when the socket stops
//! (the connection task is aborted with the socket). The token is never
//! logged.

use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use super::bindings::{BindingHandle, StreamAuth, StreamKind, StreamPermits};
use super::ingress::Shared;

/// The answer line of an established watch.
pub(super) const OK_WATCH: &str = "OK watch.v1";
/// The one line that a watch carries from the station.
pub(super) const LIST_CHANGED_LINE: &str =
    r#"{"jsonrpc":"2.0","method":"notifications/tools/list_changed"}"#;
/// A write that does not complete in this time closes the connection.
const WRITE_STALL: Duration = Duration::from_secs(30);

/// Admits a watch: the unauthenticated intake permit (like `AUTH`, held for
/// the handshake only), then the authentication of a stream of the kind
/// `Watch`. The error is the refusal line.
pub(super) fn admit(
    shared: &Shared,
    token: &str,
) -> Result<(BindingHandle, StreamPermits), &'static str> {
    let Ok(unauth) = shared.unauth.clone().try_acquire_owned() else {
        return Err("ERR limited");
    };
    let header = format!("Bearer {token}");
    let auth = shared
        .bindings
        .authenticate_stream(Some(&header), StreamKind::Watch);
    drop(unauth);
    match auth {
        StreamAuth::Unauthorized => Err("ERR unauthorized"),
        StreamAuth::Limited => Err("ERR limited"),
        StreamAuth::Starting => Err("ERR starting"),
        StreamAuth::Ok {
            handle, permits, ..
        } => Ok((handle, permits)),
    }
}

/// The push loop of an established watch: `OK watch.v1`, the notification at
/// once, then one per coalesced change, until the watch closes, the peer
/// closes or sends a byte, or a write stalls. The permits live as long as
/// the caller keeps them.
pub(super) async fn push<R, W>(reader: &mut R, write: &mut W, handle: BindingHandle)
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    if !send_line(write, OK_WATCH).await {
        return;
    }
    let mut changes = handle.changes;
    loop {
        if !send_line(write, LIST_CHANGED_LINE).await {
            return;
        }
        let mut byte = [0u8; 1];
        tokio::select! {
            // EOF, an error or any byte from the client closes.
            _ = reader.read(&mut byte) => return,
            changed = changes.changed() => {
                if changed.is_err() {
                    return;
                }
            }
        }
    }
}

/// Writes `line` and a newline within [`WRITE_STALL`]. False when the write
/// failed or did not complete in time.
async fn send_line<W: AsyncWrite + Unpin>(write: &mut W, line: &str) -> bool {
    let mut bytes = Vec::with_capacity(line.len() + 1);
    bytes.extend_from_slice(line.as_bytes());
    bytes.push(b'\n');
    let sent = async {
        write.write_all(&bytes).await?;
        write.flush().await
    };
    match tokio::time::timeout(WRITE_STALL, sent).await {
        Ok(Ok(())) => true,
        Ok(Err(error)) => {
            tracing::debug!(kind = ?error.kind(), "a watch line was not sent");
            false
        }
        Err(_) => {
            tracing::debug!("a watch line did not complete in time");
            false
        }
    }
}

#[cfg(test)]
#[path = "socket_watch_tests.rs"]
mod tests;
