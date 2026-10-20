//! Coordinator credentials and bindings (ADR-006 §5.3–§5.5).
//!
//! The station keeps only the SHA-256 digest of a credential. The raw value
//! lives in a [`Secret`] until it is put into the provider child's
//! environment; it is never stored, logged or sent.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use fleetdeck_core::secret::Secret;
use fleetdeck_proto::SessionId;
use fleetdeck_proto::command::{GrantKind, GrantScope};
use fleetdeck_proto::event::ProviderKind;
use fleetdeck_proto::mutation::CoordinatorBinding;
use ring::rand::{SecureRandom, SystemRandom};
use tokio::sync::Semaphore;

use super::fence::Fence;
use super::forward::Transport;
pub use super::tool_class::ToolClass;

/// The prefix of a coordinator credential.
pub const TOKEN_PREFIX: &str = "fdc1_";
/// The environment variable that carries the credential to the provider.
pub const TOKEN_ENV: &str = "FLEETDECK_COORDINATOR_TOKEN";
/// Concurrent requests of one binding, including waits for the deck (§5.1).
pub const BINDING_CONCURRENCY: usize = 8;
/// Requests per second of one binding (§5.1).
pub const BINDING_RATE: u32 = 20;

/// The state of a binding (§5.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BindingState {
    /// Minted; the native identity of the session is not known yet.
    Pending,
    Active,
    Expired,
    Revoked,
}

/// One coordinator binding of a session.
#[derive(Debug, Clone)]
pub struct Binding {
    pub binding_id: String,
    pub grant_id: String,
    pub session_id: SessionId,
    pub provider_instance: String,
    pub native_id: Option<String>,
    pub generation: u32,
    pub classes: BTreeSet<ToolClass>,
    /// The reach of the grant (ADR-008 §3).
    pub scope: GrantScope,
    pub state: BindingState,
    /// The cached control skill (§7.1), served as MCP `instructions`.
    pub control_skill: String,
    /// The kind of the grant (MCP P4a §4). A fleet-only binding has no
    /// grant: it is `Coordinator` and the field is unused.
    pub kind: GrantKind,
}

/// The identity of an authenticated caller. It comes from the binding only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Caller {
    pub binding_id: String,
    pub grant_id: String,
    pub session_id: SessionId,
    pub provider_instance: String,
    pub native_id: String,
    pub generation: u32,
    /// R13 P7b §4: the binding has the station-internal `input_any` class
    /// (the scope `any_window`). The desktop forward reads it at each call.
    pub input_any: bool,
}

/// A new credential: the raw value (secret) and its digest.
pub fn mint() -> Result<(Secret<String>, String), ring::error::Unspecified> {
    let mut bytes = [0u8; 32];
    SystemRandom::new().fill(&mut bytes)?;
    let token = format!("{TOKEN_PREFIX}{}", base64url(&bytes));
    let d = digest(&token);
    Ok((Secret::new(token), d))
}

/// The lowercase hex SHA-256 of a presented credential.
pub fn digest(token: &str) -> String {
    let d = ring::digest::digest(&ring::digest::SHA256, token.as_bytes());
    d.as_ref().iter().map(|b| format!("{b:02x}")).collect()
}

/// `fdc1_` and 43 base64url characters.
pub fn well_formed(token: &str) -> bool {
    token.strip_prefix(TOKEN_PREFIX).is_some_and(|rest| {
        rest.len() == 43
            && rest
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    })
}

fn base64url(bytes: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, b)| n | (u32::from(*b) << (16 - 8 * i)));
        for i in 0..=chunk.len() {
            let idx = (n >> (18 - 6 * i)) & 63;
            out.push(char::from(A[idx as usize]));
        }
    }
    out
}

struct Entry {
    binding: Binding,
    inflight: Arc<Semaphore>,
    window: (Instant, u32),
    /// The protocol version that the last successful `initialize` of this
    /// binding negotiated (MCP P1 §3.4): a header hint for the bridge path
    /// only. It is per binding, never shared, and gives no authority. The
    /// entry holds it, so removing or replacing the binding clears it.
    legacy_version: Option<String>,
}

/// The serial of a deck link and its request path.
type DeckLink = (u64, Arc<dyn Transport>);

/// The handoff check of a provider version (ADR-006 §5.4).
pub type HandoffCheck = fn(ProviderKind, Option<&str>) -> Result<(), &'static str>;

/// The bindings of a station, keyed by credential digest.
#[derive(Clone, Default)]
pub struct Bindings {
    inner: Arc<Mutex<HashMap<String, Entry>>>,
    /// The loopback URL of the ingress, once it listens (DL-216).
    endpoint: Arc<Mutex<Option<String>>>,
    /// `None`: [`fleetdeck_providers::coordinator::handoff`]. Tests set a
    /// verified check to drive the wiring before a live smoke passed.
    handoff: Option<HandoffCheck>,
    /// The probed versions of each provider family (DL-219): a start
    /// passes the handoff check only when every probed instance of its
    /// family has a verified version.
    versions: Arc<Mutex<HashMap<ProviderKind, Vec<String>>>>,
    /// The live `allow_fleet_control` of this station (ADR-008 §3), set at
    /// the start and at every publish of the policy. Off until set.
    fleet_control: Arc<std::sync::atomic::AtomicBool>,
    /// The current deck link: the serial of the link and its request path
    /// (`LinkForward` sets it on connect and clears it on disconnect).
    deck: Arc<Mutex<Option<DeckLink>>>,
    /// The MCP bridge of the shim host, once its socket listens (DL-194).
    bridge: Arc<Mutex<Option<BridgeEndpoint>>>,
    /// The invalidation fence of the shim host, once the start order
    /// created it (DL-194 §2.3). The direct host has none.
    fence: Arc<Mutex<Option<Arc<Fence>>>>,
    /// The digests of reattached bindings whose replay is not finished: the
    /// ingress answers "starting" for them (DL-194 §9.7).
    held: Arc<Mutex<HashSet<String>>>,
    /// The name and the platform of this station for the generated MCP
    /// instructions (MCP P3 §3.3): set at the start, the name again at every
    /// publish of the policy.
    pub(super) station: Arc<Mutex<Option<(String, String)>>>,
    /// The published fleet skill tree `<hash>/` (MCP P3 §4.1), once the
    /// station published it; `None` for no injection.
    pub(super) skill_dir: Arc<Mutex<Option<std::path::PathBuf>>>,
}

/// What a shim-hosted provider runs for its tools (DL-194 §2.2, §9.3): the
/// stable program path and the station socket of `fleetdeck mcp-bridge`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BridgeEndpoint {
    pub program: std::path::PathBuf,
    pub socket: std::path::PathBuf,
}

impl std::fmt::Debug for Bindings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Bindings")
            .field("count", &self.lock().len())
            .finish()
    }
}

/// The result of an authentication.
pub(crate) enum Auth {
    /// Missing, malformed, unknown, expired or revoked: one answer (§5.3).
    Unauthorized,
    /// The binding is over its rate or concurrency limit.
    Limited,
    /// The binding is held until the replay of its shim finished (DL-194
    /// §9.7).
    Starting,
    Ok {
        binding: Box<Binding>,
        permit: tokio::sync::OwnedSemaphorePermit,
    },
}

impl Binding {
    /// True for the binding of a session that has no coordinator grant: it
    /// holds only the session's own tools (DL-165). It has no grant id and
    /// never reaches the deck as a coordinator binding.
    #[must_use]
    pub fn is_fleet_only(&self) -> bool {
        self.grant_id.is_empty()
    }

    /// True for the binding of a fleet-control grant: scope `fleet`, or at
    /// least one of the classes `inspect`, `manage` and `answer`
    /// (ADR-008 §2).
    #[must_use]
    pub fn is_fleet_control(&self) -> bool {
        self.scope == GrantScope::Fleet || self.classes.iter().any(|c| c.is_fleet_control())
    }
}

impl Bindings {
    /// Records the loopback URL of the listening ingress.
    pub fn set_endpoint(&self, url: String) {
        *self
            .endpoint
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(url);
    }

    /// Records the live `allow_fleet_control` of the station (ADR-008 §3).
    pub fn set_fleet_control(&self, on: bool) {
        self.fleet_control
            .store(on, std::sync::atomic::Ordering::SeqCst);
    }

    /// The live `allow_fleet_control` of the station; off until set.
    #[must_use]
    pub fn fleet_control(&self) -> bool {
        self.fleet_control.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// A deck link came up: the request path for `coordinator.authority_check`
    /// (ADR-008 §6.2).
    pub fn set_deck(&self, serial: u64, transport: Arc<dyn Transport>) {
        *self
            .deck
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some((serial, transport));
    }

    /// The deck link of `serial` ended; a newer link stays.
    pub fn clear_deck(&self, serial: u64) {
        let mut deck = self
            .deck
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if deck.as_ref().is_some_and(|(s, _)| *s == serial) {
            *deck = None;
        }
    }

    /// The request path of the current deck link, if one is connected.
    #[must_use]
    pub fn deck(&self) -> Option<Arc<dyn Transport>> {
        self.deck
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .map(|(_, t)| Arc::clone(t))
    }

    /// True when the session holds an active binding of a grant (not the
    /// session-tools binding): `coordinator.policy_now` `bound` (ADR-008
    /// §6.2).
    #[must_use]
    pub fn holds_grant(&self, session_id: &SessionId) -> bool {
        self.lock().values().any(|e| {
            let b = &e.binding;
            &b.session_id == session_id && b.state == BindingState::Active && !b.is_fleet_only()
        })
    }

    /// True when the session holds an active binding that equals `expected`
    /// in its binding ID, grant, generation, provider instance and native ID
    /// (R13 P6 §5.6: `binding_matches` of `coordinator.policy_now`).
    #[must_use]
    pub fn binding_matches(&self, session: &SessionId, expected: &CoordinatorBinding) -> bool {
        self.lock().values().any(|e| {
            let b = &e.binding;
            &b.session_id == session
                && b.state == BindingState::Active
                && b.binding_id == expected.binding_id
                && b.grant_id == expected.grant_id
                && b.generation == expected.generation
                && b.provider_instance == expected.provider_instance.as_str()
                && b.native_id.as_deref() == Some(expected.native_id.as_str())
        })
    }

    /// True when the session holds an active binding with a mutating class
    /// on this station: it is a coordinator (ADR-008 §4.2 rule 3). A binding
    /// of a worker grant does not count (MCP P4a §4, W1).
    #[must_use]
    pub fn holds_mutating(&self, session_id: &SessionId) -> bool {
        self.lock().values().any(|e| {
            let b = &e.binding;
            &b.session_id == session_id
                && b.state == BindingState::Active
                && b.kind != GrantKind::Worker
                && b.classes.iter().any(|c| c.is_mutating())
        })
    }

    /// Records the MCP bridge of the shim host (DL-194).
    pub fn set_bridge(&self, bridge: BridgeEndpoint) {
        *self
            .bridge
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(bridge);
    }

    /// Clears the MCP bridge after its socket failed to start (DL-194).
    pub fn clear_bridge(&self) {
        *self
            .bridge
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    }

    /// The MCP bridge of the shim host, if its socket listens.
    #[must_use]
    pub fn bridge(&self) -> Option<BridgeEndpoint> {
        self.bridge
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Records the invalidation fence of the shim host (DL-194).
    pub fn set_fence(&self, fence: Arc<Fence>) {
        *self
            .fence
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(fence);
    }

    /// The invalidation fence, on the shim host only.
    #[must_use]
    pub fn fence(&self) -> Option<Arc<Fence>> {
        self.fence
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    fn held_set(&self) -> std::sync::MutexGuard<'_, HashSet<String>> {
        self.held
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Holds credentials: the ingress answers "starting" for them (DL-194
    /// §9.7).
    pub fn hold(&self, digests: impl IntoIterator<Item = String>) {
        self.held_set().extend(digests);
    }

    /// Releases held credentials.
    pub fn release<'a>(&self, digests: impl IntoIterator<Item = &'a String>) {
        let mut held = self.held_set();
        for d in digests {
            held.remove(d);
        }
    }

    /// True while the credential is held.
    #[must_use]
    pub fn is_held(&self, digest: &str) -> bool {
        self.held_set().contains(digest)
    }

    /// The credential digest of a binding (DL-194).
    #[must_use]
    pub fn digest_of(&self, binding_id: &str) -> Option<String> {
        self.lock()
            .iter()
            .find(|(_, e)| e.binding.binding_id == binding_id)
            .map(|(d, _)| d.clone())
    }

    /// The loopback URL of the ingress, if it listens.
    #[must_use]
    pub fn endpoint(&self) -> Option<String> {
        self.endpoint
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Whether the credential handoff is verified for a provider version:
    /// `Err` with the capability reason (`handoff_unverified`).
    ///
    /// # Errors
    /// The capability reason.
    pub fn handoff(
        &self,
        provider: ProviderKind,
        version: Option<&str>,
    ) -> Result<(), &'static str> {
        let check = self
            .handoff
            .unwrap_or(fleetdeck_providers::coordinator::handoff);
        if let Some(version) = version {
            return check(provider, Some(version));
        }
        let probed = self
            .versions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&provider)
            .cloned()
            .unwrap_or_default();
        if probed.is_empty() {
            return check(provider, None);
        }
        probed.iter().try_for_each(|v| check(provider, Some(v)))
    }

    /// Records the probed version of one provider instance (DL-219).
    pub fn record_version(&self, provider: ProviderKind, version: String) {
        let mut map = self
            .versions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let list = map.entry(provider).or_default();
        if !list.contains(&version) {
            list.push(version);
        }
    }

    /// The probed versions of one provider kind, in the order they were
    /// recorded (empty before the first probe).
    #[must_use]
    pub fn versions(&self, provider: ProviderKind) -> Vec<String> {
        self.versions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&provider)
            .cloned()
            .unwrap_or_default()
    }

    /// The same bindings with another handoff check (station tests).
    #[cfg(test)]
    #[must_use]
    pub(crate) fn with_handoff(mut self, check: HandoffCheck) -> Self {
        self.handoff = Some(check);
        self
    }

    /// True if a binding has this credential digest.
    #[must_use]
    pub fn has_digest(&self, digest: &str) -> bool {
        self.lock().contains_key(digest)
    }

    /// Add a binding for a credential digest. It replaces every entry with
    /// the same binding id (a resumed session gets a new credential under
    /// the old id, DL-165); the old credential then fails.
    pub fn insert(&self, digest: String, binding: Binding) {
        let entry = Entry {
            binding,
            inflight: Arc::new(Semaphore::new(BINDING_CONCURRENCY)),
            window: (Instant::now(), 0),
            legacy_version: None,
        };
        let mut map = self.lock();
        map.retain(|_, e| e.binding.binding_id != entry.binding.binding_id);
        map.insert(digest, entry);
    }

    /// Change the binding of a credential digest (DL-194). Returns false
    /// for an unknown digest.
    pub fn update_digest(&self, digest: &str, f: impl FnOnce(&mut Binding)) -> bool {
        match self.lock().get_mut(digest) {
            Some(e) => {
                f(&mut e.binding);
                true
            }
            None => false,
        }
    }

    /// The binding of a credential digest (DL-194).
    #[must_use]
    pub fn get_digest(&self, digest: &str) -> Option<Binding> {
        self.lock().get(digest).map(|e| e.binding.clone())
    }

    /// Change a binding. Returns false for an unknown binding id.
    pub fn update(&self, binding_id: &str, f: impl FnOnce(&mut Binding)) -> bool {
        let mut map = self.lock();
        match map
            .values_mut()
            .find(|e| e.binding.binding_id == binding_id)
        {
            Some(e) => {
                f(&mut e.binding);
                true
            }
            None => false,
        }
    }

    /// Bind the native identity: `pending` → `active` with generation 1,
    /// or revoke an active binding whose root identity changed (§5.6).
    pub fn native_identity(&self, binding_id: &str, native_id: &str) -> Option<BindingState> {
        let mut out = None;
        self.update(binding_id, |b| {
            match (b.state, b.native_id.as_deref()) {
                (BindingState::Pending, _) => {
                    b.native_id = Some(native_id.to_owned());
                    b.generation = 1;
                    b.state = BindingState::Active;
                }
                (BindingState::Active | BindingState::Expired, Some(old)) if old != native_id => {
                    b.state = BindingState::Revoked;
                }
                _ => {}
            }
            out = Some(b.state);
        });
        out
    }

    /// Binds the native identity of each binding of a session that is not
    /// revoked (DL-217). Returns the bindings whose state changed: a
    /// pending binding became `active` (generation 1), an active binding
    /// with another root identity became `revoked` (§5.6).
    pub fn session_native(&self, session_id: &SessionId, native_id: &str) -> Vec<Binding> {
        let ids: Vec<(String, BindingState)> = self
            .lock()
            .values()
            .filter(|e| {
                &e.binding.session_id == session_id && e.binding.state != BindingState::Revoked
            })
            .map(|e| (e.binding.binding_id.clone(), e.binding.state))
            .collect();
        ids.into_iter()
            .filter_map(|(id, before)| {
                let after = self.native_identity(&id, native_id)?;
                (after != before).then(|| self.get(&id)).flatten()
            })
            .collect()
    }

    /// Revokes every binding of a session at its process end and returns
    /// the bindings that were not revoked before (DL-217).
    pub fn end_session(&self, session_id: &SessionId) -> Vec<Binding> {
        let mut out = Vec::new();
        for e in self.lock().values_mut() {
            if &e.binding.session_id == session_id && e.binding.state != BindingState::Revoked {
                e.binding.state = BindingState::Revoked;
                out.push(e.binding.clone());
            }
        }
        out
    }

    /// Revoke every binding of a session (process exit, stop; §5.5).
    pub fn revoke_session(&self, session_id: &SessionId) {
        for e in self.lock().values_mut() {
            if &e.binding.session_id == session_id {
                e.binding.state = BindingState::Revoked;
            }
        }
    }

    /// A copy of the binding of `session_id` under `grant_id`.
    pub fn find(&self, session_id: &SessionId, grant_id: &str) -> Option<Binding> {
        self.lock()
            .values()
            .find(|e| &e.binding.session_id == session_id && e.binding.grant_id == grant_id)
            .map(|e| e.binding.clone())
    }

    /// The binding projection of a coordinator-capable snapshot (ADR-006
    /// §6.1): every binding with its fields and lifecycle state, sorted by
    /// binding id. No secret.
    #[must_use]
    pub fn projection(&self) -> Vec<fleetdeck_proto::CoordinatorBindingState> {
        use fleetdeck_proto::CoordinatorBindingPhase as Phase;
        let mut out: Vec<_> = self
            .lock()
            .values()
            .filter(|e| !e.binding.is_fleet_only())
            .map(|e| {
                let b = &e.binding;
                fleetdeck_proto::CoordinatorBindingState {
                    binding_id: b.binding_id.clone(),
                    grant_id: b.grant_id.clone(),
                    session_id: b.session_id.clone(),
                    generation: b.generation,
                    provider_instance: b.provider_instance.clone(),
                    native_id: b.native_id.clone(),
                    state: match b.state {
                        BindingState::Pending => Phase::Pending,
                        BindingState::Active => Phase::Active,
                        BindingState::Expired => Phase::Expired,
                        BindingState::Revoked => Phase::Revoked,
                    },
                }
            })
            .collect();
        out.sort_by(|a, b| a.binding_id.cmp(&b.binding_id));
        out
    }

    /// Records the protocol version that a successful `initialize` of the
    /// binding negotiated (MCP P1 §3.4). Returns false for an unknown
    /// binding id.
    pub(super) fn set_legacy_version(&self, binding_id: &str, version: &str) -> bool {
        match self
            .lock()
            .values_mut()
            .find(|e| e.binding.binding_id == binding_id)
        {
            Some(e) => {
                e.legacy_version = Some(version.to_owned());
                true
            }
            None => false,
        }
    }

    /// The protocol version that the last successful `initialize` of the
    /// binding negotiated, if any. A header hint for the bridge path only.
    pub(super) fn legacy_version(&self, binding_id: &str) -> Option<String> {
        self.lock()
            .values()
            .find(|e| e.binding.binding_id == binding_id)
            .and_then(|e| e.legacy_version.clone())
    }

    /// A copy of a binding by id.
    pub fn get(&self, binding_id: &str) -> Option<Binding> {
        self.lock()
            .values()
            .find(|e| e.binding.binding_id == binding_id)
            .map(|e| e.binding.clone())
    }

    pub(crate) fn authenticate(&self, header: Option<&str>) -> Auth {
        let Some(token) = header.and_then(|h| h.strip_prefix("Bearer ")) else {
            tracing::info!(case = "missing", "coordinator credential refused");
            return Auth::Unauthorized;
        };
        if !well_formed(token) {
            tracing::info!(case = "malformed", "coordinator credential refused");
            return Auth::Unauthorized;
        }
        let mut map = self.lock();
        let key = digest(token);
        let Some(e) = map.get_mut(&key) else {
            tracing::info!(case = "unknown", "coordinator credential refused");
            return Auth::Unauthorized;
        };
        // An ended binding is refused before the replay hold: a held
        // credential whose binding ended gets the uniform answer, never
        // "starting" (MCP P1 §3.4).
        if matches!(
            e.binding.state,
            BindingState::Revoked | BindingState::Expired
        ) {
            tracing::info!(case = "ended", binding = %e.binding.binding_id, "coordinator credential refused");
            return Auth::Unauthorized;
        }
        if self.is_held(&key) {
            return Auth::Starting;
        }
        let now = Instant::now();
        if now.duration_since(e.window.0).as_secs() >= 1 {
            e.window = (now, 0);
        }
        if e.window.1 >= BINDING_RATE {
            return Auth::Limited;
        }
        let Ok(permit) = e.inflight.clone().try_acquire_owned() else {
            return Auth::Limited;
        };
        e.window.1 += 1;
        Auth::Ok {
            binding: Box::new(e.binding.clone()),
            permit,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Entry>> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl Binding {
    /// The caller identity of an active binding.
    pub fn caller(&self) -> Option<Caller> {
        (self.state == BindingState::Active).then(|| Caller {
            binding_id: self.binding_id.clone(),
            grant_id: self.grant_id.clone(),
            session_id: self.session_id.clone(),
            provider_instance: self.provider_instance.clone(),
            native_id: self.native_id.clone().unwrap_or_default(),
            generation: self.generation,
            input_any: self.classes.contains(&ToolClass::InputAny),
        })
    }
}

/// The whole round trip of `coordinator.authority_check` (ADR-008 §6.2).
pub const AUTHORITY_CHECK_DEADLINE: std::time::Duration = std::time::Duration::from_secs(5);

/// The result of one `coordinator.authority_check` (ADR-008 §6.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Authority {
    /// The deck answered `valid`.
    Valid,
    /// The deck answered `invalid`, or refused with `forbidden`: the
    /// authority ended (`authority_lost`).
    Lost,
    /// No answer: no deck link, a link without `coordinator.v2`, a
    /// timeout, another error or an answer that does not parse
    /// (`authority_unknown`).
    Unknown,
}

impl Bindings {
    /// Asks the deck whether the authority of a routed command still holds
    /// (ADR-008 §6.2): one request, 5 s in all. The station never caches an
    /// answer.
    pub async fn authority_check(
        &self,
        check: &fleetdeck_proto::mutation::AuthorityCheck,
    ) -> Authority {
        self.authority_check_within(check, AUTHORITY_CHECK_DEADLINE)
            .await
    }

    /// [`Self::authority_check`] with another deadline (station tests).
    pub async fn authority_check_within(
        &self,
        check: &fleetdeck_proto::mutation::AuthorityCheck,
        deadline: std::time::Duration,
    ) -> Authority {
        use fleetdeck_proto::link::Outcome;
        use fleetdeck_proto::mutation::AuthorityAnswer;
        let Some(deck) = self.deck() else {
            return Authority::Unknown;
        };
        let Ok(params) = serde_json::to_value(check) else {
            return Authority::Unknown;
        };
        let asked = tokio::time::timeout(
            deadline,
            deck.request("coordinator.authority_check", None, params),
        )
        .await;
        match asked {
            Ok(Ok(Outcome::Ok(value))) => match serde_json::from_value::<AuthorityAnswer>(value) {
                Ok(AuthorityAnswer { valid: true, .. }) => Authority::Valid,
                Ok(_) => Authority::Lost,
                Err(_) => Authority::Unknown,
            },
            Ok(Ok(Outcome::Err(e))) if e.code == fleetdeck_link::methods::code::FORBIDDEN => {
                Authority::Lost
            }
            Ok(Ok(Outcome::Err(_)) | Err(_)) | Err(_) => Authority::Unknown,
        }
    }
}
