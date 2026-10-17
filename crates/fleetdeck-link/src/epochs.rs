//! Persistent per-peer link epochs (ADR-004 §9.1, G1).
//!
//! - The **dialer** increments its counter for the peer and commits it
//!   before each dial ([`Epochs::next_dial`]). It never comes from the clock.
//! - The **listener** keeps the highest epoch that it accepted from the peer
//!   ([`Epochs::accept`]). A lower or equal epoch is stale; the refusal names
//!   the lowest epoch that the listener accepts next (`min_epoch`).
//! - A dialer that gets `stale_link_epoch` raises its counter
//!   ([`Epochs::raise_dial`]) and dials again, so a dialer whose database was
//!   reset recovers after one attempt.
//!
//! The two counters of one peer are separate columns: a node can dial a peer
//! and also accept links from it (DL-042).

use std::collections::BTreeMap;
use std::sync::Mutex;

use fleetdeck_core::db::{Db, DbError};
use fleetdeck_core::identity::Fingerprint;
use rusqlite::{OptionalExtension, params};

use crate::ws::LinkError;

/// The `link_epochs` table; station.db and deck.db add it as a migration.
pub const LINK_EPOCHS_SQL: &str = "
CREATE TABLE link_epochs (
    peer_key TEXT PRIMARY KEY,
    dial_epoch INTEGER NOT NULL DEFAULT 0 CHECK (dial_epoch >= 0),
    accepted_epoch INTEGER NOT NULL DEFAULT 0 CHECK (accepted_epoch >= 0)
) STRICT;
";

#[derive(Debug, Default, Clone, Copy)]
struct Row {
    dial: u64,
    accepted: u64,
}

enum Backend {
    Memory(Mutex<BTreeMap<Fingerprint, Row>>),
    Sqlite(Db),
}

/// The link epochs of one node.
pub struct Epochs {
    backend: Backend,
}

impl std::fmt::Debug for Epochs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let kind = match self.backend {
            Backend::Memory(_) => "memory",
            Backend::Sqlite(_) => "sqlite",
        };
        f.debug_struct("Epochs").field("backend", &kind).finish()
    }
}

fn to_i64(n: u64) -> Result<i64, DbError> {
    i64::try_from(n).map_err(|_| DbError::Io(std::io::Error::other("link epoch overflow")))
}

fn to_u64(n: i64) -> u64 {
    u64::try_from(n).unwrap_or(0)
}

fn db_error(e: DbError) -> LinkError {
    LinkError::Epochs(e.to_string())
}

impl Epochs {
    /// Epochs that live only as long as the process (tests, tools).
    pub fn in_memory() -> Self {
        Self {
            backend: Backend::Memory(Mutex::default()),
        }
    }

    /// Epochs in the node database; its migrations include
    /// [`LINK_EPOCHS_SQL`].
    pub fn sqlite(db: Db) -> Self {
        Self {
            backend: Backend::Sqlite(db),
        }
    }

    fn memory<R>(
        map: &Mutex<BTreeMap<Fingerprint, Row>>,
        f: impl FnOnce(&mut BTreeMap<Fingerprint, Row>) -> R,
    ) -> Result<R, LinkError> {
        let mut m = map
            .lock()
            .map_err(|_| LinkError::Epochs("the epoch lock is poisoned".into()))?;
        Ok(f(&mut m))
    }

    /// Increments and commits the dial epoch for `peer`; returns the new
    /// value (at least 1).
    pub async fn next_dial(&self, peer: &Fingerprint) -> Result<u64, LinkError> {
        match &self.backend {
            Backend::Memory(m) => Self::memory(m, |m| {
                let row = m.entry(*peer).or_default();
                row.dial = row.dial.saturating_add(1);
                row.dial
            }),
            Backend::Sqlite(db) => {
                let key = peer.to_string();
                db.call(move |c| {
                    let n: i64 = c.query_row(
                        "INSERT INTO link_epochs (peer_key, dial_epoch) VALUES (?1, 1)
                         ON CONFLICT (peer_key) DO UPDATE SET dial_epoch = dial_epoch + 1
                         RETURNING dial_epoch",
                        params![key],
                        |r| r.get(0),
                    )?;
                    Ok(to_u64(n))
                })
                .await
                .map_err(db_error)
            }
        }
    }

    /// After `stale_link_epoch`: the next [`Epochs::next_dial`] returns at
    /// least `min_epoch`.
    pub async fn raise_dial(&self, peer: &Fingerprint, min_epoch: u64) -> Result<(), LinkError> {
        let floor = min_epoch.saturating_sub(1);
        match &self.backend {
            Backend::Memory(m) => Self::memory(m, |m| {
                let row = m.entry(*peer).or_default();
                row.dial = row.dial.max(floor);
            }),
            Backend::Sqlite(db) => {
                let key = peer.to_string();
                db.call(move |c| {
                    c.execute(
                        "INSERT INTO link_epochs (peer_key, dial_epoch) VALUES (?1, ?2)
                         ON CONFLICT (peer_key) DO UPDATE
                         SET dial_epoch = MAX(dial_epoch, excluded.dial_epoch)",
                        params![key, to_i64(floor)?],
                    )?;
                    Ok(())
                })
                .await
                .map_err(db_error)
            }
        }
    }

    /// The listener side: accepts `epoch` from `peer` and commits it when it
    /// is higher than every epoch accepted before. `Ok(Err(min_epoch))` when
    /// it is stale.
    pub async fn accept(
        &self,
        peer: &Fingerprint,
        epoch: u64,
    ) -> Result<Result<(), u64>, LinkError> {
        match &self.backend {
            Backend::Memory(m) => Self::memory(m, |m| {
                let row = m.entry(*peer).or_default();
                if epoch <= row.accepted {
                    Err(row.accepted.saturating_add(1))
                } else {
                    row.accepted = epoch;
                    Ok(())
                }
            }),
            Backend::Sqlite(db) => {
                let key = peer.to_string();
                db.call(move |c| {
                    let tx = c.transaction()?;
                    let accepted: Option<i64> = tx
                        .query_row(
                            "SELECT accepted_epoch FROM link_epochs WHERE peer_key = ?1",
                            params![key],
                            |r| r.get(0),
                        )
                        .optional()?;
                    let accepted = accepted.map(to_u64).unwrap_or(0);
                    if epoch <= accepted {
                        return Ok(Err(accepted.saturating_add(1)));
                    }
                    tx.execute(
                        "INSERT INTO link_epochs (peer_key, accepted_epoch) VALUES (?1, ?2)
                         ON CONFLICT (peer_key) DO UPDATE SET accepted_epoch = excluded.accepted_epoch",
                        params![key, to_i64(epoch)?],
                    )?;
                    tx.commit()?;
                    Ok(Ok(()))
                })
                .await
                .map_err(db_error)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fleetdeck_core::identity::Identity;
    use rusqlite_migration::{M, Migrations};

    fn sqlite(path: &std::path::Path) -> Epochs {
        let db = Db::open(path, Migrations::new(vec![M::up(LINK_EPOCHS_SQL)])).unwrap();
        Epochs::sqlite(db)
    }

    async fn rules(e: &Epochs) {
        let a = Identity::generate().unwrap().fingerprint();
        let b = Identity::generate().unwrap().fingerprint();
        assert_eq!(e.next_dial(&a).await.unwrap(), 1);
        assert_eq!(e.next_dial(&a).await.unwrap(), 2);
        assert_eq!(e.next_dial(&b).await.unwrap(), 1);
        e.raise_dial(&a, 10).await.unwrap();
        assert_eq!(e.next_dial(&a).await.unwrap(), 10);
        e.raise_dial(&a, 3).await.unwrap();
        assert_eq!(e.next_dial(&a).await.unwrap(), 11);
        // The listener counter is separate from the dial counter.
        assert_eq!(e.accept(&a, 1).await.unwrap(), Ok(()));
        assert_eq!(e.accept(&a, 1).await.unwrap(), Err(2));
        assert_eq!(e.accept(&a, 5).await.unwrap(), Ok(()));
        assert_eq!(e.accept(&a, 4).await.unwrap(), Err(6));
        assert_eq!(e.next_dial(&a).await.unwrap(), 12);
    }

    #[tokio::test]
    async fn the_memory_and_sqlite_stores_follow_the_same_rules() {
        rules(&Epochs::in_memory()).await;
        let dir = tempfile::tempdir().unwrap();
        rules(&sqlite(&dir.path().join("node.db"))).await;
    }

    #[tokio::test]
    async fn committed_epochs_survive_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("node.db");
        let peer = Identity::generate().unwrap().fingerprint();
        {
            let e = sqlite(&path);
            assert_eq!(e.next_dial(&peer).await.unwrap(), 1);
            assert_eq!(e.accept(&peer, 7).await.unwrap(), Ok(()));
        }
        let e = sqlite(&path);
        assert_eq!(e.next_dial(&peer).await.unwrap(), 2);
        assert_eq!(e.accept(&peer, 7).await.unwrap(), Err(8));
    }
}
