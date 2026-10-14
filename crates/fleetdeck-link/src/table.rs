//! The node-wide link table (ADR-004 §9.1, G1, G10).
//!
//! One table per node, keyed by the peer key fingerprint. It holds the links
//! of every listener and every dialer of the node. When a second link to the
//! same peer is admitted, both nodes compute the same winner:
//!
//! 1. Same dialer: the higher epoch wins.
//! 2. Opposite dialers (a reciprocal dial): the link dialed by the node with
//!    the lower key fingerprint wins.
//!
//! The loser closes with `superseded`. An entry is removed by its
//! [`TableGuard`], which is created by the insert itself and checks the
//! generation: a failed setup never leaves an entry behind, and the guard of
//! a superseded link never removes its successor (G10). Replacing an entry
//! also drops its data-channel slot, so a superseded setup wait ends at once.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};

use fleetdeck_core::identity::Fingerprint;
use tokio::sync::{oneshot, watch};

use crate::ws::Ws;

/// The close reason of a link that lost the resolution.
pub const SUPERSEDED: &str = "superseded";

/// Which node dialed a link, seen from this node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dialer {
    Local,
    Remote,
}

/// The identity of one link: the dialer's epoch and attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkId {
    pub epoch: u64,
    pub attempt_id: String,
    pub dialer: Dialer,
}

/// Why a link must close; sent to the link's [`Admitted::closing`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Closing {
    pub reason: String,
    pub detail: String,
}

/// The resolution rule: does `new` replace `old`? `own` is this node's
/// fingerprint, `peer` the other node's.
pub fn wins(own: &Fingerprint, peer: &Fingerprint, new: &LinkId, old: &LinkId) -> bool {
    if new.dialer == old.dialer {
        return new.epoch > old.epoch;
    }
    let dialed_by = |id: &LinkId| match id.dialer {
        Dialer::Local => own,
        Dialer::Remote => peer,
    };
    dialed_by(new) < dialed_by(old)
}

struct Entry {
    generation: u64,
    id: LinkId,
    closing: watch::Sender<Option<Closing>>,
    data: Option<oneshot::Sender<Ws>>,
}

#[derive(Default)]
struct Inner {
    next_generation: u64,
    entries: HashMap<Fingerprint, Entry>,
}

/// The link table of one node. Clones share the table.
#[derive(Clone)]
pub struct LinkTable {
    own: Fingerprint,
    inner: Arc<Mutex<Inner>>,
}

impl std::fmt::Debug for LinkTable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LinkTable")
            .field("own", &self.own.short())
            .field("links", &self.len())
            .finish()
    }
}

/// An admitted link: its table entry lives as long as the guard.
#[derive(Debug)]
pub struct Admitted {
    pub guard: TableGuard,
    /// Becomes `Some` when a later link supersedes this one (or when the
    /// node closes the peer's links).
    pub closing: watch::Receiver<Option<Closing>>,
    /// The data channel of the same attempt, when the link expects one
    /// (listener side).
    pub data: Option<oneshot::Receiver<Ws>>,
}

/// The new link lost against the current one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lost {
    pub current: LinkId,
}

impl Lost {
    pub fn detail(&self) -> String {
        format!(
            "the current link (epoch {}, dialed by the {} node) wins",
            self.current.epoch,
            match self.current.dialer {
                Dialer::Local => "receiving",
                Dialer::Remote => "sending",
            }
        )
    }
}

impl LinkTable {
    pub fn new(own: Fingerprint) -> Self {
        Self {
            own,
            inner: Arc::default(),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        // A panic while the lock was held cannot leave an entry half
        // written: every change is one map operation.
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Admits a link to `peer` or tells why it lost. A replaced entry gets
    /// `superseded` and loses its data slot.
    pub fn insert(
        &self,
        peer: Fingerprint,
        id: LinkId,
        wants_data: bool,
    ) -> Result<Admitted, Lost> {
        let mut inner = self.lock();
        if let Some(old) = inner.entries.get(&peer)
            && !wins(&self.own, &peer, &id, &old.id)
        {
            return Err(Lost {
                current: old.id.clone(),
            });
        }
        inner.next_generation += 1;
        let generation = inner.next_generation;
        let (closing_tx, closing) = watch::channel(None);
        let (data_tx, data) = if wants_data {
            let (tx, rx) = oneshot::channel();
            (Some(tx), Some(rx))
        } else {
            (None, None)
        };
        let detail = format!("a link with epoch {} replaced this link", id.epoch);
        let old = inner.entries.insert(
            peer,
            Entry {
                generation,
                id,
                closing: closing_tx,
                data: data_tx,
            },
        );
        drop(inner);
        if let Some(old) = old {
            old.closing.send_replace(Some(Closing {
                reason: SUPERSEDED.into(),
                detail,
            }));
        }
        Ok(Admitted {
            guard: TableGuard {
                table: self.clone(),
                peer,
                generation,
            },
            closing,
            data,
        })
    }

    /// The data-channel slot of the current link to `peer`, when its epoch
    /// and attempt match and the peer dialed it. The slot is single use.
    pub fn take_data_slot(
        &self,
        peer: &Fingerprint,
        epoch: u64,
        attempt_id: &str,
    ) -> Option<oneshot::Sender<Ws>> {
        let mut inner = self.lock();
        let e = inner.entries.get_mut(peer)?;
        let matches =
            e.id.dialer == Dialer::Remote && e.id.epoch == epoch && e.id.attempt_id == attempt_id;
        if matches { e.data.take() } else { None }
    }

    /// The current link to `peer`.
    pub fn current(&self, peer: &Fingerprint) -> Option<LinkId> {
        self.lock().entries.get(peer).map(|e| e.id.clone())
    }

    /// Tells the current link to `peer` to close (revocation, G5).
    pub fn close_peer(&self, peer: &Fingerprint, closing: Closing) -> bool {
        match self.lock().entries.get(peer) {
            Some(e) => {
                e.closing.send_replace(Some(closing));
                true
            }
            None => false,
        }
    }

    pub fn len(&self) -> usize {
        self.lock().entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Removes its table entry on drop, but only when the entry still has the
/// guard's generation.
#[derive(Debug)]
pub struct TableGuard {
    table: LinkTable,
    peer: Fingerprint,
    generation: u64,
}

impl TableGuard {
    /// Whether this guard's link is still the current link of its peer.
    pub fn is_current(&self) -> bool {
        self.check().is_current()
    }

    /// A cloneable check of the same test, for tasks that outlive the guard's
    /// owner.
    pub fn check(&self) -> CurrentCheck {
        CurrentCheck {
            table: self.table.clone(),
            peer: self.peer,
            generation: self.generation,
        }
    }
}

/// Tells whether a link is still the current link of its peer.
///
/// The generation is fixed when the link wins admission. A later winner
/// changes the entry, so a delayed task of an old link sees `false` (NV-12).
#[derive(Debug, Clone)]
pub struct CurrentCheck {
    table: LinkTable,
    peer: Fingerprint,
    generation: u64,
}

impl CurrentCheck {
    /// Whether the peer's entry still has this check's generation.
    pub fn is_current(&self) -> bool {
        self.table
            .lock()
            .entries
            .get(&self.peer)
            .is_some_and(|e| e.generation == self.generation)
    }
}

impl Drop for TableGuard {
    fn drop(&mut self) {
        let mut inner = self.table.lock();
        if inner
            .entries
            .get(&self.peer)
            .is_some_and(|e| e.generation == self.generation)
        {
            inner.entries.remove(&self.peer);
        }
    }
}

#[cfg(test)]
#[path = "table_tests.rs"]
mod tests;
