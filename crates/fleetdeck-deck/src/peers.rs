//! The deck's pins and grants as the link layer sees them (G5): a
//! [`GrantSource`] for the per-request scope check and a [`PairingSink`]
//! that writes each change to `deck.db` in one transaction.
//!
//! A revocation empties the cache entry **before** the store write (fail
//! closed). `deck add-station` registers the name and dial address of the
//! invitation before it pairs, so pin, grants, name and address commit in
//! one transaction.

use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex, PoisonError};

use fleetdeck_core::identity::{Fingerprint, PeerKey};
use fleetdeck_link::Grants;
use fleetdeck_link::grants::GrantSource;
use fleetdeck_link::pair_hub::{CommitError, Completion, PairApprover};
use fleetdeck_link::pairing::{PairedPeer, PairingSink};
use fleetdeck_link::tls::{PinSet, Role};
use fleetdeck_proto::link::Scope;

use crate::store::stations::NewStation;
use crate::store::{DeckError, DeckStore, Result, StationRecord};

/// What `deck add-station` expects of the station that pairs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Expected {
    pub name: String,
    pub dial_url: Option<String>,
}

/// The deck's paired stations: `deck.db` plus a grant cache.
#[derive(Debug)]
pub struct DeckPeers {
    store: DeckStore,
    cache: Arc<Grants>,
    expected: Mutex<HashMap<Fingerprint, Expected>>,
}

impl DeckPeers {
    /// Loads every paired station of `store` into the cache.
    ///
    /// # Errors
    /// A database error.
    pub async fn load(store: DeckStore) -> Result<Arc<Self>> {
        let cache = Grants::new();
        for st in store.stations().await? {
            cache.grant(st.key.fingerprint(), st.scopes.iter().copied());
        }
        Ok(Arc::new(Self {
            store,
            cache,
            expected: Mutex::default(),
        }))
    }

    #[must_use]
    pub fn store(&self) -> &DeckStore {
        &self.store
    }

    /// Registers the name and dial address for the station whose key has
    /// `fingerprint` when it pairs.
    pub fn expect(&self, fingerprint: Fingerprint, expected: Expected) {
        self.expected_map().insert(fingerprint, expected);
    }

    /// Drops the registration of a pairing that failed.
    pub fn pairing_failed(&self, fingerprint: &Fingerprint) {
        self.expected_map().remove(fingerprint);
    }

    fn expected_map(&self) -> std::sync::MutexGuard<'_, HashMap<Fingerprint, Expected>> {
        self.expected.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Pins every stored station in `pins` (at node start).
    ///
    /// # Errors
    /// A database error.
    /// Reloads the scopes of one station from the store into the grant
    /// cache (a new coordinator grant adds `coordinate`, DL-211).
    ///
    /// # Errors
    /// A database error.
    pub async fn refresh(&self, id: fleetdeck_proto::StationId) -> Result<()> {
        if let Some(st) = self.store.station(id).await? {
            self.cache
                .grant(st.key.fingerprint(), st.scopes.iter().copied());
        }
        Ok(())
    }

    pub async fn pin_all(&self, pins: &PinSet) -> Result<Vec<StationRecord>> {
        let stations = self.store.stations().await?;
        for st in &stations {
            pins.pin(st.key, Role::Station);
        }
        Ok(stations)
    }

    async fn store_pairing(&self, peer: &PairedPeer) -> Result<StationRecord> {
        if peer.role != Role::Station {
            return Err(DeckError::NotAStation);
        }
        let fingerprint = peer.key.fingerprint();
        let expected = self
            .expected_map()
            .remove(&fingerprint)
            .unwrap_or_else(|| Expected {
                name: fingerprint.short(),
                dial_url: None,
            });
        self.store
            .pin_station(NewStation {
                key: peer.key,
                name: expected.name,
                scopes: peer.scopes.clone(),
                dial_url: expected.dial_url,
            })
            .await
    }
}

impl GrantSource for DeckPeers {
    fn scopes(&self, peer: &Fingerprint) -> BTreeSet<Scope> {
        self.cache.scopes(peer)
    }
}

#[async_trait::async_trait]
impl PairingSink for DeckPeers {
    async fn paired(&self, peer: &PairedPeer) -> std::result::Result<(), String> {
        self.store_pairing(peer).await.map_err(|e| e.to_string())?;
        self.cache
            .grant(peer.key.fingerprint(), peer.scopes.iter().copied());
        Ok(())
    }

    async fn revoked(&self, peer: &PeerKey) -> std::result::Result<(), String> {
        self.cache.revoke(&peer.fingerprint());
        let removed = self
            .store
            .revoke_station(*peer)
            .await
            .map_err(|e| e.to_string())?;
        if let Some(st) = removed {
            self.store.set_reachable(&st.station_id, false);
        }
        Ok(())
    }

    async fn rotated(&self, old: &PeerKey, new: &PeerKey) -> std::result::Result<(), String> {
        self.store
            .rotate_station(*old, *new)
            .await
            .map_err(|e| e.to_string())?;
        let scopes = self.cache.scopes(&old.fingerprint());
        self.cache.revoke(&old.fingerprint());
        self.cache.grant(new.fingerprint(), scopes);
        Ok(())
    }
}

/// The approval of a pairing request (DL-178): the station, its grants and
/// the completion record in one transaction. `pin_station` is never used
/// here; a key that is already paired is refused.
#[async_trait::async_trait]
impl PairApprover for DeckPeers {
    async fn commit(&self, completion: &Completion) -> std::result::Result<(), CommitError> {
        self.store.commit_pairing(completion.clone()).await?;
        self.cache.grant(
            completion.station_key.fingerprint(),
            completion.granted_scopes.iter().copied(),
        );
        Ok(())
    }

    async fn completion(
        &self,
        request_id: &str,
    ) -> std::result::Result<Option<Completion>, String> {
        self.store
            .pairing_completion(request_id.to_owned())
            .await
            .map_err(|e| e.to_string())
    }
}
