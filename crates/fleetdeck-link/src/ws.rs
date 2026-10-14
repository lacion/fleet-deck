//! TLS + WebSocket channels and the handshake frame I/O (ADR-004 §2).
//!
//! Every best-effort cleanup write (a `close` frame, the WebSocket close) is
//! bounded by [`CLEANUP_LIMIT`]: a peer that stopped reading never holds a
//! task (DL-023).

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tokio_tungstenite::tungstenite::http::StatusCode;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::{WebSocketStream, accept_hdr_async_with_config, client_async_with_config};

use fleetdeck_core::identity::PeerKey;
use fleetdeck_proto::link::MAX_CONTROL_FRAME_BYTES;

use crate::hello::{Channel, EXPORTER_LABEL, HandshakeError, HandshakeFrame};
use crate::net::{self, NetError};
use crate::tls::{PinSet, Role, SERVER_NAME, TlsError};
use crate::transport::Stream;

/// One WebSocket channel over mutual TLS.
pub type Ws = WebSocketStream<tokio_rustls::TlsStream<Stream>>;

/// The largest data channel chunk (ADR-004 §9.2); a data message is a chunk
/// plus its frame header.
pub const MAX_DATA_CHUNK: usize = 64 * 1024;

/// Bound of every best-effort cleanup write.
pub const CLEANUP_LIMIT: Duration = Duration::from_secs(1);

/// Setup step names in [`LinkError::Timeout`].
pub const STEP_UPGRADE: &str = "WebSocket upgrade";
pub const STEP_HELLO: &str = "hello/auth exchange";
pub const STEP_READY: &str = "wait for ready";
pub const STEP_WRITE: &str = "handshake write";
pub const STEP_DATA: &str = "wait for the data channel";

#[derive(Debug, thiserror::Error)]
pub enum LinkError {
    #[error(transparent)]
    Net(#[from] NetError),
    #[error(transparent)]
    Tls(#[from] TlsError),
    #[error("websocket: {0}")]
    Ws(String),
    #[error(transparent)]
    Handshake(#[from] HandshakeError),
    #[error("the link closed: {reason}: {detail}")]
    Closed { reason: String, detail: String },
    #[error("link protocol error: {0}")]
    Protocol(String),
    /// The link-epoch store failed; the dial does not start (G1).
    #[error("link epoch store: {0}")]
    Epochs(String),
    /// A peer that stalls a setup step.
    #[error("the {0} took longer than {1:?}")]
    Timeout(&'static str, Duration),
}

impl LinkError {
    pub(crate) fn ws(e: impl std::fmt::Display) -> Self {
        Self::Ws(e.to_string())
    }

    pub(crate) fn closed(reason: &str, detail: impl Into<String>) -> Self {
        Self::Closed {
            reason: reason.to_owned(),
            detail: detail.into(),
        }
    }

    /// The stable reason code, when the peer or this node refused the link.
    pub fn reason(&self) -> Option<&str> {
        match self {
            Self::Handshake(HandshakeError::PeerClosed { reason, .. }) => Some(reason),
            Self::Handshake(e) => Some(e.reason()),
            Self::Closed { reason, .. } => Some(reason),
            Self::Timeout(..) | Self::Net(NetError::Timeout(_)) => Some("timeout"),
            Self::Net(NetError::SelfConnect(_)) => Some("self_connect"),
            _ => None,
        }
    }
}

pub(crate) fn ws_config(channel: Channel) -> WebSocketConfig {
    let max = match channel {
        Channel::Control | Channel::Pair => MAX_CONTROL_FRAME_BYTES,
        Channel::Data => MAX_DATA_CHUNK + 16,
    };
    WebSocketConfig::default()
        .max_message_size(Some(max))
        .max_frame_size(Some(max))
}

pub(crate) fn exporter_of(
    conn: &rustls::ConnectionCommon<impl Sized>,
) -> Result<[u8; 32], LinkError> {
    conn.export_keying_material([0u8; 32], EXPORTER_LABEL, None)
        .map_err(|e| LinkError::Protocol(format!("TLS exporter: {e}")))
}

/// TLS + WebSocket upgrade to `channel`; returns the proved peer key and the
/// TLS exporter.
pub(crate) async fn connect(
    addr: SocketAddr,
    config: Arc<rustls::ClientConfig>,
    channel: Channel,
    limit: Duration,
) -> Result<(Ws, PeerKey, [u8; 32]), LinkError> {
    let (tls, peer) = net::connect(addr, config).await?;
    let exporter = exporter_of(tls.get_ref().1)?;
    let url = format!("wss://{SERVER_NAME}{}", channel.path());
    let upgrade = client_async_with_config(
        url,
        tokio_rustls::TlsStream::Client(tls),
        Some(ws_config(channel)),
    );
    let (ws, _) = tokio::time::timeout(limit, upgrade)
        .await
        .map_err(|_| LinkError::Timeout(STEP_UPGRADE, limit))?
        .map_err(LinkError::ws)?;
    Ok((ws, peer, exporter))
}

/// The `Host` value that the dialer of this protocol sends (§6 of the
/// tailnet design). A browser can not send it for an address of the deck.
fn host_is_ours(req: &Request) -> bool {
    req.headers()
        .get("host")
        .and_then(|h| h.to_str().ok())
        .is_some_and(|h| h.eq_ignore_ascii_case(SERVER_NAME))
}

/// The stage deadlines of an accept, and the message limit for a key that
/// is not pinned (tailnet listeners).
pub(crate) struct AcceptOpts<'a> {
    pub tls: Duration,
    pub upgrade: Duration,
    /// `(limit, pins, role)`: a peer key without `role` in `pins` gets
    /// messages of at most `limit` bytes.
    pub unpinned_cap: Option<(usize, &'a PinSet, Role)>,
}

/// TLS accept + WebSocket upgrade; the request path names the channel. A
/// request with an `Origin` header, or with another `Host` than
/// [`SERVER_NAME`], is refused (a browser is not a link peer).
pub(crate) async fn accept(
    stream: Stream,
    from: SocketAddr,
    config: Arc<rustls::ServerConfig>,
    opts: AcceptOpts<'_>,
) -> Result<(Ws, PeerKey, Channel, [u8; 32]), LinkError> {
    let limit = opts.upgrade;
    let (tls, peer) = net::handshake_accepted_within(stream, from, config, opts.tls).await?;
    let exporter = exporter_of(tls.get_ref().1)?;
    let mut chosen = None;
    // The callback type is fixed by tungstenite.
    #[allow(clippy::result_large_err)]
    let pick = |req: &Request, resp: Response| -> Result<Response, ErrorResponse> {
        if req.headers().contains_key("origin") || !host_is_ours(req) {
            let mut e = ErrorResponse::new(Some("not a link peer".into()));
            *e.status_mut() = StatusCode::FORBIDDEN;
            return Err(e);
        }
        match Channel::from_path(req.uri().path()) {
            Some(c) => {
                chosen = Some(c);
                Ok(resp)
            }
            None => {
                let mut e = ErrorResponse::new(Some("unknown link path".into()));
                *e.status_mut() = StatusCode::NOT_FOUND;
                Err(e)
            }
        }
    };
    // The largest limit of any channel; the data channel never sends more
    // than a chunk, and a control frame is checked again when parsed.
    let mut ws_cfg = ws_config(Channel::Control);
    if let Some((cap, pins, role)) = opts.unpinned_cap
        && pins.role_of(&peer) != Some(role)
    {
        ws_cfg = ws_cfg.max_message_size(Some(cap)).max_frame_size(Some(cap));
    }
    let upgrade = accept_hdr_async_with_config(
        tokio_rustls::TlsStream::Server(tls),
        pick,
        Some(ws_cfg),
    );
    let ws = tokio::time::timeout(limit, upgrade)
        .await
        .map_err(|_| LinkError::Timeout(STEP_UPGRADE, limit))?
        .map_err(LinkError::ws)?;
    let channel = chosen.ok_or_else(|| LinkError::Protocol("no channel path".into()))?;
    Ok((ws, peer, channel, exporter))
}

pub(crate) async fn send_frame(ws: &mut Ws, frame: &HandshakeFrame) -> Result<String, LinkError> {
    let text = serde_json::to_string(frame).map_err(|e| LinkError::Protocol(e.to_string()))?;
    ws.send(Message::text(text.clone()))
        .await
        .map_err(LinkError::ws)?;
    Ok(text)
}

/// The next text message, at most `cap` bytes; WebSocket ping/pong is
/// skipped.
pub(crate) async fn recv_text_capped(ws: &mut Ws, cap: usize) -> Result<String, LinkError> {
    loop {
        match ws.next().await {
            None => return Err(LinkError::closed("eof", "the peer closed the connection")),
            Some(Err(e)) => return Err(LinkError::ws(e)),
            Some(Ok(Message::Text(t))) if t.len() > cap => {
                return Err(LinkError::Protocol(format!(
                    "a message of {} bytes is above the limit of {cap}",
                    t.len()
                )));
            }
            Some(Ok(Message::Text(t))) => return Ok(t.as_str().to_owned()),
            Some(Ok(Message::Close(_))) => {
                return Err(LinkError::closed("eof", "the peer closed the WebSocket"));
            }
            Some(Ok(Message::Ping(_) | Message::Pong(_) | Message::Frame(_))) => {}
            Some(Ok(Message::Binary(_))) => {
                return Err(LinkError::Protocol(
                    "binary message during the handshake".into(),
                ));
            }
        }
    }
}

/// The next handshake frame. A peer `Close` becomes
/// [`HandshakeError::PeerClosed`]; a `stale_link_epoch` close keeps its
/// `min_epoch` as [`HandshakeError::StaleEpoch`].
pub(crate) async fn recv_frame(ws: &mut Ws) -> Result<(String, HandshakeFrame), LinkError> {
    recv_frame_capped(ws, usize::MAX).await
}

/// [`recv_frame`] that refuses a message above `cap` bytes.
pub(crate) async fn recv_frame_capped(
    ws: &mut Ws,
    cap: usize,
) -> Result<(String, HandshakeFrame), LinkError> {
    let text = recv_text_capped(ws, cap).await?;
    let frame = serde_json::from_str(&text)
        .map_err(|e| LinkError::Handshake(HandshakeError::BadHello(e.to_string())))?;
    match frame {
        HandshakeFrame::Close {
            reason,
            min_epoch: Some(min_epoch),
            ..
        } if reason == "stale_link_epoch" => {
            Err(LinkError::Handshake(HandshakeError::StaleEpoch {
                got: 0,
                min_epoch,
            }))
        }
        HandshakeFrame::Close { reason, detail, .. } => {
            Err(LinkError::Handshake(HandshakeError::PeerClosed {
                reason,
                detail,
            }))
        }
        f => Ok((text, f)),
    }
}

/// Closes the WebSocket, best effort, within [`CLEANUP_LIMIT`].
pub(crate) async fn close_quietly(ws: &mut Ws) {
    let _ = tokio::time::timeout(CLEANUP_LIMIT, ws.close(None)).await;
}

/// Sends `frame` and then closes, best effort, within [`CLEANUP_LIMIT`].
pub(crate) async fn close_with(ws: &mut Ws, frame: &HandshakeFrame) {
    let _ = tokio::time::timeout(CLEANUP_LIMIT, async {
        let _ = send_frame(ws, frame).await;
        let _ = ws.close(None).await;
    })
    .await;
}

/// Sends the `Close` frame for `e` (best effort, bounded) and returns `e`.
pub(crate) async fn refuse(ws: &mut Ws, e: HandshakeError) -> LinkError {
    close_with(ws, &e.close_frame()).await;
    LinkError::Handshake(e)
}

/// Sends `Close { reason, detail }` (best effort, bounded) and returns the
/// matching [`LinkError::Closed`].
pub(crate) async fn close_reason(ws: &mut Ws, reason: &str, detail: String) -> LinkError {
    close_with(
        ws,
        &HandshakeFrame::Close {
            reason: reason.to_owned(),
            detail: detail.clone(),
            min_epoch: None,
        },
    )
    .await;
    LinkError::closed(reason, detail)
}

/// Tells the peer that `step` timed out (best effort, bounded) and returns
/// [`LinkError::Timeout`], whose reason is `timeout` on both sides.
pub(crate) async fn timed_out(ws: &mut Ws, step: &'static str, limit: Duration) -> LinkError {
    let e = LinkError::Timeout(step, limit);
    close_with(
        ws,
        &HandshakeFrame::Close {
            reason: "timeout".into(),
            detail: e.to_string(),
            min_epoch: None,
        },
    )
    .await;
    e
}

/// [`send_frame`] bounded by `limit`.
pub(crate) async fn send_within(
    ws: &mut Ws,
    frame: &HandshakeFrame,
    limit: Duration,
) -> Result<String, LinkError> {
    match tokio::time::timeout(limit, send_frame(ws, frame)).await {
        Ok(r) => r,
        Err(_) => Err(timed_out(ws, STEP_WRITE, limit).await),
    }
}
