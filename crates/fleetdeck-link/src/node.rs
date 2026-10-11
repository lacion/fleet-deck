//! Link setup: a control and an optional data WebSocket channel over
//! pinned-key mTLS, both opened by the dialer (PLAN §8.1), with the §8.2
//! handshake on each and the Revision 2 link rules (ADR-004 §9.1).
//!
//! - The dialer commits the next per-peer epoch before it dials and sends it
//!   with a fresh `attempt_id` on every channel of the attempt. A
//!   `stale_link_epoch` refusal raises the counter above `min_epoch`, and the
//!   dialer tries once more.
//! - The listener accepts an epoch only when it is higher than every epoch
//!   it accepted from that dialer, commits it, and then admits the link into
//!   the node-wide [`LinkTable`] before it sends `Ready`.
//! - A data channel binds to the control channel with the same peer, epoch
//!   and `attempt_id`; any other data channel gets `no_control_channel`.
//!
//! The result is an [`OpenLink`]: the authenticated channels and the table
//! guard. The control and data protocols run on top of it.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;

use fleetdeck_core::identity::{Identity, PeerKey};

use crate::control::{ControlConfig, GrantSource, Grants};
use crate::epochs::Epochs;
use crate::handshake::{self, Own};
use crate::hello::{
    Attempt, Channel, HandshakeError, HandshakeFrame, LocalHello, Negotiated, Side, feature,
};
use crate::net::{self, NetError};
use crate::pairing::{Invitations, PairLimiter, PairingSink};
use crate::table::{Admitted, Closing, Dialer, LinkId, LinkTable, SUPERSEDED, TableGuard};
use crate::tls::{self, InvitationGate, PeerPolicy, PinSet, Role};
use crate::tokens::StreamTokens;
use crate::ws::{self, LinkError, STEP_DATA, Ws};

/// The close reason of a data channel without its control channel.
pub const NO_CONTROL_CHANNEL: &str = "no_control_channel";
/// The close reason when the dialer never opened the data channel.
pub const NO_DATA_CHANNEL: &str = "no_data_channel";

/// An invitation gate that never has an open invitation.
#[derive(Debug)]
pub struct NoInvitations;

impl InvitationGate for NoInvitations {
    fn has_open(&self) -> bool {
        false
    }
}

/// What a node needs to dial or listen.
#[derive(Clone)]
pub struct NodeConfig {
    pub identity: Arc<Identity>,
    pub local: LocalHello,
    pub pins: Arc<PinSet>,
    pub invites: Arc<dyn InvitationGate>,
    pub epochs: Arc<Epochs>,
    pub table: LinkTable,
    /// The scopes this node granted to each paired key (empty: none).
    pub grants: Arc<dyn GrantSource>,
    /// Limits of the control channel of every link (G8).
    pub control: ControlConfig,
    /// Stream tokens that this node issued, for all of its links (G2).
    pub tokens: StreamTokens,
    /// Bound of each setup step after TLS: the WebSocket upgrade, the
    /// hello/auth exchange, the wait for `Ready` and for the data channel.
    pub handshake_timeout: Duration,
    /// Stores what pairing, revocation and key rotation change. The default
    /// is the in-memory [`Grants`] that is also `grants`.
    pub sink: Arc<dyn PairingSink>,
    /// The open invitations of this node; `None`: the pair channel is closed.
    pub invitations: Option<Arc<Invitations>>,
    /// Admission of pairing handshakes on this node's listeners (G11).
    pub pair_limiter: PairLimiter,
}

impl NodeConfig {
    pub fn new(identity: Arc<Identity>, local: LocalHello, epochs: Arc<Epochs>) -> Self {
        let table = LinkTable::new(identity.fingerprint());
        let grants = Grants::new();
        Self {
            identity,
            local,
            pins: PinSet::new(),
            invites: Arc::new(NoInvitations),
            epochs,
            table,
            grants: Arc::clone(&grants) as Arc<dyn GrantSource>,
            control: ControlConfig::default(),
            tokens: StreamTokens::default(),
            handshake_timeout: net::HANDSHAKE_TIMEOUT,
            sink: grants,
            invitations: None,
            pair_limiter: PairLimiter::default(),
        }
    }

    /// Uses `store` both to read grants and to store pairing changes.
    #[must_use]
    pub fn with_store<S: GrantSource + PairingSink + 'static>(mut self, store: Arc<S>) -> Self {
        self.grants = Arc::clone(&store) as Arc<dyn GrantSource>;
        self.sink = store;
        self
    }

    /// Opens the pair channel: listeners accept pairing for `invitations`,
    /// and TLS lets an unpinned key in while one of them is open.
    #[must_use]
    pub fn with_invitations(mut self, invitations: Arc<Invitations>) -> Self {
        self.invites = Arc::clone(&invitations) as Arc<dyn InvitationGate>;
        self.invitations = Some(invitations);
        self
    }

    pub fn peer_role(&self) -> Role {
        crate::hello::expected_peer_role(self.local.role)
    }

    pub(crate) fn own<'a>(&'a self, attempt: Option<&'a Attempt>) -> Own<'a> {
        Own {
            identity: &self.identity,
            local: &self.local,
            attempt,
            limit: self.handshake_timeout,
        }
    }
}

impl std::fmt::Debug for NodeConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NodeConfig")
            .field("node", &self.identity.fingerprint().short())
            .field("role", &self.local.role)
            .field("table", &self.table)
            .finish_non_exhaustive()
    }
}

/// An authenticated link: its channels and its table entry.
pub struct OpenLink {
    pub peer: PeerKey,
    pub id: LinkId,
    pub side: Side,
    pub negotiated: Negotiated,
    pub control: Ws,
    /// Present when both sides negotiated `data_streams.v1`.
    pub data: Option<Ws>,
    /// Becomes `Some` when the link must close (superseded, revoked).
    pub closing: watch::Receiver<Option<Closing>>,
    /// Removes the table entry when the link is dropped.
    pub guard: TableGuard,
}

impl std::fmt::Debug for OpenLink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenLink")
            .field("peer", &self.peer.fingerprint().short())
            .field("id", &self.id)
            .field("side", &self.side)
            .field("negotiated", &self.negotiated)
            .field("data", &self.data.is_some())
            .finish_non_exhaustive()
    }
}

/// Test hooks for [`dial_with`]: failure injection at each setup step.
#[doc(hidden)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum DialFault {
    #[default]
    None,
    /// Use this epoch and do not commit or retry (a replayed old dialer).
    FixedEpoch(u64),
    /// Close the control channel after `Auth`, before `Ready`.
    CloseBeforeReady,
    /// Never open the data channel.
    NoData,
    /// Open the data channel with another `attempt_id`.
    WrongDataAttempt,
}

/// Dials `peer` at `addr`: control channel first, then the data channel when
/// both sides support it. `peer` must be pinned with the expected role.
pub async fn dial(
    node: &NodeConfig,
    addr: SocketAddr,
    peer: PeerKey,
) -> Result<OpenLink, LinkError> {
    dial_with(node, addr, peer, DialFault::None).await
}

#[doc(hidden)]
pub async fn dial_with(
    node: &NodeConfig,
    addr: SocketAddr,
    peer: PeerKey,
    fault: DialFault,
) -> Result<OpenLink, LinkError> {
    if node.pins.role_of(&peer) != Some(node.peer_role()) {
        return Err(LinkError::Handshake(HandshakeError::NotPaired));
    }
    let fp = peer.fingerprint();
    let config = tls::client_config(&node.identity, PeerPolicy::Fingerprint(fp))?;
    let mut retried = false;
    loop {
        let epoch = match fault {
            DialFault::FixedEpoch(e) => e,
            _ => node.epochs.next_dial(&fp).await?,
        };
        let attempt = Attempt::new(epoch)?;
        match dial_attempt(node, addr, peer, &config, &attempt, fault).await {
            Err(LinkError::Handshake(HandshakeError::StaleEpoch { min_epoch, .. }))
                if !retried && !matches!(fault, DialFault::FixedEpoch(_)) =>
            {
                tracing::info!(min_epoch, "link epoch was stale; dialing again");
                node.epochs.raise_dial(&fp, min_epoch).await?;
                retried = true;
            }
            r => return r,
        }
    }
}

async fn open_channel(
    node: &NodeConfig,
    addr: SocketAddr,
    peer: PeerKey,
    config: &Arc<rustls::ClientConfig>,
    attempt: &Attempt,
    channel: Channel,
) -> Result<(Ws, Negotiated), LinkError> {
    let (mut ws, proved, exporter) =
        ws::connect(addr, config.clone(), channel, node.handshake_timeout).await?;
    if proved != peer {
        ws::close_quietly(&mut ws).await;
        return Err(LinkError::Protocol(
            "the channel reached another node".into(),
        ));
    }
    let est = handshake::run(
        &mut ws,
        &node.own(Some(attempt)),
        Side::Dialer,
        channel,
        exporter,
        peer,
    )
    .await?;
    Ok((ws, est.negotiated))
}

async fn dial_attempt(
    node: &NodeConfig,
    addr: SocketAddr,
    peer: PeerKey,
    config: &Arc<rustls::ClientConfig>,
    attempt: &Attempt,
    fault: DialFault,
) -> Result<OpenLink, LinkError> {
    let (mut control, negotiated) =
        open_channel(node, addr, peer, config, attempt, Channel::Control).await?;
    if fault == DialFault::CloseBeforeReady {
        ws::close_quietly(&mut control).await;
        return Err(LinkError::closed("test", "closed before ready"));
    }
    handshake::await_ready(&mut control, node.handshake_timeout).await?;
    let id = LinkId {
        epoch: attempt.epoch,
        attempt_id: attempt.attempt_id.clone(),
        dialer: Dialer::Local,
    };
    let fp = peer.fingerprint();
    let Admitted { guard, closing, .. } = match node.table.insert(fp, id.clone(), false) {
        Ok(a) => a,
        Err(lost) => {
            ws::close_quietly(&mut control).await;
            return Err(LinkError::closed(SUPERSEDED, lost.detail()));
        }
    };
    let data = if negotiated.has(feature::DATA_STREAMS) && fault != DialFault::NoData {
        let other;
        let data_attempt = if fault == DialFault::WrongDataAttempt {
            other = Attempt::new(attempt.epoch)?;
            &other
        } else {
            attempt
        };
        let (mut data, _) =
            open_channel(node, addr, peer, config, data_attempt, Channel::Data).await?;
        handshake::await_ready(&mut data, node.handshake_timeout).await?;
        Some(data)
    } else {
        None
    };
    Ok(OpenLink {
        peer,
        id,
        side: Side::Dialer,
        negotiated,
        control,
        data,
        closing,
        guard,
    })
}

/// A listening node. Links (and failed attempts) arrive on
/// [`LinkListener::next`].
pub struct LinkListener {
    pub local_addr: SocketAddr,
    incoming: mpsc::Receiver<Result<OpenLink, LinkError>>,
    task: JoinHandle<()>,
}

impl LinkListener {
    /// The next accepted link, or the error of a failed attempt.
    pub async fn next(&mut self) -> Option<Result<OpenLink, LinkError>> {
        self.incoming.recv().await
    }

    /// The next accepted link; failed attempts are skipped.
    pub async fn next_link(&mut self) -> Option<OpenLink> {
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
    let listener = net::bind(addr).await?;
    let local_addr = listener.local_addr().map_err(NetError::Io)?;
    let config = tls::server_config(
        &node.identity,
        PeerPolicy::PinnedOrInvited {
            pins: node.pins.clone(),
            role: node.peer_role(),
            invites: node.invites.clone(),
        },
    )?;
    let (tx, incoming) = mpsc::channel(16);
    let task = tokio::spawn(async move {
        while let Ok((stream, from)) = listener.accept().await {
            let (node, config, tx) = (node.clone(), config.clone(), tx.clone());
            tokio::spawn(async move {
                if let Some(r) = accept_one(&node, stream, from, config).await {
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

/// Handles one connection. `None` for a data channel that was handed to its
/// control channel.
async fn accept_one(
    node: &NodeConfig,
    stream: crate::transport::Stream,
    from: SocketAddr,
    config: Arc<rustls::ServerConfig>,
) -> Option<Result<OpenLink, LinkError>> {
    let limit = node.handshake_timeout;
    let accepted = async {
        let (mut ws, peer, channel, exporter) = ws::accept(stream, from, config, limit).await?;
        if channel == Channel::Pair && node.invitations.is_some() {
            return Ok(Err(crate::join::accept(
                node,
                ws,
                peer,
                from.ip(),
                exporter,
            )
            .await));
        }
        // TLS lets an unpinned key in while an invitation is open; such a key
        // may only pair.
        if channel == Channel::Pair || node.pins.role_of(&peer) != Some(node.peer_role()) {
            // Read the dialer's Hello first (bounded): a socket closed before
            // that write gives the dialer a broken pipe instead of the reason
            // (DL-025).
            let _ = tokio::time::timeout(limit, ws::recv_text(&mut ws)).await;
            let e = if channel == Channel::Pair {
                HandshakeError::Protocol("pairing is not open on this listener".into())
            } else {
                HandshakeError::NotPaired
            };
            return Err(ws::refuse(&mut ws, e).await);
        }
        let est = handshake::run(
            &mut ws,
            &node.own(None),
            Side::Listener,
            channel,
            exporter,
            peer,
        )
        .await?;
        Ok::<_, LinkError>(Ok((ws, peer, channel, est)))
    }
    .await;
    let (ws, peer, channel, est) = match accepted {
        Ok(Ok(v)) => v,
        // A pairing ended: success reports nothing, a failure its error.
        Ok(Err(paired)) => return paired.err().map(Err),
        Err(e) => return Some(Err(e)),
    };
    let epoch = est.peer_hello.link_epoch;
    let attempt_id = est.peer_hello.attempt_id.clone().unwrap_or_default();
    match channel {
        Channel::Data => attach_data(node, ws, peer, epoch, &attempt_id)
            .await
            .err()
            .map(Err),
        _ => Some(admit_control(node, ws, peer, epoch, attempt_id, est.negotiated).await),
    }
}

async fn attach_data(
    node: &NodeConfig,
    mut ws: Ws,
    peer: PeerKey,
    epoch: u64,
    attempt_id: &str,
) -> Result<(), LinkError> {
    let Some(slot) = node
        .table
        .take_data_slot(&peer.fingerprint(), epoch, attempt_id)
    else {
        let detail = format!("no control channel with link epoch {epoch} and this attempt");
        return Err(ws::close_reason(&mut ws, NO_CONTROL_CHANNEL, detail).await);
    };
    ws::send_within(&mut ws, &HandshakeFrame::Ready, node.handshake_timeout).await?;
    // The control side may have given up meanwhile; then the channel drops.
    let _ = slot.send(ws);
    Ok(())
}

async fn admit_control(
    node: &NodeConfig,
    mut ws: Ws,
    peer: PeerKey,
    epoch: u64,
    attempt_id: String,
    negotiated: Negotiated,
) -> Result<OpenLink, LinkError> {
    let fp = peer.fingerprint();
    if let Err(min_epoch) = node.epochs.accept(&fp, epoch).await? {
        return Err(ws::refuse(
            &mut ws,
            HandshakeError::StaleEpoch {
                got: epoch,
                min_epoch,
            },
        )
        .await);
    }
    let wants_data = negotiated.has(feature::DATA_STREAMS);
    let id = LinkId {
        epoch,
        attempt_id,
        dialer: Dialer::Remote,
    };
    // The guard exists from here on: every failure below removes the entry.
    let admitted = match node.table.insert(fp, id.clone(), wants_data) {
        Ok(a) => a,
        Err(lost) => return Err(ws::close_reason(&mut ws, SUPERSEDED, lost.detail()).await),
    };
    let Admitted {
        guard,
        closing,
        data,
    } = admitted;
    ws::send_within(&mut ws, &HandshakeFrame::Ready, node.handshake_timeout).await?;
    let data = match data {
        None => None,
        Some(rx) => match tokio::time::timeout(node.handshake_timeout, rx).await {
            Ok(Ok(dws)) => Some(dws),
            Ok(Err(_)) => {
                ws::close_quietly(&mut ws).await;
                return Err(LinkError::closed(
                    SUPERSEDED,
                    "a newer link replaced this link during setup",
                ));
            }
            Err(_) => {
                ws::close_quietly(&mut ws).await;
                return Err(LinkError::closed(
                    NO_DATA_CHANNEL,
                    format!(
                        "the {STEP_DATA} took longer than {:?}",
                        node.handshake_timeout
                    ),
                ));
            }
        },
    };
    Ok(OpenLink {
        peer,
        id,
        side: Side::Listener,
        negotiated,
        control: ws,
        data,
        closing,
        guard,
    })
}
