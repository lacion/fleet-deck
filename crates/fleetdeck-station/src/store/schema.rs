//! `station.db` migrations (PLAN §10). Each entry is one transactional
//! migration; the list only grows (a shipped migration never changes).
//!
//! V1 (M2 P2-4): `meta`, `pairing`, `deck_acks`, `sessions`, `events`,
//! `commands`, `shells`. V2 (M2 P2-5): `processes`, `effects`,
//! `runtime_requests` and the worker fence. V3 (M2 P3-5): pins, grants and
//! the revocation generation (G5). V4: link epochs (G1). V5 (M2 P3-5c):
//! the listen addresses that `station invite --listen` added (DL-048). The
//! workspace tables follow in Phase 6.

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

const V2: &str = "
-- The fence token of the current effect worker (PLAN §6.4). Each worker
-- start takes the next value; a claim or settle with an older token fails.
ALTER TABLE meta ADD COLUMN worker_fence INTEGER NOT NULL DEFAULT 0;

-- Provider processes (provider incarnations): one row per spawned process.
CREATE TABLE processes (
    process_incarnation  TEXT PRIMARY KEY,
    session_id           TEXT    NOT NULL REFERENCES sessions(session_id),
    pid                  INTEGER,
    state                TEXT    NOT NULL CHECK (state IN ('live', 'dead')),
    started_at           INTEGER NOT NULL,
    ended_at             INTEGER,
    reason               TEXT
) STRICT;
CREATE INDEX processes_session ON processes(session_id, state);

-- The effect outbox (§6.4). seq orders the worker.
CREATE TABLE effects (
    seq                  INTEGER PRIMARY KEY AUTOINCREMENT,
    effect_id            TEXT    NOT NULL UNIQUE,
    command_id           TEXT,
    session_id           TEXT    NOT NULL REFERENCES sessions(session_id),
    -- The target process; NULL for an effect that starts a process.
    process_incarnation  TEXT REFERENCES processes(process_incarnation),
    kind                 TEXT    NOT NULL,
    payload              TEXT    NOT NULL,
    -- The provider evidence that confirms the effect (input id, request id, mode).
    correlation          TEXT,
    state                TEXT    NOT NULL CHECK (state IN
        ('queued', 'claimed', 'confirmed', 'retired_not_dispatched', 'outcome_unknown')),
    fence_token          INTEGER,
    reason               TEXT,
    created_at           INTEGER NOT NULL,
    updated_at           INTEGER NOT NULL
) STRICT;
CREATE INDEX effects_state ON effects(state, seq);
CREATE INDEX effects_correlation ON effects(session_id, correlation);

-- Runtime requests (approvals, questions) with the provider's option ids (L3).
CREATE TABLE runtime_requests (
    request_id           TEXT PRIMARY KEY,
    session_id           TEXT    NOT NULL REFERENCES sessions(session_id),
    process_incarnation  TEXT REFERENCES processes(process_incarnation),
    node_id              TEXT,
    kind                 TEXT    NOT NULL,
    options              TEXT    NOT NULL,
    response_channel     TEXT    NOT NULL,
    state                TEXT    NOT NULL CHECK (state IN ('open', 'resolved', 'expired')),
    -- The first answer (JSON AnswerReceipt): a repeated answer returns it.
    receipt              TEXT,
    -- deny_after (§6.5): the time of the automatic deny; NULL for wait.
    deny_at              INTEGER,
    requested_at         INTEGER NOT NULL,
    updated_at           INTEGER NOT NULL
) STRICT;
CREATE INDEX runtime_requests_open ON runtime_requests(state, deny_at);
";

const V3: &str = "
-- G5: the pin (key and role) and the grants of each paired key, and a
-- revocation generation that every revocation and rotation increments.
ALTER TABLE meta ADD COLUMN revocation_generation INTEGER NOT NULL DEFAULT 0;
ALTER TABLE pairing ADD COLUMN fingerprint TEXT;
ALTER TABLE pairing ADD COLUMN role TEXT NOT NULL DEFAULT 'deck'
    CHECK (role IN ('deck', 'station'));
-- JSON array of scope names.
ALTER TABLE pairing ADD COLUMN scopes TEXT NOT NULL DEFAULT '[]';
CREATE UNIQUE INDEX pairing_fingerprint ON pairing(fingerprint);
";

const V5: &str = "
-- DL-048: a listen address that `station invite --listen` opened; the
-- station listens on it again after a restart (with `station.toml` listen).
CREATE TABLE listen_addrs (
    addr      TEXT PRIMARY KEY,
    added_at  INTEGER NOT NULL
) STRICT;
";

/// The `station.db` migrations. V4 is the link epochs table of
/// `fleetdeck-link` (G1).
#[must_use]
pub fn migrations() -> Migrations<'static> {
    Migrations::new(vec![
        M::up(V1),
        M::up(V2),
        M::up(V3),
        M::up(fleetdeck_link::LINK_EPOCHS_SQL),
        M::up(V5),
    ])
}
