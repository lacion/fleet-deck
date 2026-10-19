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
use fleetdeck_link::node::{self, LinkListener, ListenerKind, NodeConfig, TailnetLimits};
use fleetdeck_link::pair_hub::{HubConfig, PairApprover, PairRequests};
use fleetdeck_link::pairing::{Invitation, Invitations, PairError};
use fleetdeck_link::reconnect::{BackoffPolicy, DialerEvent, spawn_dialer};
use fleetdeck_link::report_accept::ReportSink;
use fleetdeck_link::revoke::revoke;
use fleetdeck_link::tls::Role;
use fleetdeck_link::ws::LinkError;
use fleetdeck_link::{Epochs, start};
use fleetdeck_proto::link::Scope;
use tokio::task::JoinHandle;

use super::{DeckLink, serve};
use crate::peers::{DeckPeers, Expected};
use crate::provision_reports::ProvisionReports;
use crate::store::{DeckError, DeckStore, Direction, StationRecord};

/// How often the command router expires and resends commands (G3).
pub const ROUTER_TICK: Duration = Duration::from_secs(1);

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
    /// A revocation whose store write failed (NV-15). The key is unpinned
    /// in memory, but the station stays in the store.
    #[error(
        "the revocation was not stored: {0}. The key is unpinned until the deck restarts. Run the command again."
    )]
    Revoke(String),
}

/// What a test changes in the link node: the limits of the pairing window
/// and of the tailnet listeners, and the kind of every listener (a loopback
/// socket that has the rules of a tailnet listener). The default is the
/// production behaviour.
#[derive(Debug, Clone, Default)]
pub struct NodeOptions {
    pub hub: HubConfig,
    pub tailnet: Option<TailnetLimits>,
    pub listener_kind: Option<ListenerKind>,
    /// Listen addresses that the automatic tailnet mode added (design 14.4).
    /// Each one is checked and bound after the `link_listen` addresses; an
    /// address that is not local, or that cannot be bound, is skipped with a
    /// warning ([`DeckNode::skipped`]) and never stops the start.
    pub optional_listen: Vec<SocketAddr>,
}

/// The `io::ErrorKind` of a failed bind; `Other` for an error that is not an
/// I/O error.
fn link_error_kind(e: &LinkError) -> std::io::ErrorKind {
    match e {
        LinkError::Net(fleetdeck_link::net::NetError::Io(io)) => io.kind(),
        _ => std::io::ErrorKind::Other,
    }
}

/// The running link node of the deck.
pub struct DeckNode {
    ctx: Arc<DeckLink>,
    invitations: Arc<Invitations>,
    pair_requests: Arc<PairRequests>,
    provision_reports: Arc<ProvisionReports>,
    listening: Vec<SocketAddr>,
    skipped: Vec<(SocketAddr, String)>,
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
        Self::start_with(
            store,
            identity,
            link_listen,
            backoff,
            NodeOptions::default(),
        )
        .await
    }

    /// [`DeckNode::start`] with `options`.
    ///
    /// # Errors
    /// As [`DeckNode::start`].
    pub async fn start_with(
        store: DeckStore,
        identity: Arc<Identity>,
        link_listen: &[SocketAddr],
        backoff: BackoffPolicy,
        options: NodeOptions,
    ) -> Result<Arc<Self>, NodeError> {
        let peers = DeckPeers::load(store.clone()).await?;
        let invitations = Invitations::new();
        let mut node = NodeConfig::new(
            Arc::clone(&identity),
            LocalHello::new(Role::Deck, store.incarnation().clone()),
            Arc::new(Epochs::sqlite(store.db())),
        )
        .with_store(Arc::clone(&peers))
        .with_invitations(Arc::clone(&invitations));
        // The pairing request window and its pending requests (DL-178): in
        // memory only; a restart closes the window.
        let pair_requests = PairRequests::new(
            identity.public_key(),
            STATION_SCOPES.into_iter().collect(),
            Arc::clone(&node.pins),
            Arc::clone(&peers) as Arc<dyn PairApprover>,
            options.hub,
        );
        node = node.with_pair_requests(Arc::clone(&pair_requests));
        // The setup reports of unpaired keys (BG-3): the reporting window is
        // in memory only, and it is separate from the pairing window.
        let provision_reports = ProvisionReports::new(store.clone());
        node = node.with_provision_reports(Arc::clone(&provision_reports) as Arc<dyn ReportSink>);
        if let Some(limits) = options.tailnet {
            node.tailnet = limits;
        }
        node.listener_kind = options.listener_kind;
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
        let mut skipped = Vec::new();
        for addr in &options.optional_listen {
            let outcome = match crate::tailnet_auto::unusable(*addr) {
                Some(reason) => Err(reason),
                None => node::listen(ctx.node.clone(), *addr)
                    .await
                    .map_err(|e| crate::tailnet_auto::bind_failed(link_error_kind(&e))),
            };
            match outcome {
                Ok(listener) => {
                    tracing::info!(addr = %listener.local_addr, "deck listens for stations");
                    listeners.push(listener);
                }
                Err(reason) => {
                    tracing::warn!(%addr, %reason, "an automatic tailnet link address is not used");
                    skipped.push((*addr, reason));
                }
            }
        }
        let this = Arc::new(Self {
            ctx,
            invitations,
            pair_requests,
            provision_reports,
            listening: listeners.iter().map(|l| l.local_addr).collect(),
            skipped,
            tasks: Mutex::default(),
            backoff,
        });
        for listener in listeners {
            this.spawn(tokio::spawn(accept_loop(Arc::clone(&this.ctx), listener)));
        }
        for st in stations {
            this.dial_stored(&st);
        }
        let ctx = Arc::clone(&this.ctx);
        this.spawn(tokio::spawn(async move {
            let mut every = tokio::time::interval(ROUTER_TICK);
            every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                every.tick().await;
                ctx.tick().await;
            }
        }));
        Ok(this)
    }

    /// The pairing request window and the pending requests.
    #[must_use]
    pub fn pair_requests(&self) -> &Arc<PairRequests> {
        &self.pair_requests
    }

    /// The reporting window and the setup reports of unpaired keys.
    #[must_use]
    pub fn provision_reports(&self) -> &Arc<ProvisionReports> {
        &self.provision_reports
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

    /// The automatic addresses that were not used, with the reason.
    #[must_use]
    pub fn skipped(&self) -> &[(SocketAddr, String)] {
        &self.skipped
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
    /// Creates the owner's coordinator grant (ADR-006 §4). Every station that
    /// the grant names gets the link scope `coordinate` in the same
    /// transaction, and the link's grant cache follows at once (DL-211).
    ///
    /// # Errors
    /// An invalid grant or a database error.
    pub async fn create_grant(
        &self,
        grant: crate::store::NewGrant,
    ) -> Result<crate::store::Grant, DeckError> {
        let created = self.ctx.store.create_grant(grant).await?;
        self.ctx.peers.refresh_changed().await?;
        Ok(created)
    }

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
    /// [`NodeError::UnknownStation`], a database error, or
    /// [`NodeError::Revoke`] when the revocation was not stored (NV-15).
    pub async fn remove_station(&self, name_or_id: &str) -> Result<StationRecord, NodeError> {
        let st = self
            .ctx
            .store
            .find_station(name_or_id.to_owned())
            .await?
            .ok_or_else(|| NodeError::UnknownStation(name_or_id.to_owned()))?;
        revoke(&self.ctx.node, &st.key)
            .await
            .map_err(NodeError::Revoke)?;
        tracing::info!(station = %st.name, "station removed");
        Ok(st)
    }

    /// Stops the listeners and the dialers and closes every link. A pairing
    /// request that waits ends with `cancelled`.
    pub fn shutdown(&self) {
        self.pair_requests.cancel_all_now();
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
