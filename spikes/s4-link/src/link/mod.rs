//! A link: one control and one data WebSocket channel over pinned-key mTLS,
//! both opened by the dialer (PLAN §8.1), with the §8.2 handshake on each.
//!
//! Either role can dial: a station dials its deck (the NAT case), or a deck
//! dials a station that listens. The listener keeps a link table per peer key:
//! a control channel with a higher `link_epoch` supersedes the current link
//! (the old one gets `Close { reason: "superseded" }`), a lower or equal epoch
//! is refused with `stale_link_epoch`, and a data channel attaches only to the
//! current control channel of the same epoch.

pub mod control;
pub mod data;
mod handshake;

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, RwLock};

use tokio::net::TcpListener;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tokio_tungstenite::tungstenite::http::StatusCode;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::{WebSocketStream, accept_hdr_async_with_config, client_async_with_config};

pub use control::{Control, Inbound};
pub use data::{Data, DataEvent};

use crate::frames::{MAX_CONTROL_FRAME, Scope};
use crate::hello::{
    Channel, EXPORTER_LABEL, HandshakeError, HandshakeFrame, LocalHello, Negotiated, Side, feature,
};
use crate::identity::{Identity, PeerKey};
use crate::mux::MAX_CHUNK;
use crate::net::{self, NetError};
use crate::tls::{self, PeerPolicy, PinSet, Role, SERVER_NAME, TlsError};

pub(crate) type Ws = WebSocketStream<tokio_rustls::TlsStream<tokio::net::TcpStream>>;

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
}

impl LinkError {
    fn ws(e: impl std::fmt::Display) -> Self {
        Self::Ws(e.to_string())
    }

    fn closed(reason: &str, detail: &str) -> Self {
        Self::Closed {
            reason: reason.to_owned(),
            detail: detail.to_owned(),
        }
    }

    fn gone() -> Self {
        Self::closed("closed", "the link task ended")
    }

    /// The stable reason code, when the peer or this node refused the link.
    pub fn reason(&self) -> Option<&str> {
        match self {
            Self::Handshake(HandshakeError::PeerClosed { reason, .. }) => Some(reason),
            Self::Handshake(e) => Some(e.reason()),
            Self::Closed { reason, .. } => Some(reason),
            _ => None,
        }
    }
}

/// The scopes this node grants to each paired peer (L15). Revocation and
/// narrowing take effect on the next request.
#[derive(Debug, Default)]
pub struct Grants {
    map: RwLock<BTreeMap<PeerKey, BTreeSet<Scope>>>,
}

impl Grants {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn grant(&self, peer: PeerKey, scopes: impl IntoIterator<Item = Scope>) {
        if let Ok(mut m) = self.map.write() {
            m.insert(peer, scopes.into_iter().collect());
        }
    }

    pub fn revoke(&self, peer: &PeerKey) {
        if let Ok(mut m) = self.map.write() {
            m.remove(peer);
        }
    }

    /// The scopes of `peer`; none when unknown or when the lock is poisoned.
    pub fn of(&self, peer: &PeerKey) -> BTreeSet<Scope> {
        self.map
            .read()
            .ok()
            .and_then(|m| m.get(peer).cloned())
            .unwrap_or_default()
    }
}

/// What a node needs to dial or listen.
#[derive(Clone)]
pub struct NodeConfig {
    pub identity: Arc<Identity>,
    pub local: LocalHello,
    pub pins: Arc<PinSet>,
    pub grants: Arc<Grants>,
}

impl NodeConfig {
    pub fn new(identity: Arc<Identity>, role: Role) -> Self {
        Self {
            identity,
            local: LocalHello::new(role),
            pins: PinSet::new(),
            grants: Grants::new(),
        }
    }

    fn peer_role(&self) -> Role {
        crate::hello::expected_peer_role(self.local.role)
    }
}

/// A live link.
pub struct Link {
    pub peer: PeerKey,
    pub peer_role: Role,
    /// The dialer's epoch that names this link.
    pub epoch: u64,
    pub side: Side,
    pub negotiated: Negotiated,
    pub control: Control,
    pub inbound: mpsc::Receiver<Inbound>,
    /// Present when both sides negotiated `data_streams.v1`.
    pub data: Option<(Data, mpsc::UnboundedReceiver<DataEvent>)>,
}

impl std::fmt::Debug for Link {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Link")
            .field("peer", &self.peer.fingerprint().short())
            .field("peer_role", &self.peer_role)
            .field("epoch", &self.epoch)
            .field("side", &self.side)
            .field("negotiated", &self.negotiated)
            .field("data", &self.data.is_some())
            .finish()
    }
}

fn ws_config(channel: Channel) -> WebSocketConfig {
    let max = match channel {
        Channel::Control => MAX_CONTROL_FRAME,
        Channel::Data => MAX_CHUNK + 16,
    };
    WebSocketConfig::default()
        .max_message_size(Some(max))
        .max_frame_size(Some(max))
}

fn exporter_of(conn: &rustls::ConnectionCommon<impl Sized>) -> Result<[u8; 32], LinkError> {
    conn.export_keying_material([0u8; 32], EXPORTER_LABEL, None)
        .map_err(|e| LinkError::Protocol(format!("TLS exporter: {e}")))
}

async fn dial_channel(
    node: &NodeConfig,
    addr: SocketAddr,
    config: Arc<rustls::ClientConfig>,
    channel: Channel,
    epoch: u64,
) -> Result<(Ws, PeerKey, Negotiated), LinkError> {
    let (tls, peer) = net::connect(addr, config).await?;
    let exporter = exporter_of(tls.get_ref().1)?;
    let url = format!("wss://{SERVER_NAME}{}", channel.path());
    let (mut ws, _) = client_async_with_config(
        url,
        tokio_rustls::TlsStream::Client(tls),
        Some(ws_config(channel)),
    )
    .await
    .map_err(LinkError::ws)?;
    let est = handshake::run(&mut ws, node, Side::Dialer, channel, epoch, exporter, peer).await?;
    handshake::await_ready(&mut ws).await?;
    Ok((ws, peer, est.negotiated))
}

/// Opens one channel alone and waits for `Ready`. Tests use it to show that a
/// data channel without its control channel is refused.
#[doc(hidden)]
pub async fn dial_lone_channel(
    node: &NodeConfig,
    addr: SocketAddr,
    channel: Channel,
    epoch: u64,
) -> Result<(), LinkError> {
    let config = tls::client_config(
        &node.identity,
        PeerPolicy::Pinned {
            pins: node.pins.clone(),
            role: node.peer_role(),
        },
    )?;
    dial_channel(node, addr, config, channel, epoch)
        .await
        .map(|_| ())
}

/// Dials a listener: control channel first, then the data channel when both
/// sides support it. `epoch` must grow with every new link to the same peer.
pub async fn dial(node: &NodeConfig, addr: SocketAddr, epoch: u64) -> Result<Link, LinkError> {
    let peer_role = node.peer_role();
    let config = tls::client_config(
        &node.identity,
        PeerPolicy::Pinned {
            pins: node.pins.clone(),
            role: peer_role,
        },
    )?;
    let (ws, peer, negotiated) =
        dial_channel(node, addr, config.clone(), Channel::Control, epoch).await?;
    let data = if negotiated.has(feature::DATA_STREAMS) {
        let (dws, dpeer, _) = dial_channel(node, addr, config, Channel::Data, epoch).await?;
        if dpeer != peer {
            return Err(LinkError::Protocol(
                "the data channel reached another node".into(),
            ));
        }
        Some(data::spawn(dws, Side::Dialer))
    } else {
        None
    };
    let (control, inbound) = control::spawn(
        ws,
        control::Params {
            negotiated: negotiated.clone(),
            grants: node.grants.clone(),
            peer,
            // The dialer has no link table; it ends the link by dropping it.
            shutdown: None,
            on_exit: Box::new(|| {}),
        },
    );
    Ok(Link {
        peer,
        peer_role,
        epoch,
        side: Side::Dialer,
        negotiated,
        control,
        inbound,
        data,
    })
}

struct Entry {
    epoch: u64,
    shutdown: Option<oneshot::Sender<(String, String)>>,
    data: Option<oneshot::Sender<Ws>>,
}

type Table = Arc<Mutex<HashMap<PeerKey, Entry>>>;

/// A listening node. Links (and failed attempts) arrive on [`LinkListener::next`].
pub struct LinkListener {
    pub local_addr: SocketAddr,
    incoming: mpsc::Receiver<Result<Link, LinkError>>,
    task: JoinHandle<()>,
}

impl LinkListener {
    /// The next accepted link, or the error of a failed attempt.
    pub async fn next(&mut self) -> Option<Result<Link, LinkError>> {
        self.incoming.recv().await
    }

    /// The next accepted link; failed attempts are skipped.
    pub async fn next_link(&mut self) -> Option<Link> {
        while let Some(r) = self.incoming.recv().await {
            if let Ok(link) = r {
                return Some(link);
            }
        }
        None
    }
}

impl Drop for LinkListener {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Binds `addr` (D13 rules) and accepts links from pinned peers.
pub async fn listen(node: NodeConfig, addr: SocketAddr) -> Result<LinkListener, LinkError> {
    let listener: TcpListener = net::bind(addr).await?;
    let local_addr = listener.local_addr().map_err(NetError::Io)?;
    let config = tls::server_config(
        &node.identity,
        PeerPolicy::Pinned {
            pins: node.pins.clone(),
            role: node.peer_role(),
        },
    )?;
    let (tx, incoming) = mpsc::channel(16);
    let table: Table = Arc::default();
    let task = tokio::spawn(async move {
        while let Ok((tcp, from)) = listener.accept().await {
            let (node, config, tx, table) =
                (node.clone(), config.clone(), tx.clone(), table.clone());
            tokio::spawn(async move {
                if let Some(r) = accept_one(&node, tcp, from, config, &table).await {
                    let _ = tx.send(r).await;
                }
            });
        }
    });
    Ok(LinkListener {
        local_addr,
        incoming,
        task,
    })
}

/// Handles one TCP connection. `None` for a data channel that was handed to
/// its control channel.
async fn accept_one(
    node: &NodeConfig,
    tcp: tokio::net::TcpStream,
    from: SocketAddr,
    config: Arc<rustls::ServerConfig>,
    table: &Table,
) -> Option<Result<Link, LinkError>> {
    let r = async {
        let (tls, peer) = net::handshake_accepted(tcp, from, config).await?;
        let exporter = exporter_of(tls.get_ref().1)?;
        let mut chosen = None;
        // The callback type is fixed by tungstenite.
        #[allow(clippy::result_large_err)]
        let pick = |req: &Request, resp: Response| -> Result<Response, ErrorResponse> {
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
        let mut ws = accept_hdr_async_with_config(
            tokio_rustls::TlsStream::Server(tls),
            pick,
            Some(ws_config(Channel::Control)),
        )
        .await
        .map_err(LinkError::ws)?;
        let channel = chosen.ok_or_else(|| LinkError::Protocol("no channel path".into()))?;
        let est = handshake::run(&mut ws, node, Side::Listener, channel, 0, exporter, peer).await?;
        Ok::<_, LinkError>((ws, peer, channel, est))
    }
    .await;
    let (mut ws, peer, channel, est) = match r {
        Ok(v) => v,
        Err(e) => return Some(Err(e)),
    };
    let epoch = est.peer_hello.link_epoch;
    match channel {
        Channel::Control => {
            Some(attach_control(node, ws, peer, epoch, est.negotiated, table).await)
        }
        Channel::Data => {
            let slot = table.lock().ok().and_then(|mut t| {
                t.get_mut(&peer)
                    .filter(|e| e.epoch == epoch)
                    .and_then(|e| e.data.take())
            });
            match slot {
                Some(slot) => {
                    if handshake::send_frame(&mut ws, &HandshakeFrame::Ready)
                        .await
                        .is_ok()
                    {
                        let _ = slot.send(ws);
                    }
                    None
                }
                None => {
                    let e = HandshakeError::Protocol(format!(
                        "no control channel with link epoch {epoch} for this data channel"
                    ));
                    let _ = handshake::refuse(&mut ws, e).await;
                    Some(Err(LinkError::closed(
                        "no_control_channel",
                        "a data channel without its control channel",
                    )))
                }
            }
        }
    }
}

async fn attach_control(
    node: &NodeConfig,
    mut ws: Ws,
    peer: PeerKey,
    epoch: u64,
    negotiated: Negotiated,
    table: &Table,
) -> Result<Link, LinkError> {
    let (shutdown_tx, shutdown) = oneshot::channel();
    let wants_data = negotiated.has(feature::DATA_STREAMS);
    let (data_tx, data_rx) = oneshot::channel();
    let decision = {
        let Ok(mut t) = table.lock() else {
            return Err(LinkError::Protocol(
                "the link table lock is poisoned".into(),
            ));
        };
        match t.get_mut(&peer) {
            Some(cur) if cur.epoch >= epoch => Err(cur.epoch),
            cur => {
                if let Some(old) = cur.and_then(|c| c.shutdown.take()) {
                    let _ = old.send((
                        "superseded".into(),
                        format!("a link with epoch {epoch} replaced this link"),
                    ));
                }
                t.insert(
                    peer,
                    Entry {
                        epoch,
                        shutdown: Some(shutdown_tx),
                        data: wants_data.then_some(data_tx),
                    },
                );
                Ok(())
            }
        }
    };
    if let Err(current) = decision {
        let e = HandshakeError::Protocol(format!(
            "link epoch {epoch} is not newer than the current link epoch {current}"
        ));
        let _ = handshake::send_frame(
            &mut ws,
            &HandshakeFrame::Close {
                reason: "stale_link_epoch".into(),
                detail: e.to_string(),
            },
        )
        .await;
        return Err(LinkError::closed("stale_link_epoch", &e.to_string()));
    }
    handshake::send_frame(&mut ws, &HandshakeFrame::Ready).await?;
    let data = if wants_data {
        match tokio::time::timeout(net::HANDSHAKE_TIMEOUT, data_rx).await {
            Ok(Ok(dws)) => Some(data::spawn(dws, Side::Listener)),
            _ => {
                return Err(LinkError::closed(
                    "no_data_channel",
                    "the dialer did not open the data channel",
                ));
            }
        }
    } else {
        None
    };
    let table = table.clone();
    let on_exit = Box::new(move || {
        if let Ok(mut t) = table.lock()
            && t.get(&peer).is_some_and(|e| e.epoch == epoch)
        {
            t.remove(&peer);
        }
    });
    let (control, inbound) = control::spawn(
        ws,
        control::Params {
            negotiated: negotiated.clone(),
            grants: node.grants.clone(),
            peer,
            shutdown: Some(shutdown),
            on_exit,
        },
    );
    Ok(Link {
        peer,
        peer_role: node.peer_role(),
        epoch,
        side: Side::Listener,
        negotiated,
        control,
        inbound,
        data,
    })
}
