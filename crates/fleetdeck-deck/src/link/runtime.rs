//! The deck's link node at run time (M2 P3-6b; PLAN §8.5, ADR-004 §2.8,
//! §2.10): listeners, dialers to the stations that listen, pairing in both
//! directions and `remove-station`.
//!
//! - **Listen.** The deck listens on the `link_listen` addresses of
//!   `deck.toml` for stations that dial in (a station behind NAT). Without
//!   such an address the deck only dials.
//! - **Dial.** Each paired station with a stored dial address gets a
//!   dialer with jittered backoff; it stops when the station is no longer
//!   pinned.
//! - **Pairing.** `invite` opens a single-use invitation (15 min) for a
//!   station that dials the deck (`station join`); `add_station` pairs with
//!   a station that listens (`station invite`), stores pin, grants, name and
//!   address in one transaction and starts its dialer.
//! - **Remove.** `remove_station` revokes the station key: one transaction
//!   in the store, and the link layer closes its links within 1 s (G5).

use std::net::SocketAddr;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use fleetdeck_core::Secret;
use fleetdeck_core::identity::{Identity, PeerKey};
use fleetdeck_link::hello::LocalHello;
use fleetdeck_link::node::{self, LinkListener, NodeConfig};
use fleetdeck_link::pairing::{Invitation, Invitations, PairError};
use fleetdeck_link::reconnect::{BackoffPolicy, DialerEvent, spawn_dialer};
use fleetdeck_link::revoke::revoke;
use fleetdeck_link::tls::Role;
use fleetdeck_link::ws::LinkError;
use fleetdeck_link::{Epochs, start};
use fleetdeck_proto::link::Scope;
use tokio::task::JoinHandle;

use super::{DeckLink, serve};
use crate::peers::{DeckPeers, Expected};
use crate::store::{DeckError, DeckStore, Direction, StationRecord};

/// How long an invitation of `deck invite` is valid (PLAN §8.5).
pub const INVITE_TTL: Duration = Duration::from_secs(15 * 60);

/// The scopes that a paired station gets on the deck (DL-050): `admin`
/// for `link.rotate_key`. `coordinate` (coordinator agents, G7) is granted
/// per station later.
pub const STATION_SCOPES: [Scope; 1] = [Scope::Admin];

/// How long `add-station` waits for the first link to learn the station's
/// name.
pub const NAME_WAIT: Duration = Duration::from_secs(3);

/// The close reason of the links at deck shutdown.
pub const SHUTDOWN: &str = "shutdown";

/// Why a deck link operation failed.
#[derive(Debug, thiserror::Error)]
pub enum NodeError {
    #[error(
        "the deck has no link listener; set `link_listen` in deck.toml for stations that dial in, or use `fleetdeck station invite` and `fleetdeck deck add-station`"
    )]
    NoListener,
    #[error("invitation: {0}")]
    Invitation(#[from] PairError),
    #[error(
        "an invitation of the deck cannot pair the deck with itself; use the station's invitation"
    )]
    NotAStation,
    #[error("a station is already named `{0}`")]
    NameTaken(String),
    #[error("no paired station is named `{0}`")]
    UnknownStation(String),
    #[error("pairing failed: {0}")]
    Pairing(String),
    #[error("link listen address {addr}: {reason}")]
    Listen { addr: SocketAddr, reason: String },
    #[error(transparent)]
    Store(#[from] DeckError),
}

/// The running link node of the deck.
pub struct DeckNode {
    ctx: Arc<DeckLink>,
    invitations: Arc<Invitations>,
    listening: Vec<SocketAddr>,
    tasks: Mutex<Vec<JoinHandle<()>>>,
    backoff: BackoffPolicy,
}

impl std::fmt::Debug for DeckNode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeckNode")
            .field("node", &self.ctx.node)
            .field("listening", &self.listening)
            .finish_non_exhaustive()
    }
}

impl DeckNode {
    /// Starts the node: pins from the store, listeners, dialers.
    ///
    /// # Errors
    /// A database error, or a listen address that cannot be bound (a public
    /// address is refused, D13).
    pub async fn start(
        store: DeckStore,
        identity: Arc<Identity>,
        link_listen: &[SocketAddr],
        backoff: BackoffPolicy,
    ) -> Result<Arc<Self>, NodeError> {
        let peers = DeckPeers::load(store.clone()).await?;
        let invitations = Invitations::new();
        let node = NodeConfig::new(
            identity,
            LocalHello::new(Role::Deck, store.incarnation().clone()),
            Arc::new(Epochs::sqlite(store.db())),
        )
        .with_store(Arc::clone(&peers))
        .with_invitations(Arc::clone(&invitations));
        let stations = peers.pin_all(&node.pins).await?;
        let ctx = Arc::new(DeckLink::new(store, peers, node));
        let mut listeners = Vec::new();
        for addr in link_listen {
            let listener =
                node::listen(ctx.node.clone(), *addr)
                    .await
                    .map_err(|e| NodeError::Listen {
                        addr: *addr,
                        reason: e.to_string(),
                    })?;
            tracing::info!(addr = %listener.local_addr, "deck listens for stations");
            listeners.push(listener);
        }
        let this = Arc::new(Self {
            ctx,
            invitations,
            listening: listeners.iter().map(|l| l.local_addr).collect(),
            tasks: Mutex::default(),
            backoff,
        });
        for listener in listeners {
            this.spawn(tokio::spawn(accept_loop(Arc::clone(&this.ctx), listener)));
        }
        for st in stations {
            this.dial_stored(&st);
        }
        Ok(this)
    }

    /// The shared link context.
    #[must_use]
    pub fn link(&self) -> &Arc<DeckLink> {
        &self.ctx
    }

    /// The bound link listen addresses.
    #[must_use]
    pub fn listen_addrs(&self) -> &[SocketAddr] {
        &self.listening
    }

    fn spawn(&self, task: JoinHandle<()>) {
        let mut tasks = self.tasks.lock().unwrap_or_else(PoisonError::into_inner);
        tasks.retain(|t| !t.is_finished());
        tasks.push(task);
    }

    fn dial_stored(&self, st: &StationRecord) {
        if st.direction != Direction::DeckDials {
            return;
        }
        let Some(url) = &st.dial_url else { return };
        match url.parse::<SocketAddr>() {
            Ok(addr) => self.dial(addr, st.key),
            Err(_) => tracing::warn!(url = %url, "a stored station address does not parse"),
        }
    }

    fn dial(&self, addr: SocketAddr, peer: PeerKey) {
        let (mut events, dialer) = spawn_dialer(self.ctx.node.clone(), addr, peer, self.backoff);
        let ctx = Arc::clone(&self.ctx);
        self.spawn(tokio::spawn(async move {
            while let Some(event) = events.recv().await {
                match event {
                    DialerEvent::Linked(link) => {
                        tokio::spawn(serve(Arc::clone(&ctx), *link));
                    }
                    DialerEvent::Stopped { reason } => {
                        tracing::info!(station = %peer.fingerprint().short(), %reason, "station dialer stopped");
                        break;
                    }
                    DialerEvent::Failed { .. } | DialerEvent::Lost { .. } => {}
                }
            }
            dialer.abort();
        }));
    }

    /// `deck invite`: a single-use invitation for a station that dials the
    /// deck (`fleetdeck station join`).
    ///
    /// # Errors
    /// [`NodeError::NoListener`] without a link listener.
    pub fn invite(&self) -> Result<Secret<String>, NodeError> {
        let addr = *self.listening.first().ok_or(NodeError::NoListener)?;
        let invitation = self.invitations.create(
            addr,
            self.ctx.node.identity.fingerprint(),
            INVITE_TTL,
            STATION_SCOPES,
        )?;
        Ok(invitation.reveal())
    }

    /// `deck add-station`: pairs with the listening station of `text`,
    /// stores it under `name` and starts dialing it. Without `name`, the
    /// station is stored under its short fingerprint until the first link
    /// tells its name; the call waits up to [`NAME_WAIT`] for that link.
    ///
    /// # Errors
    /// [`NodeError`]: the invitation is not valid, the name is taken, or the
    /// pairing failed.
    pub async fn add_station(
        &self,
        text: &Secret<String>,
        name: Option<String>,
    ) -> Result<StationRecord, NodeError> {
        let invitation = Invitation::parse(text)?;
        let fingerprint = invitation.fingerprint;
        if fingerprint == self.ctx.node.identity.fingerprint() {
            return Err(NodeError::NotAStation);
        }
        let store = self.ctx.peers.store();
        let wait_for_name = name.is_none();
        if let Some(name) = &name
            && let Some(other) = store.find_station(name.clone()).await?
            && other.key.fingerprint() != fingerprint
        {
            return Err(NodeError::NameTaken(name.clone()));
        }
        self.ctx.peers.expect(
            fingerprint,
            Expected {
                name: name.unwrap_or_else(|| fingerprint.short()),
                dial_url: Some(invitation.addr.to_string()),
            },
        );
        match fleetdeck_link::pair(&self.ctx.node, &invitation, STATION_SCOPES).await {
            Ok(paired) => {
                tracing::info!(station = %fingerprint.short(), "paired with the station");
                self.dial(invitation.addr, paired.key);
                let deadline = tokio::time::Instant::now() + NAME_WAIT;
                loop {
                    let st = store
                        .station_by_key(paired.key)
                        .await?
                        .ok_or_else(|| NodeError::Pairing("the pin was not stored".to_owned()))?;
                    if !wait_for_name
                        || st.name != fingerprint.short()
                        || tokio::time::Instant::now() >= deadline
                    {
                        return Ok(st);
                    }
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            }
            Err(e) => {
                self.ctx.peers.pairing_failed(&fingerprint);
                Err(NodeError::Pairing(describe(&e)))
            }
        }
    }

    /// `deck remove-station <name>`: revokes the station key and closes its
    /// links. Returns the removed record.
    ///
    /// # Errors
    /// [`NodeError::UnknownStation`], or a database error.
    pub async fn remove_station(&self, name_or_id: &str) -> Result<StationRecord, NodeError> {
        let st = self
            .ctx
            .store
            .find_station(name_or_id.to_owned())
            .await?
            .ok_or_else(|| NodeError::UnknownStation(name_or_id.to_owned()))?;
        revoke(&self.ctx.node, &st.key).await;
        tracing::info!(station = %st.name, "station removed");
        Ok(st)
    }

    /// Stops the listeners and the dialers and closes every link.
    pub fn shutdown(&self) {
        let tasks = std::mem::take(&mut *self.tasks.lock().unwrap_or_else(PoisonError::into_inner));
        for task in tasks {
            task.abort();
        }
        self.ctx.close_all(SHUTDOWN);
    }
}

fn describe(e: &LinkError) -> String {
    match e.reason() {
        Some(reason) => format!("{reason} ({e})"),
        None => e.to_string(),
    }
}

async fn accept_loop(ctx: Arc<DeckLink>, mut listener: LinkListener) {
    while let Some(result) = listener.next().await {
        match result {
            Ok(open) => {
                let link = start(&ctx.node, open);
                tokio::spawn(serve(Arc::clone(&ctx), link));
            }
            Err(e) => tracing::info!(error = %describe(&e), "a link attempt failed"),
        }
    }
}
