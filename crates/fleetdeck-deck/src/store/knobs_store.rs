//! The store side of the deck knobs: the copy that a settings publish
//! writes ([`DeckStore::set_knobs`]), and the deletion of the sessions over a
//! lowered limit (knobs design, rule 11).

use std::sync::PoisonError;

use rusqlite::params;

use super::{DeckStore, Inner, Result, call};
use crate::board_fence::registry::SessionKind;
use crate::knobs::DeckKnobs;

impl Inner {
    /// The current knobs (a poisoned lock gives the inner value).
    pub(super) fn knobs(&self) -> DeckKnobs {
        *self.knobs.read().unwrap_or_else(PoisonError::into_inner)
    }
}

impl DeckStore {
    /// Sets the knobs that the store reads (a settings publish calls it; the
    /// deck start calls it once).
    pub fn set_knobs(&self, knobs: DeckKnobs) {
        *self
            .inner
            .knobs
            .write()
            .unwrap_or_else(PoisonError::into_inner) = knobs;
    }

    /// The current knobs. An operation reads them once when it starts.
    #[must_use]
    pub fn knobs(&self) -> DeckKnobs {
        self.inner.knobs()
    }

    /// Rule 11: in one transaction, deletes the sessions that are over the
    /// current knobs (call it after [`Self::set_knobs`]).
    ///
    /// - `logins`: the owner `login_sessions` with `created_at <= now -
    ///   login_max_ms` or `last_seen <= now - login_idle_ms`, and the
    ///   `member_sessions` with `created_at <= now - login_max_ms`.
    /// - `displays`: the `display_sessions` with `created_at <= now -
    ///   display_max_ms` or `last_seen <= now - display_idle_ms`.
    ///
    /// Returns the `(kind, session_hash)` of the owner and member sessions
    /// that it deleted (the display sessions are deleted only: a display
    /// socket ends at its own session recheck).
    ///
    /// # Errors
    /// A database error.
    pub async fn end_sessions_over_limits(
        &self,
        logins: bool,
        displays: bool,
        now: i64,
    ) -> Result<Vec<(SessionKind, String)>> {
        let knobs = self.knobs();
        call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            let mut ended = Vec::new();
            if logins {
                let mut stmt = tx.prepare(
                    "DELETE FROM login_sessions
                     WHERE created_at <= ?1 OR last_seen <= ?2
                     RETURNING session_hash",
                )?;
                let rows = stmt.query_map(
                    params![now - knobs.login_max_ms, now - knobs.login_idle_ms],
                    |r| r.get::<_, String>(0),
                )?;
                for hash in rows {
                    ended.push((SessionKind::Owner, hash?));
                }
                drop(stmt);
                let mut stmt = tx.prepare(
                    "DELETE FROM member_sessions WHERE created_at <= ?1
                     RETURNING session_hash",
                )?;
                let rows = stmt.query_map([now - knobs.login_max_ms], |r| r.get::<_, String>(0))?;
                for hash in rows {
                    ended.push((SessionKind::Member, hash?));
                }
            }
            if displays {
                tx.execute(
                    "DELETE FROM display_sessions WHERE created_at <= ?1 OR last_seen <= ?2",
                    params![now - knobs.display_max_ms, now - knobs.display_idle_ms],
                )?;
            }
            tx.commit()?;
            Ok(ended)
        })
        .await
    }
}

#[cfg(test)]
#[path = "knobs_tests.rs"]
mod tests;
