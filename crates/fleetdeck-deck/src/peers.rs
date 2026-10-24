//! The deck's pins and grants as the link layer sees them (G5): a
//! [`GrantSource`] for the per-request scope check and a [`PairingSink`]
//! that writes each change to `deck.db` in one transaction.
//!
//! A revocation empties the cache entry **before** the store write (fail
//! closed). `deck add-station` registers the name and dial address of the
//! invitation before it pairs, so pin, grants, name and address commit in
//! one transaction.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Arc, Mutex, PoisonError, RwLock};

use fleetdeck_core::identity::{Fingerprint, PeerKey};
use fleetdeck_link::Grants;
use fleetdeck_link::grants::GrantSource;
use fleetdeck_link::pair_hub::{CommitError, Completion, PairApprover};
use fleetdeck_link::pairing::{PairedPeer, PairingSink};
use fleetdeck_link::tls::{PinSet, Role};
use fleetdeck_proto::SessionId;
use fleetdeck_proto::link::Scope;
use serde_json::Value;

use crate::store::grants::grant_end::{EndFacts, end_text};
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
    /// Why the newest grant of each session of a station is not in effect
    /// (G6 §3.6), by the key of the station. Written with the scopes.
    ends: RwLock<HashMap<Fingerprint, BTreeMap<SessionId, EndFacts>>>,
    expected: Mutex<HashMap<Fingerprint, Expected>>,
    /// Held across the database read and the publish of one refresh, and
    /// across the drain and the publish of one batch of `refresh_changed`, so
    /// two refreshes never publish out of order (grant stations design §2.4).
    refresh_lock: tokio::sync::Mutex<()>,
}

impl DeckPeers {
    /// Loads every paired station of `store` into the cache.
    ///
    /// # Errors
    /// A database error.
    pub async fn load(store: DeckStore) -> Result<Arc<Self>> {
        let cache = Grants::new();
        let mut ends = HashMap::new();
        for st in store.stations().await? {
            cache.grant(st.key.fingerprint(), st.scopes.iter().copied());
            ends.insert(
                st.key.fingerprint(),
                store.grant_ends(st.station_id.clone()).await?,
            );
        }
        let peers = Arc::new(Self {
            store,
            cache,
            ends: RwLock::new(ends),
            expected: Mutex::default(),
            refresh_lock: tokio::sync::Mutex::new(()),
        });
        peers.follow_scope_changes();
        Ok(peers)
    }

    /// Starts the task that refreshes the cache after each note of changed
    /// scopes of the store (a lifecycle event, a lazy expiry, a snapshot,
    /// the sweep). It ends with the store or with the last `Arc` of `self`.
    fn follow_scope_changes(self: &Arc<Self>) {
        let weak = Arc::downgrade(self);
        let mut changes = self.store.subscribe_scope_changes();
        tokio::spawn(async move {
            while changes.changed().await.is_ok() {
                let Some(peers) = weak.upgrade() else { return };
                if let Err(e) = peers.refresh_changed().await {
                    tracing::warn!(error = %e, "refresh of the link scopes");
                }
            }
        });
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

    fn ends_map(
        &self,
    ) -> std::sync::RwLockWriteGuard<'_, HashMap<Fingerprint, BTreeMap<SessionId, EndFacts>>> {
        self.ends.write().unwrap_or_else(PoisonError::into_inner)
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
        let _turn = self.refresh_lock.lock().await;
        self.refresh_one(id).await
    }

    /// One refresh; the caller holds `refresh_lock`.
    async fn refresh_one(&self, id: fleetdeck_proto::StationId) -> Result<()> {
        if let Some(st) = self.store.station(id).await? {
            // The ends first, then the scopes: a refusal never reads ends
            // that are older than the scopes that caused it.
            let ends = self.store.grant_ends(st.station_id.clone()).await?;
            self.ends_map().insert(st.key.fingerprint(), ends);
            self.cache
                .grant(st.key.fingerprint(), st.scopes.iter().copied());
        }
        Ok(())
    }

    /// Refreshes every station that the store noted as changed. The store is
    /// the truth: each refresh reads the current stored scopes. The drain of
    /// the noted set and the publish of the whole batch run under one lock,
    /// and a call with nothing noted takes it too: the call returns only
    /// after every earlier batch is published (grant stations PRE-PR r1).
    ///
    /// # Errors
    /// A database error (the stations stay noted for the next refresh).
    pub async fn refresh_changed(&self) -> Result<()> {
        let _turn = self.refresh_lock.lock().await;
        let changed = self.store.take_scope_changes();
        for id in &changed {
            if let Err(e) = self.refresh_one(id.clone()).await {
                // Retried by the next refresh (the sweep runs every 30 s).
                self.store.restore_scope_changes(changed);
                return Err(e);
            }
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

    fn refusal(&self, peer: &Fingerprint, method: &str, params: &Value) -> Option<String> {
        let caller = match method {
            "coordinator.call" => params.get("call")?.get("caller")?.as_str()?,
            "coordinator.query" => params.get("caller")?.as_str()?,
            _ => return None,
        };
        let facts = {
            let ends = self.ends.read().unwrap_or_else(PoisonError::into_inner);
            ends.get(peer)?.get(&SessionId::from(caller)).copied()?
        };
        Some(end_text(facts.at(self.store.now_ms())))
    }
}

#[async_trait::async_trait]
impl PairingSink for DeckPeers {
    async fn paired(&self, peer: &PairedPeer) -> std::result::Result<(), String> {
        let st = self.store_pairing(peer).await.map_err(|e| e.to_string())?;
        // The stored scopes are the truth: `coordinate` follows the grants in
        // effect, not the pairing scopes (design §2.3 item 6, §2.4).
        self.refresh(st.station_id)
            .await
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    async fn revoked(&self, peer: &PeerKey) -> std::result::Result<(), String> {
        self.cache.revoke(&peer.fingerprint());
        self.ends_map().remove(&peer.fingerprint());
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
        let mut ends = self.ends_map();
        if let Some(moved) = ends.remove(&old.fingerprint()) {
            ends.insert(new.fingerprint(), moved);
        }
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

#[cfg(test)]
#[path = "peers_tests.rs"]
mod tests;
