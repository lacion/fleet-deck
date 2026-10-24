//! The station's pins and grants as the link layer sees them (G5): a
//! [`GrantSource`] for the per-request scope check and a [`PairingSink`]
//! that writes each change to `station.db` in one transaction.
//!
//! The in-memory cache only answers the request check of the control
//! channel. A mutating command re-reads the pin and grant in its decision
//! transaction (`Caller::peer`), so a stale cache can never authorize a
//! decision. A revocation empties the cache entry **before** the store
//! write (fail closed).

use std::collections::{BTreeSet, HashMap};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, PoisonError};

use fleetdeck_core::identity::{Fingerprint, PeerKey};
use fleetdeck_link::Grants;
use fleetdeck_link::grants::GrantSource;
use fleetdeck_link::pairing::{PairedPeer, PairingSink};
use fleetdeck_link::tls::PinSet;
use fleetdeck_proto::link::Scope;

use crate::store::peers::PairedKey;
use crate::store::{Result, Store};

/// The station's paired keys: `station.db` plus a grant cache.
#[derive(Debug)]
pub struct StationPeers {
    store: Store,
    cache: Arc<Grants>,
    /// `station join`: the deck address to store with the deck's pin, by
    /// the fingerprint of the invitation, so pin, grants and dial address
    /// commit in one transaction.
    dial_after: Mutex<HashMap<Fingerprint, SocketAddr>>,
}

impl StationPeers {
    /// Loads every paired key of `store` into the cache.
    ///
    /// # Errors
    /// A database error.
    pub async fn load(store: Store) -> Result<Arc<Self>> {
        let cache = Grants::new();
        for peer in store.paired_keys().await? {
            cache.grant(peer.key.fingerprint(), peer.scopes.iter().copied());
        }
        Ok(Arc::new(Self {
            store,
            cache,
            dial_after: Mutex::default(),
        }))
    }

    /// The store of these peers.
    #[must_use]
    pub fn store(&self) -> &Store {
        &self.store
    }

    /// Stores `addr` as the dial address of the key with `fingerprint`
    /// when it pairs (`station join`).
    pub fn dial_after_pairing(&self, fingerprint: Fingerprint, addr: SocketAddr) {
        self.dials().insert(fingerprint, addr);
    }

    /// Drops the dial address of a pairing that failed.
    pub fn pairing_failed(&self, fingerprint: &Fingerprint) {
        self.dials().remove(fingerprint);
    }

    fn dials(&self) -> std::sync::MutexGuard<'_, HashMap<Fingerprint, SocketAddr>> {
        self.dial_after
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// Pins every stored key in `pins` (at node start).
    ///
    /// # Errors
    /// A database error.
    pub async fn pin_all(&self, pins: &PinSet) -> Result<Vec<PairedKey>> {
        let keys = self.store.paired_keys().await?;
        for peer in &keys {
            pins.pin(peer.key, peer.role);
        }
        Ok(keys)
    }

    /// Moves the cached grants of `old` to `new` after a rotation that the
    /// decision transaction committed.
    pub fn rotated_in_store(&self, old: &PeerKey, new: &PeerKey) {
        let scopes = self.cache.scopes(&old.fingerprint());
        self.cache.revoke(&old.fingerprint());
        self.cache.grant(new.fingerprint(), scopes);
    }
}

impl GrantSource for StationPeers {
    fn scopes(&self, peer: &Fingerprint) -> BTreeSet<Scope> {
        self.cache.scopes(peer)
    }
}

#[async_trait::async_trait]
impl PairingSink for StationPeers {
    async fn paired(&self, peer: &PairedPeer) -> std::result::Result<(), String> {
        let dial_url = self
            .dials()
            .remove(&peer.key.fingerprint())
            .map(|a| a.to_string());
        self.store
            .pin_peer(PairedKey {
                key: peer.key,
                role: peer.role,
                scopes: peer.scopes.clone(),
                dial_url,
            })
            .await
            .map_err(|e| e.to_string())?;
        self.cache
            .grant(peer.key.fingerprint(), peer.scopes.iter().copied());
        Ok(())
    }

    async fn revoked(&self, peer: &PeerKey) -> std::result::Result<(), String> {
        self.cache.revoke(&peer.fingerprint());
        self.store
            .revoke_peer(*peer)
            .await
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    async fn rotated(&self, old: &PeerKey, new: &PeerKey) -> std::result::Result<(), String> {
        self.store
            .rotate_peer(*old, *new)
            .await
            .map_err(|e| e.to_string())?;
        self.rotated_in_store(old, new);
        Ok(())
    }
}
