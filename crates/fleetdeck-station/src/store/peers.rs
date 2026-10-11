//! The station's paired deck keys: pins, grants and the revocation
//! generation (PLAN §8.5, ADR-004 §9.5, G5).
//!
//! - **One transaction per change.** Pairing stores the pin (the key and its
//!   role) and the grants in one row. A revocation deletes that row (the
//!   deck's acknowledgement goes with it) and increments the persistent
//!   `revocation_generation` in the same transaction. A rotation moves the
//!   row, its grants and its acknowledgement to the new key, deletes the old
//!   key and increments the generation.
//! - **The decision transaction re-checks** the caller's pin and grant
//!   ([`granted`]), so a command that was queued on a link whose key was
//!   revoked is refused (`forbidden`).
//!
//! The `pairing` table keeps the deck key (64 hex digits) in `deck_key`.
//! Rows of the Phase 2 tests (`pair_deck`) have no fingerprint and grant
//! nothing.

use std::collections::BTreeSet;

use fleetdeck_core::identity::{Fingerprint, PeerKey};
use fleetdeck_link::tls::Role;
use fleetdeck_proto::link::Scope;
use rusqlite::{Connection, OptionalExtension, params};

use super::{Result, Store, StoreError, call};

/// One paired key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairedKey {
    pub key: PeerKey,
    pub role: Role,
    pub scopes: BTreeSet<Scope>,
    /// The address that this station dials, if it dials the deck.
    pub dial_url: Option<String>,
}

/// Lower-case hex of a key.
#[must_use]
pub fn key_hex(key: &PeerKey) -> String {
    use std::fmt::Write;
    key.as_bytes()
        .iter()
        .fold(String::with_capacity(64), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
}

fn key_from_hex(text: &str) -> Option<PeerKey> {
    if text.len() != 64 || !text.is_ascii() {
        return None;
    }
    let mut out = [0_u8; 32];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(text.get(2 * i..2 * i + 2)?, 16).ok()?;
    }
    PeerKey::from_bytes(out).ok()
}

fn role_name(role: Role) -> &'static str {
    match role {
        Role::Deck => "deck",
        Role::Station => "station",
    }
}

fn role_from(text: &str) -> Result<Role> {
    match text {
        "deck" => Ok(Role::Deck),
        "station" => Ok(Role::Station),
        other => Err(StoreError::Corrupt(format!("pairing role `{other}`"))),
    }
}

/// True when `fingerprint` is paired and holds `scope`. Runs inside the
/// caller's (decision) transaction.
pub(super) fn granted(conn: &Connection, fingerprint: &Fingerprint, scope: Scope) -> Result<bool> {
    let scopes: Option<String> = conn
        .query_row(
            "SELECT scopes FROM pairing WHERE fingerprint = ?1",
            [fingerprint.to_string()],
            |r| r.get(0),
        )
        .optional()?;
    let Some(scopes) = scopes else {
        return Ok(false);
    };
    let scopes: BTreeSet<Scope> = serde_json::from_str(&scopes)?;
    Ok(scopes.contains(&scope))
}

fn bump_generation(conn: &Connection) -> Result<u64> {
    let n: i64 = conn.query_row(
        "UPDATE meta SET revocation_generation = revocation_generation + 1 WHERE id = 1
         RETURNING revocation_generation",
        [],
        |r| r.get(0),
    )?;
    super::to_u64(n)
}

/// Replaces `old` by `new` inside the caller's transaction: the new key
/// gets the role, the grants, the dial address and the acknowledgement of
/// the old key and nothing else; the old key is removed and the revocation
/// generation grows (G5).
///
/// # Errors
/// [`StoreError::UnknownDeck`] when `old` is not paired, or a database
/// error.
pub fn rotate_in(conn: &Connection, old: &PeerKey, new: &PeerKey, now: i64) -> Result<()> {
    let old_hex = key_hex(old);
    let new_hex = key_hex(new);
    let copied = conn.execute(
        "INSERT INTO pairing (deck_key, deck_url, paired_at, fingerprint, role, scopes)
         SELECT ?2, deck_url, ?3, ?4, role, scopes FROM pairing WHERE deck_key = ?1",
        params![old_hex, new_hex, now, new.fingerprint().to_string()],
    )?;
    if copied == 0 {
        return Err(StoreError::UnknownDeck(old.fingerprint().short()));
    }
    conn.execute(
        "UPDATE deck_acks SET deck_key = ?2 WHERE deck_key = ?1",
        params![old_hex, new_hex],
    )?;
    conn.execute("DELETE FROM pairing WHERE deck_key = ?1", [old_hex])?;
    bump_generation(conn)?;
    Ok(())
}

impl Store {
    /// Stores the pin and the grants of a paired key (one transaction). A
    /// key that is already paired gets the new role, grants and address.
    ///
    /// # Errors
    /// A database error.
    pub async fn pin_peer(&self, peer: PairedKey) -> Result<()> {
        let now = self.now();
        call(&self.inner.db, move |conn| {
            conn.execute(
                "INSERT INTO pairing (deck_key, deck_url, paired_at, fingerprint, role, scopes)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT (deck_key) DO UPDATE SET deck_url = excluded.deck_url,
                     fingerprint = excluded.fingerprint, role = excluded.role,
                     scopes = excluded.scopes",
                params![
                    key_hex(&peer.key),
                    peer.dial_url,
                    now,
                    peer.key.fingerprint().to_string(),
                    role_name(peer.role),
                    serde_json::to_string(&peer.scopes)?,
                ],
            )?;
            Ok(())
        })
        .await
    }

    /// Removes the pin, the grants and the acknowledgement of `key` and
    /// increments the revocation generation, in one transaction. Returns
    /// whether the key was paired.
    ///
    /// # Errors
    /// A database error.
    pub async fn revoke_peer(&self, key: PeerKey) -> Result<bool> {
        call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            let removed = tx.execute("DELETE FROM pairing WHERE deck_key = ?1", [key_hex(&key)])?;
            if removed > 0 {
                bump_generation(&tx)?;
            }
            tx.commit()?;
            Ok(removed > 0)
        })
        .await
    }

    /// Every paired key (the node loads its pins and grants at start).
    ///
    /// # Errors
    /// A database or JSON error.
    pub async fn paired_keys(&self) -> Result<Vec<PairedKey>> {
        call(&self.inner.db, move |conn| {
            let mut stmt = conn.prepare(
                "SELECT deck_key, role, scopes, deck_url FROM pairing
                 WHERE fingerprint IS NOT NULL ORDER BY paired_at",
            )?;
            let rows = stmt
                .query_map([], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, Option<String>>(3)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows.into_iter()
                .map(|(key, role, scopes, dial_url)| {
                    Ok(PairedKey {
                        key: key_from_hex(&key)
                            .ok_or_else(|| StoreError::Corrupt(format!("pairing key `{key}`")))?,
                        role: role_from(&role)?,
                        scopes: serde_json::from_str(&scopes)?,
                        dial_url,
                    })
                })
                .collect()
        })
        .await
    }

    /// The persistent revocation generation (grows with each revocation and
    /// rotation).
    ///
    /// # Errors
    /// A database error.
    pub async fn revocation_generation(&self) -> Result<u64> {
        call(&self.inner.db, move |conn| {
            let n: i64 = conn.query_row(
                "SELECT revocation_generation FROM meta WHERE id = 1",
                [],
                |r| r.get(0),
            )?;
            super::to_u64(n)
        })
        .await
    }

    /// The database handle, for the link epochs of this node.
    #[must_use]
    pub fn db(&self) -> fleetdeck_core::db::Db {
        self.inner.db.clone()
    }
}

#[cfg(test)]
#[path = "peers_tests.rs"]
mod tests;
