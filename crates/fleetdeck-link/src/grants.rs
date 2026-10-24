//! The scopes that a node granted to its paired keys (PLAN §9, L15).

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::{Arc, PoisonError, RwLock};

use fleetdeck_core::identity::{Fingerprint, PeerKey};
use fleetdeck_proto::link::{Scope, WireError};

/// The scopes that this node granted to each paired key. Read for every
/// inbound request, so a revocation applies to the next request at once.
pub trait GrantSource: fmt::Debug + Send + Sync {
    fn scopes(&self, peer: &Fingerprint) -> BTreeSet<Scope>;

    /// The message of a request from `peer` that the scope check refused,
    /// when this source can say why the scope is missing (G6). `None`
    /// keeps the generic text. It runs in the read loop of the link: no
    /// blocking, no I/O.
    fn refusal(
        &self,
        _peer: &Fingerprint,
        _method: &str,
        _params: &serde_json::Value,
    ) -> Option<String> {
        None
    }
}

/// `e` with the text of `source` when `e` is the scope refusal of
/// `methods::authorize` (code `forbidden`). The code and the decision stay.
pub(crate) fn explain(
    source: &dyn GrantSource,
    peer: &Fingerprint,
    method: &str,
    params: &serde_json::Value,
    mut e: WireError,
) -> WireError {
    if e.code == crate::methods::code::FORBIDDEN
        && let Some(text) = source.refusal(peer, method, params)
    {
        e.message = text;
    }
    e
}

/// An in-memory [`GrantSource`]. A key without an entry has no scope.
#[derive(Debug, Default)]
pub struct Grants {
    map: RwLock<BTreeMap<Fingerprint, BTreeSet<Scope>>>,
}

impl Grants {
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Replaces the scopes of `peer`.
    pub fn grant(&self, peer: Fingerprint, scopes: impl IntoIterator<Item = Scope>) {
        let mut map = self.map.write().unwrap_or_else(PoisonError::into_inner);
        map.insert(peer, scopes.into_iter().collect());
    }

    /// Removes every scope of `peer`.
    pub fn revoke(&self, peer: &Fingerprint) {
        let mut map = self.map.write().unwrap_or_else(PoisonError::into_inner);
        map.remove(peer);
    }
}

impl GrantSource for Grants {
    fn scopes(&self, peer: &Fingerprint) -> BTreeSet<Scope> {
        let map = self.map.read().unwrap_or_else(PoisonError::into_inner);
        map.get(peer).cloned().unwrap_or_default()
    }
}

#[async_trait::async_trait]
impl crate::pairing::PairingSink for Grants {
    async fn paired(&self, peer: &crate::pairing::PairedPeer) -> Result<(), String> {
        self.grant(peer.key.fingerprint(), peer.scopes.iter().copied());
        Ok(())
    }

    async fn revoked(&self, peer: &PeerKey) -> Result<(), String> {
        self.revoke(&peer.fingerprint());
        Ok(())
    }

    async fn rotated(&self, old: &PeerKey, new: &PeerKey) -> Result<(), String> {
        let mut map = self.map.write().unwrap_or_else(PoisonError::into_inner);
        if let Some(scopes) = map.remove(&old.fingerprint()) {
            map.insert(new.fingerprint(), scopes);
        }
        Ok(())
    }
}
