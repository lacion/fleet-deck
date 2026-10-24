//! Who has a session open now, and the updates of the people features (R11.5,
//! DL-166). Presence lives in the memory of the deck: an open board socket
//! sends a heartbeat every 20 s for the session that it shows, and a viewer
//! without a heartbeat for 60 s is gone. A deck restart clears it; the open
//! boards send their heartbeats again.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::PoisonError;
use std::sync::atomic::Ordering;

use fleetdeck_proto::board::{BoardServerMessage, SessionKey};
use fleetdeck_proto::share::PRESENCE_GONE_MS;
use tokio::sync::broadcast;

use super::DeckStore;

/// One update for the sockets that may see `key`.
#[derive(Debug, Clone, PartialEq)]
pub struct SocialEvent {
    pub key: SessionKey,
    /// A `presence`, `chat`, `driver` or `drive_request` message.
    pub message: BoardServerMessage,
}

/// The viewers of each session: `(actor, socket id)` → last heartbeat (Unix
/// ms). A person with two tabs has two sockets.
pub(super) type PresenceMap = BTreeMap<SessionKey, BTreeMap<(String, u64), i64>>;

fn who(viewers: &BTreeMap<(String, u64), i64>, now: i64) -> Vec<String> {
    let names: BTreeSet<&str> = viewers
        .iter()
        .filter(|(_, at)| **at > now - PRESENCE_GONE_MS)
        .map(|((actor, _), _)| actor.as_str())
        .collect();
    names.into_iter().map(str::to_owned).collect()
}

impl DeckStore {
    /// The updates of presence, chat and the mob driver from now on.
    #[must_use]
    pub fn subscribe_social(&self) -> broadcast::Receiver<std::sync::Arc<SocialEvent>> {
        self.inner.social.subscribe()
    }

    pub(crate) fn publish_social(&self, key: SessionKey, message: BoardServerMessage) {
        // No receiver is not an error.
        let _ = self
            .inner
            .social
            .send(std::sync::Arc::new(SocialEvent { key, message }));
    }

    /// A new id for an open socket (the presence of a socket ends with it).
    #[must_use]
    pub fn new_socket_id(&self) -> u64 {
        self.inner.sockets.fetch_add(1, Ordering::Relaxed)
    }

    fn presence_map(&self) -> std::sync::MutexGuard<'_, PresenceMap> {
        self.inner
            .presence
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    fn presence_message(key: &SessionKey, who: Vec<String>) -> BoardServerMessage {
        BoardServerMessage::Presence {
            station_id: key.station_id.clone(),
            session_id: key.session_id.clone(),
            who,
        }
    }

    /// The people who have `key` open now.
    #[must_use]
    pub fn presence_of(&self, key: &SessionKey) -> Vec<String> {
        let now = self.now_ms();
        self.presence_map()
            .get(key)
            .map(|v| who(v, now))
            .unwrap_or_default()
    }

    /// A heartbeat of `actor` on `socket` for `key`. A change of the set of
    /// people is published.
    pub fn presence_heartbeat(&self, key: &SessionKey, actor: &str, socket: u64) {
        let now = self.now_ms();
        let changed = {
            let mut map = self.presence_map();
            let viewers = map.entry(key.clone()).or_default();
            let before = who(viewers, now);
            viewers.insert((actor.to_owned(), socket), now);
            let after = who(viewers, now);
            (before != after).then_some(after)
        };
        if let Some(after) = changed {
            self.publish_social(key.clone(), Self::presence_message(key, after));
        }
    }

    /// The socket closed: its viewers are gone.
    pub fn presence_leave(&self, socket: u64) {
        let now = self.now_ms();
        let changes = {
            let mut map = self.presence_map();
            let mut out = Vec::new();
            for (key, viewers) in map.iter_mut() {
                let before = who(viewers, now);
                viewers.retain(|(_, s), _| *s != socket);
                let after = who(viewers, now);
                if before != after {
                    out.push((key.clone(), after));
                }
            }
            map.retain(|_, v| !v.is_empty());
            out
        };
        for (key, after) in changes {
            self.publish_social(key.clone(), Self::presence_message(&key, after));
        }
    }

    /// Drops the viewers without a heartbeat for 60 s and publishes the
    /// sessions whose set changed.
    pub fn presence_sweep(&self) {
        let now = self.now_ms();
        let changes = {
            let mut map = self.presence_map();
            let mut out = Vec::new();
            for (key, viewers) in map.iter_mut() {
                let before = who(viewers, now - 1);
                viewers.retain(|_, at| *at > now - PRESENCE_GONE_MS);
                let after = who(viewers, now);
                if before != after || viewers.is_empty() {
                    out.push((key.clone(), after));
                }
            }
            map.retain(|_, v| !v.is_empty());
            out
        };
        for (key, after) in changes {
            self.publish_social(key.clone(), Self::presence_message(&key, after));
        }
    }
}
