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

use std::collections::{BTreeMap, BTreeSet};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use fleetdeck_core::Secret;
use fleetdeck_core::identity::{Fingerprint, Identity, PeerKey};
use fleetdeck_link::hello::LocalHello;
use fleetdeck_link::node::{self, LinkListener, NodeConfig};
use fleetdeck_link::pairing::{Invitation, Invitations, PairError, PairedPeer};
use fleetdeck_link::reconnect::{BackoffPolicy, DialerEvent, spawn_dialer};
use fleetdeck_link::retry_file;
use fleetdeck_link::revoke::revoke;
use fleetdeck_link::tailnet_join::{JoinError, JoinPlan, Joined, drive};
use fleetdeck_link::tailnet_target;
use fleetdeck_link::tls::Role;
use fleetdeck_link::ws::LinkError;
use fleetdeck_link::{Epochs, start};
use fleetdeck_proto::link::Scope;
use tokio::task::JoinHandle;

use super::{StationLink, StationPeers, serve};
use crate::fleet_git_runtime::FleetGitRuntime;
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
    /// A revocation whose store write failed (NV-15). The key is unpinned
    /// in memory, but the pairing stays in the store.
    #[error(
        "the revocation was not stored: {0}. The key is unpinned until the station restarts. Run the command again."
    )]
    Revoke(String),
    #[error("the coordinator endpoint did not start: {0}")]
    Coordinator(std::io::Error),
    /// `station join --tailnet` did not finish; `code` is the stable code
    /// of the refusal.
    #[error("{code}: {detail}")]
    Tailnet { code: String, detail: String },
}

fn tailnet_refusal(code: &str, detail: impl Into<String>) -> NodeError {
    NodeError::Tailnet {
        code: code.to_owned(),
        detail: detail.into(),
    }
}

/// The result of `station join --tailnet`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TailnetResult {
    /// No fingerprint was known: this is the key the deck showed. Nothing
    /// was written or sent; the person confirms it and runs the command
    /// again with the fingerprint.
    Observed(Fingerprint),
    /// The owner approved: the deck is paired.
    Paired {
        /// The DNS name (or host name) of the deck device.
        device: String,
        deck: Fingerprint,
    },
}

/// What `station join --tailnet` asks of the station.
#[derive(Debug, Clone)]
pub struct TailnetJoin {
    /// The deck as the person typed it: a DNS name, a short name or an
    /// address. It must be a peer in `tailscale status`.
    pub host: String,
    pub port: u16,
    /// The deck link key fingerprint that the person confirmed. Without
    /// one, the station only observes the key of the deck (see
    /// [`TailnetResult::Observed`]).
    pub deck_fingerprint: Option<Fingerprint>,
    /// The scopes this station gives to the deck.
    pub scopes: BTreeSet<Scope>,
    /// The requested name of this station on the deck.
    pub name: String,
    /// The `tailscale` program of this station.
    pub cli: fleetdeck_core::tailscale::TailscaleCli,
    /// Tests only: dial these addresses instead of the tailnet addresses of
    /// the peer (a test host has no tailnet address). The host is still
    /// checked against `tailscale status`. The local socket never sets it.
    #[doc(hidden)]
    pub dial: Option<Vec<SocketAddr>>,
}

/// How the waiting part of a tailnet join ended.
enum JoinStep {
    /// The probe of the deck key (no fingerprint was given).
    Observed(TailnetResult),
    /// The deck approved and the signed answer is verified.
    Approved {
        joined: Joined,
        device: String,
        deck_fingerprint: Fingerprint,
        dir: std::path::PathBuf,
    },
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
    /// The station socket of the MCP bridge of the shim host; it lives as
    /// long as the endpoint (DL-194).
    _bridge_socket: Option<crate::coordinator::BridgeSocket>,
    bindings: crate::coordinator::Bindings,
    /// The station data directory; it holds the retry file of a tailnet
    /// join. Set by the local socket, which knows the directory.
    data_dir: std::sync::OnceLock<std::path::PathBuf>,
    /// One tailnet join at a time.
    join_gate: tokio::sync::Mutex<()>,
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
        Self::start_with_dirs(
            manager, identity, name, listen, backoff, shells, paste_dir, None,
        )
        .await
    }

    /// [`StationNode::start_with_paste_dir`] with the artifact directory
    /// (DL-165). Without it the artifact methods answer `unsupported` and
    /// `publish_artifact` fails.
    ///
    /// # Errors
    /// As [`StationNode::start`].
    #[allow(clippy::too_many_arguments)]
    pub async fn start_with_dirs(
        manager: Arc<SessionManager>,
        identity: Arc<Identity>,
        name: String,
        listen: &[SocketAddr],
        backoff: BackoffPolicy,
        shells: Arc<Shells>,
        paste_dir: Option<std::path::PathBuf>,
        artifacts_dir: Option<std::path::PathBuf>,
    ) -> Result<Arc<Self>, NodeError> {
        // A node that `serve` did not make clones with plain Git options.
        let runtime = Arc::new(FleetGitRuntime::new(Arc::new(
            crate::workspace::GitOptions::isolated,
        )));
        Self::start_with_fleet_git(
            manager,
            identity,
            name,
            listen,
            backoff,
            shells,
            paste_dir,
            artifacts_dir,
            runtime,
        )
        .await
    }

    /// [`StationNode::start_with_dirs`] with the Fleet Git runtime that
    /// `serve` made (FG-5).
    ///
    /// # Errors
    /// As [`StationNode::start`].
    #[allow(clippy::too_many_arguments)]
    pub async fn start_with_fleet_git(
        manager: Arc<SessionManager>,
        identity: Arc<Identity>,
        name: String,
        listen: &[SocketAddr],
        backoff: BackoffPolicy,
        shells: Arc<Shells>,
        paste_dir: Option<std::path::PathBuf>,
        artifacts_dir: Option<std::path::PathBuf>,
        fleet_git: Arc<FleetGitRuntime>,
    ) -> Result<Arc<Self>, NodeError> {
        if let Some(dir) = artifacts_dir {
            manager.set_artifacts_dir(dir, manager.private_paths());
        }
        manager.set_fleet_git_runtime(Arc::clone(&fleet_git));
        // The trash decisions and the file work read the open shells
        // (DL-150).
        manager.set_shells(Arc::clone(&shells));
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
        let bindings = manager.coordinator_bindings().clone();
        // The durable coordinator outbox (ADR-006 §6.5, DL-218).
        let forward = crate::coordinator::LinkForward::with_outbox(store.clone(), bindings.clone())
            .with_bindings(bindings.clone());
        // The session's own tools are answered by the station (DL-165).
        let fleet = Arc::new(crate::artifacts::fleet::StationFleet::new(
            store.clone(),
            manager.artifacts().cloned(),
        ));
        let coordinator = crate::coordinator::Ingress::start_with(
            bindings.clone(),
            Arc::new(forward.clone()),
            Some(fleet.clone()),
            crate::coordinator::Limits::default(),
        )
        .await
        .map_err(NodeError::Coordinator)?;
        let bridge_socket = crate::coordinator::BridgeSocket::start_for(
            &bindings,
            Arc::new(forward.clone()),
            Some(fleet),
        )
        .await;
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
                fleet_git,
            }),
            invitations,
            listening: tokio::sync::Mutex::default(),
            tasks: Mutex::default(),
            backoff,
            coordinator,
            _bridge_socket: bridge_socket,
            bindings,
            data_dir: std::sync::OnceLock::new(),
            join_gate: tokio::sync::Mutex::new(()),
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

    /// Tells the node the station data directory (the first call wins).
    pub fn set_data_dir(&self, dir: &std::path::Path) {
        let _ = self.data_dir.set(dir.to_path_buf());
    }

    fn data_dir(&self) -> Result<&std::path::Path, NodeError> {
        self.data_dir
            .get()
            .map(std::path::PathBuf::as_path)
            .ok_or_else(|| tailnet_refusal("unsupported", "this station has no data directory"))
    }

    /// `station join --tailnet`: asks the deck of `join` to pair, waits for
    /// its owner (at most 11 minutes), checks the signed answer, stores the
    /// deck pin, the grants and the dial address in **one** transaction and
    /// starts dialing the deck. Returns the name of the deck device and its
    /// key fingerprint.
    ///
    /// The retry file `pair-pending.json` holds the request from before it
    /// is sent until the pin is stored; the same command resumes it.
    ///
    /// `gone` resolves when the caller went away. It ends the join only
    /// while the station waits for the deck (the status, the probe and the
    /// request); the steps after a verified approval never stop for it.
    ///
    /// # Errors
    /// [`NodeError`]: a deck is paired already, the host is not a peer of
    /// `tailscale status`, the caller went away (`client_gone`), or the
    /// request ended with a code ([`NodeError::Tailnet`]).
    pub async fn join_tailnet(
        &self,
        join: &TailnetJoin,
        gone: impl std::future::Future<Output = ()> + Send,
    ) -> Result<TailnetResult, NodeError> {
        let Ok(_one) = self.join_gate.try_lock() else {
            return Err(tailnet_refusal(
                "busy",
                "another `station join --tailnet` is running",
            ));
        };
        tokio::pin!(gone);
        let step = tokio::select! {
            r = self.join_until_approved(join) => r?,
            () = &mut gone => {
                return Err(tailnet_refusal(
                    "client_gone",
                    "the command ended before the deck answered",
                ));
            }
        };
        let (joined, device, deck_fingerprint, dir) = match step {
            JoinStep::Observed(result) => return Ok(result),
            JoinStep::Approved {
                joined,
                device,
                deck_fingerprint,
                dir,
            } => (joined, device, deck_fingerprint, dir),
        };
        // The approval is verified: nothing below stops for `gone`.
        let deck = joined.approved.deck;
        if !deck.fingerprint().matches(&deck_fingerprint) {
            // TLS pinned the fingerprint; this is a second check.
            return Err(tailnet_refusal(
                "fingerprint_mismatch",
                "the deck key is not the confirmed fingerprint",
            ));
        }
        // The pin, the grants and the dial address: one local transaction.
        self.ctx
            .peers
            .dial_after_pairing(deck.fingerprint(), joined.addr);
        let paired = PairedPeer {
            key: deck,
            role: Role::Deck,
            scopes: joined.scopes,
        };
        if let Err(detail) = self.ctx.node.sink.paired(&paired).await {
            self.ctx.peers.pairing_failed(&deck.fingerprint());
            return Err(tailnet_refusal("pairing_failed", detail));
        }
        self.ctx.node.pins.pin(deck, Role::Deck);
        if let Err(e) = retry_file::remove(&dir) {
            tracing::warn!(error = %e, "the retry file of the pairing request stays");
        }
        tracing::info!(deck = %deck.fingerprint().short(), "paired with the deck by request");
        self.dial(joined.addr, deck);
        Ok(TailnetResult::Paired {
            device,
            deck: deck.fingerprint(),
        })
    }

    /// The part of the join that waits for the deck: the checks, the
    /// probe, and the request up to the signed approval.
    async fn join_until_approved(&self, join: &TailnetJoin) -> Result<JoinStep, NodeError> {
        self.refuse_if_paired().await?;
        let dir = self.data_dir()?.to_path_buf();
        let status = join.cli.status().await.map_err(|e| {
            tailnet_refusal(
                "tailscale_unavailable",
                format!("Tailscale is not available: {}", e.reason()),
            )
        })?;
        let target = tailnet_target::find(&status, &join.host)
            .map_err(|e| tailnet_refusal("not_in_tailnet", e.to_string()))?;
        let addrs: Vec<SocketAddr> = join.dial.clone().unwrap_or_else(|| {
            target
                .ips
                .iter()
                .map(|ip| SocketAddr::new(*ip, join.port))
                .collect()
        });
        // Without a fingerprint the station only observes the key of the
        // deck; the person confirms it and runs the join again.
        let Some(deck_fingerprint) = join.deck_fingerprint else {
            return self.observe(&addrs).await.map(JoinStep::Observed);
        };
        let plan = JoinPlan {
            host: join.host.clone(),
            port: join.port,
            deck: deck_fingerprint,
            name: join.name.clone(),
            scopes: join.scopes.clone(),
            addrs,
            dir: dir.clone(),
            wait: fleetdeck_link::pair_dial::WAIT_FOR_OWNER,
        };
        let joined = drive(&self.ctx.node, &plan).await.map_err(|e| match &e {
            JoinError::Link(_) => tailnet_refusal(e.code(), describe_join(&e)),
            _ => tailnet_refusal(e.code(), e.to_string()),
        })?;
        Ok(JoinStep::Approved {
            joined,
            device: target.name,
            deck_fingerprint,
            dir,
        })
    }

    /// The observation-only probe: the key of the deck, from a TLS connect.
    /// It writes and sends nothing.
    async fn observe(&self, addrs: &[SocketAddr]) -> Result<TailnetResult, NodeError> {
        let mut last = String::from("no address");
        for addr in addrs {
            match fleetdeck_link::net::observe_key(*addr).await {
                Ok(key) => return Ok(TailnetResult::Observed(key.fingerprint())),
                Err(e) => last = format!("{addr}: {e}"),
            }
        }
        Err(tailnet_refusal(
            "probe_failed",
            format!("the deck link key could not be observed ({last})"),
        ))
    }

    /// `station join --tailnet --cancel`: removes the retry file. Returns
    /// whether there was one. A request that already reached the deck is not
    /// withdrawn; the deck forgets it when the connection ends.
    ///
    /// # Errors
    /// [`NodeError::Tailnet`] when the file can not be removed.
    pub fn cancel_tailnet(&self) -> Result<bool, NodeError> {
        let dir = self.data_dir()?;
        retry_file::remove(dir).map_err(|e| tailnet_refusal("retry_file", e.to_string()))
    }

    /// `station forget-deck`: revokes every deck key and closes its links.
    /// Returns how many keys were revoked.
    ///
    /// # Errors
    /// A database error while the keys are read, or [`NodeError::Revoke`]
    /// when a revocation was not stored. Every key is revoked in memory
    /// anyway; the error holds the first store error (NV-15).
    pub async fn forget_deck(&self) -> Result<usize, NodeError> {
        let keys = self.ctx.peers.store().paired_keys().await?;
        let mut count = 0;
        let mut first_error = None;
        for key in keys.into_iter().filter(|k| k.role == Role::Deck) {
            match revoke(&self.ctx.node, &key.key).await {
                Ok(_) => {
                    tracing::info!(deck = %key.key.fingerprint().short(), "deck forgotten");
                    count += 1;
                }
                Err(e) => {
                    first_error.get_or_insert(e);
                }
            }
        }
        match first_error {
            Some(e) => Err(NodeError::Revoke(e)),
            None => Ok(count),
        }
    }

    /// Stops the listeners and the dialers.
    pub fn shutdown(&self) {
        let tasks = std::mem::take(&mut *self.tasks.lock().unwrap_or_else(PoisonError::into_inner));
        for task in tasks {
            task.abort();
        }
    }
}

fn describe_join(e: &JoinError) -> String {
    match e {
        JoinError::Link(l) => describe(l),
        other => other.to_string(),
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
