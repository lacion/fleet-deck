//! The pairing completion records (DL-178, tailnet design 5.4).
//!
//! The owner's approval of a pairing request commits, in **one**
//! transaction, the station (key, grants, name) and an immutable completion
//! record with the whole approval context. If the answer to the station is
//! lost, the station asks again with `PairResume` and the deck re-signs
//! that context from the record. A record goes away with its station (a
//! removal, a re-pairing or a key rotation delete it in their own
//! transactions, see `stations.rs`).
//!
//! The commit never calls `pin_station`: a key that is already paired is
//! refused, never changed.

use std::fmt::Write as _;

use fleetdeck_core::identity::PeerKey;
use fleetdeck_link::pair_hub::{CommitError, Completion};
use rusqlite::{OptionalExtension, params};

use super::stations::{key_from_hex, key_hex};
use super::{DeckError, DeckStore, Result, call, to_i64, to_u64};

fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

fn unhex32(text: &str) -> Result<[u8; 32]> {
    let bad = || DeckError::Corrupt("a completion record holds a bad value".to_owned());
    if text.len() != 64 || !text.is_ascii() {
        return Err(bad());
    }
    let mut out = [0u8; 32];
    for (i, byte) in out.iter_mut().enumerate() {
        let pair = text.get(2 * i..2 * i + 2).ok_or_else(bad)?;
        *byte = u8::from_str_radix(pair, 16).map_err(|_| bad())?;
    }
    Ok(out)
}

impl DeckStore {
    /// Stores the station of an approved request and its completion record
    /// in one transaction. The station gets `granted_scopes`, no dial
    /// address (it dials the deck) and the requested name.
    ///
    /// # Errors
    /// [`CommitError::AlreadyPaired`] when the key is a paired station,
    /// [`CommitError::NameTaken`] when another station has the name,
    /// [`CommitError::RequestIdConflict`] when the request id has a record,
    /// [`CommitError::Store`] for a database error. Nothing is written
    /// then.
    pub async fn commit_pairing(&self, c: Completion) -> std::result::Result<(), CommitError> {
        let now = self.now_ms();
        let stored = call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            let hex_key = key_hex(&c.station_key);
            let paired: Option<i64> = tx
                .query_row("SELECT 1 FROM stations WHERE key = ?1", [&hex_key], |r| {
                    r.get(0)
                })
                .optional()?;
            if paired.is_some() {
                return Ok(Err(CommitError::AlreadyPaired));
            }
            let named: Option<i64> = tx
                .query_row(
                    "SELECT 1 FROM stations WHERE name = ?1",
                    [&c.requested_name],
                    |r| r.get(0),
                )
                .optional()?;
            if named.is_some() {
                return Ok(Err(CommitError::NameTaken(c.requested_name.clone())));
            }
            let used: Option<i64> = tx
                .query_row(
                    "SELECT 1 FROM pairing_completions WHERE request_id = ?1",
                    [&c.request_id],
                    |r| r.get(0),
                )
                .optional()?;
            if used.is_some() {
                return Ok(Err(CommitError::RequestIdConflict));
            }
            let fingerprint = c.station_key.fingerprint().to_string();
            let granted = serde_json::to_string(&c.granted_scopes)?;
            tx.execute(
                "INSERT INTO stations (station_id, name, key, fingerprint, role, scopes,
                     dial_url, direction, paired_at)
                 VALUES (?1, ?2, ?3, ?1, 'station', ?4, NULL, 'station_dials', ?5)",
                params![fingerprint, c.requested_name, hex_key, granted, now],
            )?;
            tx.execute(
                "INSERT INTO pairing_completions (request_id, station_key, deck_key,
                     request_digest, nonce, requested_name, offered_scopes, granted_scopes,
                     window_generation, approved_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    c.request_id,
                    hex_key,
                    key_hex(&c.deck_key),
                    hex(&c.request_digest),
                    hex(&c.nonce),
                    c.requested_name,
                    serde_json::to_string(&c.offered_scopes)?,
                    granted,
                    to_i64(c.window_generation)?,
                    c.approved_at,
                ],
            )?;
            tx.commit()?;
            Ok(Ok(()))
        })
        .await
        .map_err(|e| CommitError::Store(e.to_string()))?;
        if stored.is_ok() {
            self.board_changed();
        }
        stored
    }

    /// The completion record of `request_id`.
    ///
    /// # Errors
    /// A database error, or a record that does not parse.
    pub async fn pairing_completion(&self, request_id: String) -> Result<Option<Completion>> {
        call(&self.inner.db, move |conn| {
            let row = conn
                .query_row(
                    "SELECT request_id, station_key, deck_key, request_digest, nonce,
                            requested_name, offered_scopes, granted_scopes, window_generation,
                            approved_at
                     FROM pairing_completions WHERE request_id = ?1",
                    [&request_id],
                    |r| {
                        Ok((
                            r.get::<_, String>(0)?,
                            r.get::<_, String>(1)?,
                            r.get::<_, String>(2)?,
                            r.get::<_, String>(3)?,
                            r.get::<_, String>(4)?,
                            r.get::<_, String>(5)?,
                            r.get::<_, String>(6)?,
                            r.get::<_, String>(7)?,
                            r.get::<_, i64>(8)?,
                            r.get::<_, i64>(9)?,
                        ))
                    },
                )
                .optional()?;
            let Some((id, station, deck, digest, nonce, name, offered, granted, generation, at)) =
                row
            else {
                return Ok(None);
            };
            let key = |text: &str| -> Result<PeerKey> { key_from_hex(text) };
            Ok(Some(Completion {
                request_id: id,
                station_key: key(&station)?,
                deck_key: key(&deck)?,
                request_digest: unhex32(&digest)?,
                nonce: unhex32(&nonce)?,
                requested_name: name,
                offered_scopes: serde_json::from_str(&offered)?,
                granted_scopes: serde_json::from_str(&granted)?,
                window_generation: to_u64(generation)?,
                approved_at: at,
            }))
        })
        .await
    }
}
