//! The station registry: pins, grants, link direction and the revocation
//! generation (PLAN §8.5, ADR-004 §9.5, G5).
//!
//! - **One transaction per change.** Pairing stores the pin (key and role),
//!   the grants, the name and the dial address in one row. A revocation
//!   deletes the row (the station's sessions and timeline go with it) and
//!   increments `revocation_generation` in the same transaction. A rotation
//!   moves the key of the row (the `station_id` stays, DL-049) and
//!   increments the generation.

use std::collections::BTreeSet;
use std::fmt::Write as _;

use fleetdeck_core::identity::PeerKey;
use fleetdeck_proto::link::Scope;
use fleetdeck_proto::{IncarnationId, StationId};
use rusqlite::{Connection, OptionalExtension, Row, params};

use super::{DeckError, DeckStore, Result, bump_generation, call, to_u64};

/// Which side opens the link.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// The deck dials the station's listen address.
    DeckDials,
    /// The station dials the deck (a station behind NAT).
    StationDials,
}

impl Direction {
    fn name(self) -> &'static str {
        match self {
            Self::DeckDials => "deck_dials",
            Self::StationDials => "station_dials",
        }
    }

    fn from_name(text: &str) -> Result<Self> {
        match text {
            "deck_dials" => Ok(Self::DeckDials),
            "station_dials" => Ok(Self::StationDials),
            other => Err(DeckError::Corrupt(format!("direction `{other}`"))),
        }
    }
}

/// One paired station.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StationRecord {
    pub station_id: StationId,
    pub name: String,
    pub key: PeerKey,
    /// The scopes the deck grants to the station's requests.
    pub scopes: BTreeSet<Scope>,
    /// The address the deck dials (`DeckDials` only).
    pub dial_url: Option<String>,
    pub direction: Direction,
    pub paired_at: i64,
    pub last_seen: Option<i64>,
    /// The station incarnation of the read model (`None` before the first
    /// snapshot).
    pub incarnation: Option<IncarnationId>,
    /// The highest applied `station_seq` of that incarnation.
    pub cursor: u64,
}

/// What pairing stores for a new station.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewStation {
    pub key: PeerKey,
    pub name: String,
    pub scopes: BTreeSet<Scope>,
    pub dial_url: Option<String>,
}

/// Lower-case hex of a key.
#[must_use]
pub fn key_hex(key: &PeerKey) -> String {
    key.as_bytes()
        .iter()
        .fold(String::with_capacity(64), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
}

pub(crate) fn key_from_hex(text: &str) -> Result<PeerKey> {
    let bad = || DeckError::Corrupt(format!("station key `{text}`"));
    if text.len() != 64 || !text.is_ascii() {
        return Err(bad());
    }
    let mut out = [0_u8; 32];
    for (i, byte) in out.iter_mut().enumerate() {
        let pair = text.get(2 * i..2 * i + 2).ok_or_else(bad)?;
        *byte = u8::from_str_radix(pair, 16).map_err(|_| bad())?;
    }
    PeerKey::from_bytes(out).map_err(|_| bad())
}

const COLUMNS: &str = "station_id, name, key, scopes, dial_url, direction, paired_at, last_seen,
     incarnation_id, cursor";

fn record(row: &Row<'_>) -> rusqlite::Result<RawRecord> {
    Ok(RawRecord {
        station_id: row.get(0)?,
        name: row.get(1)?,
        key: row.get(2)?,
        scopes: row.get(3)?,
        dial_url: row.get(4)?,
        direction: row.get(5)?,
        paired_at: row.get(6)?,
        last_seen: row.get(7)?,
        incarnation: row.get(8)?,
        cursor: row.get(9)?,
    })
}

struct RawRecord {
    station_id: String,
    name: String,
    key: String,
    scopes: String,
    dial_url: Option<String>,
    direction: String,
    paired_at: i64,
    last_seen: Option<i64>,
    incarnation: Option<String>,
    cursor: i64,
}

impl RawRecord {
    fn parse(self) -> Result<StationRecord> {
        Ok(StationRecord {
            station_id: StationId::from(self.station_id),
            name: self.name,
            key: key_from_hex(&self.key)?,
            scopes: serde_json::from_str(&self.scopes)?,
            dial_url: self.dial_url,
            direction: Direction::from_name(&self.direction)?,
            paired_at: self.paired_at,
            last_seen: self.last_seen,
            incarnation: self.incarnation.map(IncarnationId::from),
            cursor: to_u64(self.cursor)?,
        })
    }
}

pub(crate) fn station_by_id(conn: &Connection, id: &StationId) -> Result<Option<StationRecord>> {
    conn.query_row(
        &format!("SELECT {COLUMNS} FROM stations WHERE station_id = ?1"),
        [id.as_str()],
        record,
    )
    .optional()?
    .map(RawRecord::parse)
    .transpose()
}

impl DeckStore {
    /// Stores the pin, grants, name and dial address of a new station in
    /// one transaction and returns its record. A key that is already paired
    /// gets the new grants and address and keeps its id and name.
    ///
    /// # Errors
    /// [`DeckError::NameTaken`] when another station has the name, or a
    /// database error.
    pub async fn pin_station(&self, new: NewStation) -> Result<StationRecord> {
        let now = self.now_ms();
        let out = call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            let hex = key_hex(&new.key);
            let taken: Option<String> = tx
                .query_row(
                    "SELECT key FROM stations WHERE name = ?1 AND key <> ?2",
                    params![new.name, hex],
                    |r| r.get(0),
                )
                .optional()?;
            if taken.is_some() {
                return Err(DeckError::NameTaken(new.name));
            }
            let direction = if new.dial_url.is_some() {
                Direction::DeckDials
            } else {
                Direction::StationDials
            };
            let scopes = serde_json::to_string(&new.scopes)?;
            let existing: Option<String> = tx
                .query_row(
                    "SELECT station_id FROM stations WHERE key = ?1",
                    [&hex],
                    |r| r.get(0),
                )
                .optional()?;
            let id = if let Some(id) = existing {
                tx.execute(
                    "UPDATE stations SET scopes = ?2, dial_url = ?3, direction = ?4
                     WHERE station_id = ?1",
                    params![id, scopes, new.dial_url, direction.name()],
                )?;
                // A new pairing replaces the approval that made the old one.
                tx.execute(
                    "DELETE FROM pairing_completions WHERE station_key = ?1",
                    [&hex],
                )?;
                id
            } else {
                let fingerprint = new.key.fingerprint().to_string();
                tx.execute(
                    "INSERT INTO stations (station_id, name, key, fingerprint, role, scopes,
                         dial_url, direction, paired_at)
                     VALUES (?1, ?2, ?3, ?1, 'station', ?4, ?5, ?6, ?7)",
                    params![
                        fingerprint,
                        new.name,
                        hex,
                        scopes,
                        new.dial_url,
                        direction.name(),
                        now,
                    ],
                )?;
                fingerprint
            };
            let rec = station_by_id(&tx, &StationId::from(id))?
                .ok_or_else(|| DeckError::Corrupt("the pinned station is missing".to_owned()))?;
            tx.commit()?;
            Ok(rec)
        })
        .await?;
        self.board_changed();
        Ok(out)
    }

    /// Removes the station with `key` (its pin, grants, sessions and
    /// timeline) and increments the revocation generation, in one
    /// transaction. Returns the removed record.
    ///
    /// # Errors
    /// A database error.
    pub async fn revoke_station(&self, key: PeerKey) -> Result<Option<StationRecord>> {
        let now = self.now_ms();
        let out = call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            let found = tx
                .query_row(
                    &format!("SELECT {COLUMNS} FROM stations WHERE key = ?1"),
                    [key_hex(&key)],
                    record,
                )
                .optional()?
                .map(RawRecord::parse)
                .transpose()?;
            if let Some(rec) = &found {
                super::fleet::block_unlisted_tasks(&tx, &rec.station_id, &BTreeSet::new(), now)?;
                super::review::delete_station(&tx, &rec.station_id)?;
                tx.execute(
                    "DELETE FROM stations WHERE station_id = ?1",
                    [rec.station_id.as_str()],
                )?;
                // The approval record goes with its station.
                tx.execute(
                    "DELETE FROM pairing_completions WHERE station_key = ?1",
                    [key_hex(&key)],
                )?;
                super::commands::fail_station(&tx, &rec.station_id, now)?;
                bump_generation(&tx)?;
            }
            tx.commit()?;
            Ok(found)
        })
        .await?;
        if out.is_some() {
            self.commands_changed();
            self.board_changed();
        }
        Ok(out)
    }

    /// Gives the row of `old` to `new` (the id, name, grants, address and
    /// cursor stay; G5) and increments the revocation generation, in one
    /// transaction.
    ///
    /// # Errors
    /// [`DeckError::UnknownStation`] when `old` is not paired, or a database
    /// error.
    pub async fn rotate_station(&self, old: PeerKey, new: PeerKey) -> Result<()> {
        call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            let moved = tx.execute(
                "UPDATE stations SET key = ?2, fingerprint = ?3 WHERE key = ?1",
                params![key_hex(&old), key_hex(&new), new.fingerprint().to_string()],
            )?;
            if moved == 0 {
                return Err(DeckError::UnknownStation(old.fingerprint().short()));
            }
            tx.execute(
                "DELETE FROM pairing_completions WHERE station_key = ?1",
                [key_hex(&old)],
            )?;
            bump_generation(&tx)?;
            tx.commit()?;
            Ok(())
        })
        .await?;
        self.board_changed();
        Ok(())
    }

    /// Every paired station, by name.
    ///
    /// # Errors
    /// A database error.
    pub async fn stations(&self) -> Result<Vec<StationRecord>> {
        call(&self.inner.db, |conn| {
            let mut stmt =
                conn.prepare(&format!("SELECT {COLUMNS} FROM stations ORDER BY name"))?;
            let rows = stmt.query_map([], record)?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row?.parse()?);
            }
            Ok(out)
        })
        .await
    }

    /// One station by id.
    ///
    /// # Errors
    /// A database error.
    pub async fn station(&self, id: StationId) -> Result<Option<StationRecord>> {
        call(&self.inner.db, move |conn| station_by_id(conn, &id)).await
    }

    /// The station with `name` or with this id (the CLI accepts both).
    ///
    /// # Errors
    /// A database error.
    pub async fn find_station(&self, name_or_id: String) -> Result<Option<StationRecord>> {
        call(&self.inner.db, move |conn| {
            conn.query_row(
                &format!("SELECT {COLUMNS} FROM stations WHERE name = ?1 OR station_id = ?1"),
                [name_or_id],
                record,
            )
            .optional()?
            .map(RawRecord::parse)
            .transpose()
        })
        .await
    }

    /// The station that holds `key` now.
    ///
    /// # Errors
    /// A database error.
    pub async fn station_by_key(&self, key: PeerKey) -> Result<Option<StationRecord>> {
        call(&self.inner.db, move |conn| {
            conn.query_row(
                &format!("SELECT {COLUMNS} FROM stations WHERE key = ?1"),
                [key_hex(&key)],
                record,
            )
            .optional()?
            .map(RawRecord::parse)
            .transpose()
        })
        .await
    }

    /// Gives the station a new name.
    ///
    /// # Errors
    /// [`DeckError::NameTaken`], [`DeckError::UnknownStation`], or a
    /// database error.
    pub async fn rename_station(&self, id: StationId, name: String) -> Result<()> {
        call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            let taken: Option<String> = tx
                .query_row(
                    "SELECT station_id FROM stations WHERE name = ?1 AND station_id <> ?2",
                    params![name, id.as_str()],
                    |r| r.get(0),
                )
                .optional()?;
            if taken.is_some() {
                return Err(DeckError::NameTaken(name));
            }
            let n = tx.execute(
                "UPDATE stations SET name = ?2 WHERE station_id = ?1",
                params![id.as_str(), name],
            )?;
            if n == 0 {
                return Err(DeckError::UnknownStation(id.to_string()));
            }
            tx.commit()?;
            Ok(())
        })
        .await?;
        self.board_changed();
        Ok(())
    }

    /// Whether a station other than `except` has the name `name`.
    ///
    /// # Errors
    /// A database error.
    pub async fn station_name_taken(&self, name: String, except: StationId) -> Result<bool> {
        call(&self.inner.db, move |conn| {
            let taken: Option<String> = conn
                .query_row(
                    "SELECT station_id FROM stations WHERE name = ?1 AND station_id <> ?2",
                    params![name, except.as_str()],
                    |r| r.get(0),
                )
                .optional()?;
            Ok(taken.is_some())
        })
        .await
    }

    /// Records that the station's link was up now.
    ///
    /// # Errors
    /// A database error.
    pub async fn touch_station(&self, id: StationId) -> Result<()> {
        let now = self.now_ms();
        call(&self.inner.db, move |conn| {
            conn.execute(
                "UPDATE stations SET last_seen = ?2 WHERE station_id = ?1",
                params![id.as_str(), now],
            )?;
            Ok(())
        })
        .await?;
        self.board_changed();
        Ok(())
    }
}
