//! The station's link node at run time (M2 P3-5c; PLAN §8.5, ADR-004 §2.8,
//! §2.10): the listeners, the dialers to paired decks, pairing in both
//! directions and `forget-deck`.
//!
//! - **Listen.** At start the station listens on the `station.toml` listen
//!   addresses and on the addresses that `station invite --listen` added
//!   (DL-048). A station without any listen address only dials (the NAT
//!   case of the Phase 3 exit check).
//! - **Dial.** Each paired deck with a stored dial address gets a dialer
//!   with jittered backoff; it stops when the deck is no longer pinned.
//! - **Pairing.** `invite` opens a single-use invitation (15 min) for a deck
//!   that dials this station; `join` pairs with a deck that this station
//!   dials and starts its dialer. A station pairs with one deck: both
//!   refuse while a deck is paired (DL-048).
//! - **Forget.** `forget_deck` revokes every deck key: one transaction in
//!   the store, and the link layer closes the links within 1 s (G5).

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use fleetdeck_core::Secret;
use fleetdeck_core::identity::{Fingerprint, Identity, PeerKey};
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

use super::{StationLink, StationPeers, serve};
use crate::manager::SessionManager;
use crate::shell::{ShellConfig, Shells};
use crate::store::StoreError;

/// How long an invitation of `station invite` is valid (PLAN §8.5).
pub const INVITE_TTL: Duration = Duration::from_secs(15 * 60);

/// The scopes that a paired deck gets on this station: everything a deck
/// may call (ADR-004 §9.6). `coordinate` is a station → deck scope.
pub const DECK_SCOPES: [Scope; 4] = [Scope::Read, Scope::Drive, Scope::Approve, Scope::Admin];

/// Why a pairing operation failed.
#[derive(Debug, thiserror::Error)]
pub enum NodeError {
    #[error(
        "this station is already paired with deck {0}; run `fleetdeck station forget-deck` first"
    )]
    AlreadyPaired(String),
    #[error("invitation: {0}")]
    Invitation(#[from] PairError),
    #[error("an invitation of a station cannot pair two stations; use the deck's invitation")]
    NotADeck,
    #[error("pairing failed: {0}")]
    Pairing(String),
    #[error("listen address {addr}: {reason}")]
    Listen { addr: SocketAddr, reason: String },
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("the coordinator endpoint did not start: {0}")]
    Coordinator(std::io::Error),
}

/// The running link node of a station.
pub struct StationNode {
    ctx: Arc<StationLink>,
    invitations: Arc<Invitations>,
    /// Requested listen address → bound address.
    listening: tokio::sync::Mutex<BTreeMap<SocketAddr, SocketAddr>>,
    tasks: Mutex<Vec<JoinHandle<()>>>,
    backoff: BackoffPolicy,
    /// The loopback coordinator MCP endpoint (ADR-006 §5, DL-211). Its
    /// tool calls go to the current deck link.
    coordinator: crate::coordinator::Ingress,
    bindings: crate::coordinator::Bindings,
}

impl std::fmt::Debug for StationNode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StationNode")
            .field("node", &self.ctx.node)
            .finish_non_exhaustive()
    }
}

impl StationNode {
    /// Starts the node: pins from the store, listeners, dialers.
    ///
    /// # Errors
    /// A database error, or a listen address that cannot be bound (a
    /// public address is refused, D13).
    pub async fn start(
        manager: Arc<SessionManager>,
        identity: Arc<Identity>,
        name: String,
        listen: &[SocketAddr],
        backoff: BackoffPolicy,
    ) -> Result<Arc<Self>, NodeError> {
        let shells = Arc::new(Shells::new(ShellConfig::default()));
        Self::start_with_shells(manager, identity, name, listen, backoff, shells).await
    }

    /// [`StationNode::start`] with the shell service of `station.toml`.
    ///
    /// # Errors
    /// As [`StationNode::start`].
    pub async fn start_with_shells(
        manager: Arc<SessionManager>,
        identity: Arc<Identity>,
        name: String,
        listen: &[SocketAddr],
        backoff: BackoffPolicy,
        shells: Arc<Shells>,
    ) -> Result<Arc<Self>, NodeError> {
        Self::start_with_paste_dir(manager, identity, name, listen, backoff, shells, None).await
    }

    /// [`StationNode::start_with_shells`] with the paste directory of
    /// `files.write_temp` (DL-147). Without it the method answers
    /// `unsupported`.
    ///
    /// # Errors
    /// As [`StationNode::start`].
    pub async fn start_with_paste_dir(
        manager: Arc<SessionManager>,
        identity: Arc<Identity>,
        name: String,
        listen: &[SocketAddr],
        backoff: BackoffPolicy,
        shells: Arc<Shells>,
        paste_dir: Option<std::path::PathBuf>,
    ) -> Result<Arc<Self>, NodeError> {
        let store = manager.store().clone();
        let peers = StationPeers::load(store.clone()).await?;
        let invitations = Invitations::new();
        let node = NodeConfig::new(
            identity,
            LocalHello::new(Role::Station, store.incarnation().clone()),
            Arc::new(Epochs::sqlite(store.db())),
        )
        .with_store(Arc::clone(&peers))
        .with_invitations(Arc::clone(&invitations));
        let paired = peers.pin_all(&node.pins).await?;
        let bindings = manager.policy().coordinator_bindings().clone();
        // The durable coordinator outbox (ADR-006 §6.5, DL-218).
        let forward = crate::coordinator::LinkForward::with_outbox(store.clone(), bindings.clone());
        let coordinator = crate::coordinator::Ingress::start(
            bindings.clone(),
            Arc::new(forward.clone()),
            crate::coordinator::Limits::default(),
        )
        .await
        .map_err(NodeError::Coordinator)?;
        // A coordinator start hands this URL to its child (DL-216).
        bindings.set_endpoint(coordinator.url());
        let this = Arc::new(Self {
            ctx: Arc::new(StationLink {
                manager,
                node,
                peers,
                name,
                shells,
                coordinator: forward,
                paste_dir,
            }),
            invitations,
            listening: tokio::sync::Mutex::default(),
            tasks: Mutex::default(),
            backoff,
            coordinator,
            bindings,
        });
        this.spawn(tokio::spawn(this.ctx.coordinator.clone().run()));
        // Probe the providers once at start: the coordinator handoff check
        // needs their versions (DL-219).
        let manager = Arc::clone(&this.ctx.manager);
        this.spawn(tokio::spawn(async move {
            manager.providers().await;
        }));
        // The holds of the terminals of an earlier process end or stay
        // before any board can ask (DL-140).
        crate::shell::handover::recover_holds(&store).await?;
        if let Some(exits) = this.ctx.shells.take_exits() {
            this.spawn(crate::shell::handover::spawn_exit_release(
                store.clone(),
                exits,
            ));
        }
        let mut addrs = listen.to_vec();
        addrs.extend(store.listen_addrs().await?);
        for addr in addrs {
            this.listen_on(addr).await?;
        }
        for peer in paired {
            let Some(url) = &peer.dial_url else { continue };
            match url.parse::<SocketAddr>() {
                Ok(addr) => this.dial(addr, peer.key),
                Err(_) => tracing::warn!(url = %url, "a stored deck address does not parse"),
            }
        }
        Ok(this)
    }

    /// The shared link context (tests and the deck-less paths).
    #[must_use]
    pub fn link(&self) -> &Arc<StationLink> {
        &self.ctx
    }

    /// The MCP URL of the loopback coordinator endpoint.
    #[must_use]
    pub fn coordinator_url(&self) -> String {
        self.coordinator.url()
    }

    /// The coordinator bindings that the endpoint authenticates against.
    #[must_use]
    pub fn coordinator_bindings(&self) -> &crate::coordinator::Bindings {
        &self.bindings
    }

    /// The bound addresses of the listeners.
    pub async fn listen_addrs(&self) -> Vec<SocketAddr> {
        self.listening.lock().await.values().copied().collect()
    }

    fn spawn(&self, task: JoinHandle<()>) {
        let mut tasks = self.tasks.lock().unwrap_or_else(PoisonError::into_inner);
        tasks.retain(|t| !t.is_finished());
        tasks.push(task);
    }

    /// Listens on `addr` unless a listener for it runs; returns the bound
    /// address.
    async fn listen_on(&self, addr: SocketAddr) -> Result<SocketAddr, NodeError> {
        let mut listening = self.listening.lock().await;
        if let Some(bound) = listening.get(&addr) {
            return Ok(*bound);
        }
        let listener = node::listen(self.ctx.node.clone(), addr)
            .await
            .map_err(|e| NodeError::Listen {
                addr,
                reason: e.to_string(),
            })?;
        let bound = listener.local_addr;
        tracing::info!(addr = %bound, "station listens for its deck");
        listening.insert(addr, bound);
        self.spawn(tokio::spawn(accept_loop(Arc::clone(&self.ctx), listener)));
        Ok(bound)
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
                        tracing::info!(deck = %peer.fingerprint().short(), %reason, "deck dialer stopped");
                        break;
                    }
                    DialerEvent::Failed { .. } | DialerEvent::Lost { .. } => {}
                }
            }
            dialer.abort();
        }));
    }

    /// The key fingerprint of the paired deck, if any.
    ///
    /// # Errors
    /// A database error.
    pub async fn paired_deck(&self) -> Result<Option<Fingerprint>, NodeError> {
        let keys = self.ctx.peers.store().paired_keys().await?;
        Ok(keys
            .into_iter()
            .find(|k| k.role == Role::Deck)
            .map(|k| k.key.fingerprint()))
    }

    async fn refuse_if_paired(&self) -> Result<(), NodeError> {
        match self.paired_deck().await? {
            Some(fp) => Err(NodeError::AlreadyPaired(fp.short())),
            None => Ok(()),
        }
    }

    /// `station invite --listen <addr>`: listens on `addr` (now and after
    /// restarts) and opens a single-use invitation for a deck that dials
    /// this station. Returns the invitation text.
    ///
    /// # Errors
    /// [`NodeError`]: a deck is paired, the address is refused, or a
    /// database error.
    pub async fn invite(&self, listen: SocketAddr) -> Result<Secret<String>, NodeError> {
        self.refuse_if_paired().await?;
        let bound = self.listen_on(listen).await?;
        self.ctx.peers.store().add_listen_addr(bound).await?;
        let invitation = self.invitations.create(
            bound,
            self.ctx.node.identity.fingerprint(),
            INVITE_TTL,
            DECK_SCOPES,
        )?;
        Ok(invitation.reveal())
    }

    /// `station join`: pairs with the deck of `text`, stores its key, grants
    /// and address in one transaction, and starts dialing it. Returns the
    /// deck's fingerprint.
    ///
    /// # Errors
    /// [`NodeError`]: a deck is paired already, the invitation is not
    /// valid, or the pairing failed.
    pub async fn join(&self, text: &Secret<String>) -> Result<Fingerprint, NodeError> {
        self.refuse_if_paired().await?;
        let invitation = Invitation::parse(text)?;
        if invitation.fingerprint == self.ctx.node.identity.fingerprint() {
            return Err(NodeError::NotADeck);
        }
        let fingerprint = invitation.fingerprint;
        self.ctx
            .peers
            .dial_after_pairing(fingerprint, invitation.addr);
        match fleetdeck_link::pair(&self.ctx.node, &invitation, DECK_SCOPES).await {
            Ok(paired) => {
                tracing::info!(deck = %fingerprint.short(), "paired with the deck");
                self.dial(invitation.addr, paired.key);
                Ok(fingerprint)
            }
            Err(e) => {
                self.ctx.peers.pairing_failed(&fingerprint);
                Err(NodeError::Pairing(describe(&e)))
            }
        }
    }

    /// `station forget-deck`: revokes every deck key and closes its links.
    /// Returns how many keys were revoked.
    ///
    /// # Errors
    /// A database error while the keys are read.
    pub async fn forget_deck(&self) -> Result<usize, NodeError> {
        let keys = self.ctx.peers.store().paired_keys().await?;
        let mut count = 0;
        for key in keys.into_iter().filter(|k| k.role == Role::Deck) {
            revoke(&self.ctx.node, &key.key).await;
            tracing::info!(deck = %key.key.fingerprint().short(), "deck forgotten");
            count += 1;
        }
        Ok(count)
    }

    /// Stops the listeners and the dialers.
    pub fn shutdown(&self) {
        let tasks = std::mem::take(&mut *self.tasks.lock().unwrap_or_else(PoisonError::into_inner));
        for task in tasks {
            task.abort();
        }
    }
}

fn describe(e: &LinkError) -> String {
    match e.reason() {
        Some(reason) => format!("{reason} ({e})"),
        None => e.to_string(),
    }
}

async fn accept_loop(ctx: Arc<StationLink>, mut listener: LinkListener) {
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
