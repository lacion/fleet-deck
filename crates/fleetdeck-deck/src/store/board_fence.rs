//! The store part of the board fence (R13 P7 §7.6): the address of each
//! browser session, the exposed addresses that the durable operation rows
//! give, the sign-out of the sessions of a set of addresses, the socket
//! registry and the local station.

use std::collections::HashSet;
use std::sync::{Arc, OnceLock, PoisonError};

use fleetdeck_proto::StationId;
use rusqlite::{Connection, Transaction, params};

use super::{DeckStore, Result, call};
use crate::board_fence::exposure::{
    FINISHED_TAIL_MS, SIGN_OUT_TAIL_MS, UNFINISHED_WINDOW_MS, parse_snapshot,
};
use crate::board_fence::registry::{Registry, SessionKind};

/// A session address is written again when it is older than this.
const TOUCH_EVERY_MS: i64 = 10_000;

/// One deleted browser session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignedOut {
    pub kind: SessionKind,
    /// The SHA-256 of the cookie value (never the cookie).
    pub session_hash: String,
}

/// The table of a kind of session.
fn table(kind: SessionKind) -> &'static str {
    match kind {
        SessionKind::Owner => "login_sessions",
        SessionKind::Member => "member_sessions",
    }
}

/// The canonical addresses of every exposing operation at `now`: the
/// `exposure_addrs` of each input operation that was `attempted` and whose
/// exposure plus the sign-out tail has not passed.
fn read_exposed(conn: &Connection, now: i64) -> Result<HashSet<String>> {
    let mut stmt = conn.prepare(
        "SELECT operation_id, exposure_addrs FROM coordinator_desktop_ops
         WHERE tool IN ('key_press', 'type_text', 'pointer_move', 'pointer_click')
           AND attempted_at IS NOT NULL
           AND COALESCE(finished_at + ?2, attempted_at + ?3) + ?4 > ?1",
    )?;
    let rows = stmt.query_map(
        params![
            now,
            FINISHED_TAIL_MS,
            UNFINISHED_WINDOW_MS,
            SIGN_OUT_TAIL_MS
        ],
        |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?)),
    )?;
    let mut set = HashSet::new();
    for row in rows {
        let (operation_id, text) = row?;
        let Some(text) = text else { continue };
        match parse_snapshot(&text) {
            Some(addrs) => set.extend(addrs),
            None => tracing::warn!(
                operation_id,
                "an input operation has an exposure snapshot that is not a JSON array of strings; it counts as no address"
            ),
        }
    }
    Ok(set)
}

/// Deletes every owner login session and member session whose `last_addr`
/// is in `addrs` (whatever its age), and every session of `sessions` (the
/// sessions that the socket registry names for the same addresses,
/// [`Registry::sessions_at`]), and returns them. K1 calls it in the
/// transaction that marks an input operation `attempted` and writes its
/// `exposure_addrs`; the audit rows follow the commit
/// ([`crate::board_fence::audit_signed_out`]). `_now` is not used: no
/// session is spared for its age or its expiry.
///
/// # Errors
/// A database error.
pub(crate) fn sign_out_in_tx(
    tx: &Transaction<'_>,
    addrs: &[String],
    sessions: &[(SessionKind, String)],
    _now: i64,
) -> Result<Vec<SignedOut>> {
    let list = serde_json::to_string(addrs)?;
    let mut out = Vec::new();
    for kind in [SessionKind::Owner, SessionKind::Member] {
        let hashes: Vec<&str> = sessions
            .iter()
            .filter(|(k, _)| *k == kind)
            .map(|(_, hash)| hash.as_str())
            .collect();
        let hashes = serde_json::to_string(&hashes)?;
        let mut stmt = tx.prepare(&format!(
            "DELETE FROM {} WHERE last_addr IN (SELECT value FROM json_each(?1))
                OR session_hash IN (SELECT value FROM json_each(?2))
             RETURNING session_hash",
            table(kind)
        ))?;
        let rows = stmt.query_map([&list, &hashes], |r| r.get::<_, String>(0))?;
        for hash in rows {
            out.push(SignedOut {
                kind,
                session_hash: hash?,
            });
        }
    }
    Ok(out)
}

/// True when `addr` is exposed at `now`, read inside `tx` (§7.6 rule 2, for
/// the creation of a session): the same rows and windows as
/// [`DeckStore::exposed_addresses`], never its cache. A session creation
/// calls it in the transaction that inserts the session, so it is
/// serialized with the attempted transaction of an input operation. No
/// address is never exposed.
///
/// # Errors
/// A database error.
pub(crate) fn exposed_in_tx(tx: &Transaction<'_>, addr: Option<&str>, now: i64) -> Result<bool> {
    let Some(addr) = addr else {
        return Ok(false);
    };
    Ok(read_exposed(tx, now)?.contains(addr))
}

impl DeckStore {
    /// The socket registry of the board fence.
    #[must_use]
    pub fn registry(&self) -> &Arc<Registry> {
        &self.inner.fence.registry
    }

    /// Records the canonical address of a browser session: both columns,
    /// only when `last_addr` differs or `last_addr_seen_at` is older than
    /// 10 s (§7.6 "Last address of a session").
    ///
    /// # Errors
    /// A database error.
    pub async fn touch_session_addr(
        &self,
        kind: SessionKind,
        session_hash: &str,
        addr: &str,
        now: i64,
    ) -> Result<()> {
        let sql = format!(
            "UPDATE {} SET last_addr = ?2, last_addr_seen_at = ?3
             WHERE session_hash = ?1
               AND (last_addr IS NOT ?2 OR last_addr_seen_at IS NULL
                    OR ?3 - last_addr_seen_at > ?4)",
            table(kind)
        );
        let (hash, addr) = (session_hash.to_owned(), addr.to_owned());
        call(&self.inner.db, move |conn| {
            conn.execute(&sql, params![hash, addr, now, TOUCH_EVERY_MS])?;
            Ok(())
        })
        .await
    }

    /// True while the row of a browser session exists (the check of a socket
    /// at `on_upgrade`).
    ///
    /// # Errors
    /// A database error.
    pub async fn session_row_exists(&self, kind: SessionKind, session_hash: &str) -> Result<bool> {
        let sql = format!(
            "SELECT COUNT(*) FROM {} WHERE session_hash = ?1",
            table(kind)
        );
        let hash = session_hash.to_owned();
        call(&self.inner.db, move |conn| {
            let n: i64 = conn.query_row(&sql, [hash], |r| r.get(0))?;
            Ok(n > 0)
        })
        .await
    }

    /// True when `addr` is exposed at `now`, read from the rows, never from
    /// the cache of [`Self::exposed_addresses`] (the admission of a socket:
    /// a read that begins after the registration sees every committed
    /// attempted transaction whose sweep missed that socket).
    ///
    /// # Errors
    /// A database error.
    pub async fn addr_exposed_now(&self, addr: &str, now: i64) -> Result<bool> {
        let addr = addr.to_owned();
        call(&self.inner.db, move |conn| {
            Ok(read_exposed(conn, now)?.contains(&addr))
        })
        .await
    }

    /// Deletes one browser session. True when it existed.
    ///
    /// # Errors
    /// A database error.
    pub async fn end_session_of(&self, kind: SessionKind, session_hash: String) -> Result<bool> {
        match kind {
            SessionKind::Owner => self.end_login(session_hash).await,
            SessionKind::Member => self.end_member_login(session_hash).await,
        }
    }

    /// The canonical addresses that are exposed at `now`: the union of the
    /// `exposure_addrs` of every input operation (`key_press`, `type_text`,
    /// `pointer_move`, `pointer_click`) that was `attempted` and whose end
    /// plus 60 s has not passed. The end is `finished_at + 5 s` when `finished_at` is set, else
    /// `attempted_at + 45 s`. The set is cached for 1 s; a malformed
    /// `exposure_addrs` counts as no address and logs a warning.
    ///
    /// # Errors
    /// A database error.
    pub async fn exposed_addresses(&self, now: i64) -> Result<HashSet<String>> {
        let cache = &self.inner.fence.exposure;
        if let Some(set) = cache.get(now) {
            return Ok(set);
        }
        let ticket = cache.ticket();
        let set = call(&self.inner.db, move |conn| read_exposed(conn, now)).await?;
        cache.put(ticket, now, &set);
        Ok(set)
    }

    /// Clears the cache of [`Self::exposed_addresses`]. K1 calls it after it
    /// writes an input operation row.
    pub fn invalidate_exposure(&self) {
        self.inner.fence.exposure.invalidate();
    }

    /// The station of `fleetdeck up`: the local station of this deck, if it
    /// is known yet.
    #[must_use]
    pub fn local_station(&self) -> Option<StationId> {
        self.local_cell().get().cloned()
    }

    /// Sets the local station. The first value stays.
    pub fn set_local_station(&self, id: StationId) {
        // A second value is not an error: the local station does not change.
        let _ = self.local_cell().set(id);
    }

    /// Reads and writes the local station through `cell`, the cell that
    /// `Board.local_station` shares (`fleetdeck up` sets it after the
    /// pairing). A value that is set already moves to `cell`.
    pub fn share_local_station(&self, cell: Arc<OnceLock<StationId>>) {
        let mut slot = self
            .inner
            .fence
            .local_station
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some(id) = slot.get() {
            let _ = cell.set(id.clone());
        }
        *slot = cell;
    }

    fn local_cell(&self) -> Arc<OnceLock<StationId>> {
        Arc::clone(
            &self
                .inner
                .fence
                .local_station
                .lock()
                .unwrap_or_else(PoisonError::into_inner),
        )
    }
}
