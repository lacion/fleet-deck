//! `deck.db` migrations (PLAN §10). Each entry is one transactional
//! migration; the list only grows (a shipped migration never changes).
//!
//! V1 (M2 P3-6a): `meta` (incarnation, revocation generation), the station
//! registry with its pins, grants and resync cursor, the session read model
//! and the condensed timeline. V2: link epochs (G1). The command router,
//! outbox, auth and audit tables follow in P3-7 and P3-8.

use rusqlite_migration::{M, Migrations};

const V1: &str = "
CREATE TABLE meta (
    id                     INTEGER PRIMARY KEY CHECK (id = 1),
    incarnation_id         TEXT    NOT NULL,
    revocation_generation  INTEGER NOT NULL DEFAULT 0 CHECK (revocation_generation >= 0),
    identity_key_ref       TEXT,
    created_at             INTEGER NOT NULL
) STRICT;

-- One paired station: its pin (key, role), the grants the deck gives it,
-- the link direction and the resync cursor of its event stream (G4).
-- `station_id` is the fingerprint at pairing and stays when the key
-- rotates (DL-049).
CREATE TABLE stations (
    station_id      TEXT    PRIMARY KEY,
    name            TEXT    NOT NULL UNIQUE,
    key             TEXT    NOT NULL UNIQUE,
    fingerprint     TEXT    NOT NULL UNIQUE,
    role            TEXT    NOT NULL CHECK (role = 'station'),
    scopes          TEXT    NOT NULL,
    dial_url        TEXT,
    direction       TEXT    NOT NULL CHECK (direction IN ('deck_dials', 'station_dials')),
    paired_at       INTEGER NOT NULL,
    last_seen       INTEGER,
    incarnation_id  TEXT,
    cursor          INTEGER NOT NULL DEFAULT 0 CHECK (cursor >= 0)
) STRICT;

-- The read model: the station's SessionSummary projection (JSON).
CREATE TABLE sessions (
    station_id  TEXT    NOT NULL REFERENCES stations(station_id) ON DELETE CASCADE,
    session_id  TEXT    NOT NULL,
    summary     TEXT    NOT NULL,
    updated_at  INTEGER NOT NULL,
    PRIMARY KEY (station_id, session_id)
) STRICT;

-- The condensed per-session timeline for the board: every event except
-- the streaming deltas, capped per session (DL-049).
CREATE TABLE timeline (
    station_id   TEXT    NOT NULL REFERENCES stations(station_id) ON DELETE CASCADE,
    station_seq  INTEGER NOT NULL,
    session_id   TEXT    NOT NULL,
    session_seq  INTEGER NOT NULL,
    kind         TEXT    NOT NULL,
    envelope     TEXT    NOT NULL,
    PRIMARY KEY (station_id, station_seq)
) STRICT;

CREATE INDEX timeline_by_session ON timeline (station_id, session_id, station_seq);
";

/// The `deck.db` migrations.
#[must_use]
pub fn migrations() -> Migrations<'static> {
    Migrations::new(vec![M::up(V1), M::up(fleetdeck_link::LINK_EPOCHS_SQL)])
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_migrations_are_valid() {
        super::migrations().validate().unwrap();
    }
}
