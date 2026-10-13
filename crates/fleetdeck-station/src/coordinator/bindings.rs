//! Coordinator credentials and bindings (ADR-006 §5.3–§5.5).
//!
//! The station keeps only the SHA-256 digest of a credential. The raw value
//! lives in a [`Secret`] until it is put into the provider child's
//! environment; it is never stored, logged or sent.

use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use fleetdeck_core::secret::Secret;
use fleetdeck_proto::SessionId;
use fleetdeck_proto::event::ProviderKind;
use ring::rand::{SecureRandom, SystemRandom};
use tokio::sync::Semaphore;

/// The prefix of a coordinator credential.
pub const TOKEN_PREFIX: &str = "fdc1_";
/// The environment variable that carries the credential to the provider.
pub const TOKEN_ENV: &str = "FLEETDECK_COORDINATOR_TOKEN";
/// Concurrent requests of one binding, including waits for the deck (§5.1).
pub const BINDING_CONCURRENCY: usize = 8;
/// Requests per second of one binding (§5.1).
pub const BINDING_RATE: u32 = 20;

/// A tool class of a grant (ADR-006 §4, §6.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ToolClass {
    Read,
    Mail,
    Drive,
    Spawn,
    /// The session's own tools (`publish_artifact`, `set_focus`,
    /// `ask_coordinator`; DL-165). Every binding has it; no grant names
    /// it ([`ToolClass::parse`] never returns it).
    Own,
}

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
    pub state: BindingState,
    /// The cached control skill (§7.1), served as MCP `instructions`.
    pub control_skill: String,
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
}

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
    Ok {
        binding: Box<Binding>,
        permit: tokio::sync::OwnedSemaphorePermit,
    },
}

impl ToolClass {
    /// The class of a grant class name (`read`, `mail`, `drive`, `spawn`).
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "read" => Self::Read,
            "mail" => Self::Mail,
            "drive" => Self::Drive,
            "spawn" => Self::Spawn,
            _ => return None,
        })
    }
}

impl Binding {
    /// True for the binding of a session that has no coordinator grant: it
    /// holds only the session's own tools (DL-165). It has no grant id and
    /// never reaches the deck as a coordinator binding.
    #[must_use]
    pub fn is_fleet_only(&self) -> bool {
        self.grant_id.is_empty()
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
        };
        let mut map = self.lock();
        map.retain(|_, e| e.binding.binding_id != entry.binding.binding_id);
        map.insert(digest, entry);
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
        let Some(e) = map.get_mut(&digest(token)) else {
            tracing::info!(case = "unknown", "coordinator credential refused");
            return Auth::Unauthorized;
        };
        if matches!(
            e.binding.state,
            BindingState::Revoked | BindingState::Expired
        ) {
            tracing::info!(case = "ended", binding = %e.binding.binding_id, "coordinator credential refused");
            return Auth::Unauthorized;
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
        })
    }
}
