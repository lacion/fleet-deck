//! `station.db` migrations (PLAN §10). Each entry is one transactional
//! migration; the list only grows (a shipped migration never changes).
//!
//! Phase 2 tables: `meta`, `pairing`, `deck_acks`, `sessions`, `events`,
//! `commands`, `shells`. The effect outbox and the runtime requests follow in
//! the next migration (M2 P2-5); the workspace tables in Phase 6.

use rusqlite_migration::{M, Migrations};

const V1: &str = "
CREATE TABLE meta (
    id                INTEGER PRIMARY KEY CHECK (id = 1),
    incarnation_id    TEXT    NOT NULL,
    station_seq_next  INTEGER NOT NULL CHECK (station_seq_next >= 1),
    -- The highest station_seq that retention removed (0: none).
    pruned_through    INTEGER NOT NULL DEFAULT 0,
    identity_key_ref  TEXT,
    created_at        INTEGER NOT NULL
) STRICT;

CREATE TABLE pairing (
    deck_key   TEXT PRIMARY KEY,
    deck_url   TEXT,
    paired_at  INTEGER NOT NULL
) STRICT;

-- The highest station_seq that a paired deck acknowledged.
CREATE TABLE deck_acks (
    deck_key     TEXT PRIMARY KEY REFERENCES pairing(deck_key) ON DELETE CASCADE,
    station_seq  INTEGER NOT NULL,
    at           INTEGER NOT NULL
) STRICT;

CREATE TABLE sessions (
    session_id               TEXT PRIMARY KEY,
    provider                 TEXT NOT NULL,
    provider_instance        TEXT NOT NULL,
    native_session_id        TEXT,
    cwd                      TEXT NOT NULL,
    worktree                 TEXT,
    mode                     TEXT,
    state                    TEXT NOT NULL,
    offline_approval_policy  TEXT NOT NULL,
    -- The last allocated session_seq.
    session_seq              INTEGER NOT NULL DEFAULT 0,
    -- The SessionSummary projection (JSON), folded in each event's transaction.
    summary                  TEXT NOT NULL,
    created_at               INTEGER NOT NULL,
    updated_at               INTEGER NOT NULL
) STRICT;

CREATE TABLE events (
    station_seq  INTEGER PRIMARY KEY,
    session_id   TEXT    NOT NULL REFERENCES sessions(session_id),
    session_seq  INTEGER NOT NULL,
    kind         TEXT    NOT NULL,
    payload      TEXT    NOT NULL,
    raw_ref      TEXT,
    at           INTEGER NOT NULL,
    UNIQUE (session_id, session_seq)
) STRICT;

-- Command receipts (§6.4, G3).
CREATE TABLE commands (
    command_id     TEXT PRIMARY KEY,
    method         TEXT NOT NULL,
    caller         TEXT NOT NULL,
    payload_sha256 TEXT NOT NULL,
    state          TEXT NOT NULL CHECK (state IN ('accepted', 'rejected')),
    station_seq    INTEGER,
    receipt        TEXT NOT NULL,
    at             INTEGER NOT NULL
) STRICT;
CREATE INDEX commands_at ON commands(at);

CREATE TABLE shells (
    shell_id    TEXT PRIMARY KEY,
    cwd         TEXT NOT NULL,
    state       TEXT NOT NULL,
    created_at  INTEGER NOT NULL
) STRICT;
";

/// The `station.db` migrations.
#[must_use]
pub fn migrations() -> Migrations<'static> {
    Migrations::new(vec![M::up(V1)])
}
