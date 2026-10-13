//! `deck.db` migrations (PLAN §10). Each entry is one transactional
//! migration; the list only grows (a shipped migration never changes).
//!
//! V1 (M2 P3-6a): `meta` (incarnation, revocation generation), the station
//! registry with its pins, grants and resync cursor, the session read model
//! and the condensed timeline. V2: link epochs (G1). V3 (P3-7): the
//! command router's `commands` table (G3). V4 (P3-7b): the outbox of
//! deck-owned intent (A5) and the receipts of `coordinator.call` (G7). Auth
//! and audit tables follow in P3-8 (V5). V6 (M3 6-6): the plan
//! library. V7 (M3 6-2, 6-4): mail and tasks. V8 (M3 6-7): file touches.

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

/// The command router (G3, PLAN §8.4). `envelope` is the stored envelope:
/// it never changes, and every send sends these bytes. `attempts` counts
/// the sends; the first send sets `sent_at`.
const V3: &str = "
CREATE TABLE commands (
    command_id   TEXT PRIMARY KEY,
    station_id   TEXT NOT NULL,
    method       TEXT NOT NULL,
    origin       TEXT NOT NULL,
    envelope     TEXT NOT NULL,
    retry_until  INTEGER NOT NULL,
    state        TEXT NOT NULL CHECK (state IN ('pending', 'done', 'failed', 'ambiguous')),
    attempts     INTEGER NOT NULL DEFAULT 0,
    sent_at      INTEGER,
    receipt      TEXT,
    reason       TEXT,
    created_at   INTEGER NOT NULL,
    updated_at   INTEGER NOT NULL
) STRICT;
CREATE INDEX commands_pending ON commands (station_id, created_at) WHERE state = 'pending';
CREATE INDEX commands_by_station ON commands (station_id, created_at);
";

/// The outbox (A5) and the coordinator receipts (G7). An outbox item is
/// deck-owned intent for one station; each delivery attempt is one routed
/// command (`command_id`, in the same transaction). The receipts table
/// follows the station's `commands` table: same id + method + caller +
/// payload hash returns the stored receipt.
const V4: &str = "
CREATE TABLE outbox (
    outbox_id    TEXT PRIMARY KEY,
    station_id   TEXT NOT NULL,
    kind         TEXT NOT NULL CHECK (kind IN ('command')),
    origin       TEXT NOT NULL,
    payload      TEXT NOT NULL,
    state        TEXT NOT NULL CHECK (state IN ('queued', 'sending', 'delivered', 'dead')),
    command_id   TEXT UNIQUE REFERENCES commands (command_id),
    attempts     INTEGER NOT NULL DEFAULT 0,
    next_at      INTEGER NOT NULL,
    expires_at   INTEGER NOT NULL,
    reason       TEXT,
    created_at   INTEGER NOT NULL,
    updated_at   INTEGER NOT NULL
) STRICT;
CREATE INDEX outbox_due ON outbox (station_id, next_at) WHERE state = 'queued';
CREATE TABLE coordinator_receipts (
    command_id     TEXT PRIMARY KEY,
    station_id     TEXT NOT NULL,
    method         TEXT NOT NULL,
    caller         TEXT NOT NULL,
    payload_sha256 TEXT NOT NULL,
    receipt        TEXT NOT NULL,
    at             INTEGER NOT NULL
) STRICT;
";

/// Owner login and audit (P3-8; PLAN §9, A13, L15). Secrets are stored as
/// SHA-256 hashes (session cookies, login links, API tokens) or as an
/// argon2id PHC string (the password).
const V5: &str = "
CREATE TABLE owner (
    id              INTEGER PRIMARY KEY CHECK (id = 1),
    owner_id        TEXT NOT NULL,
    password_hash   TEXT,
    password_set_at INTEGER,
    created_at      INTEGER NOT NULL
) STRICT;
CREATE TABLE login_sessions (
    session_hash TEXT PRIMARY KEY,
    csrf_token   TEXT NOT NULL,
    method       TEXT NOT NULL CHECK (method IN ('password', 'link')),
    created_at   INTEGER NOT NULL,
    last_seen    INTEGER NOT NULL,
    expires_at   INTEGER NOT NULL
) STRICT;
CREATE TABLE login_links (
    token_hash TEXT PRIMARY KEY,
    created_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL
) STRICT;
CREATE TABLE api_tokens (
    name       TEXT PRIMARY KEY,
    token_hash TEXT NOT NULL UNIQUE,
    scopes     TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL,
    last_used  INTEGER
) STRICT;
CREATE TABLE audit (
    audit_id   INTEGER PRIMARY KEY AUTOINCREMENT,
    at         INTEGER NOT NULL,
    actor      TEXT NOT NULL,
    action     TEXT NOT NULL,
    target     TEXT,
    command_id TEXT,
    result     TEXT NOT NULL CHECK (result IN ('ok', 'refused', 'failed')),
    detail     TEXT
) STRICT;
";

/// The plan library (M3 6-6, DL-301): one row for each folded
/// `plan.proposed` event. No foreign key to `stations`: a plan is fleet
/// intent and stays when its station is removed or gets a new incarnation.
const V6: &str = "
CREATE TABLE plans (
    plan_key       TEXT    PRIMARY KEY,
    station_id     TEXT    NOT NULL,
    session_id     TEXT    NOT NULL,
    plan_id        TEXT    NOT NULL,
    turn_id        TEXT,
    session_title  TEXT,
    markdown       TEXT    NOT NULL,
    truncated      INTEGER NOT NULL CHECK (truncated IN (0, 1)),
    status         TEXT    NOT NULL CHECK (status IN ('proposed', 'executed', 'archived')),
    proposed_at    INTEGER NOT NULL,
    updated_at     INTEGER NOT NULL
) STRICT;
CREATE INDEX plans_by_time ON plans (proposed_at DESC);
";

/// Fleet mail (M3 6-2) and tasks with waitable completions (6-4). Each row
/// names the outbox item that delivers its text; the item is inserted in
/// the same transaction (A5).
const V7: &str = "
CREATE TABLE mail (
    mail_id    TEXT PRIMARY KEY,
    sender     TEXT NOT NULL,
    station_id TEXT NOT NULL,
    session_id TEXT NOT NULL,
    delivery   TEXT NOT NULL CHECK (delivery IN ('follow_up', 'steer')),
    text       TEXT NOT NULL,
    outbox_id  TEXT NOT NULL REFERENCES outbox (outbox_id),
    created_at INTEGER NOT NULL
) STRICT;
CREATE INDEX mail_by_session ON mail (station_id, session_id, created_at);
CREATE TABLE tasks (
    task_id      TEXT PRIMARY KEY,
    title        TEXT NOT NULL,
    creator      TEXT NOT NULL,
    station_id   TEXT NOT NULL,
    session_id   TEXT NOT NULL,
    state        TEXT NOT NULL CHECK (state IN ('open', 'done', 'failed')),
    result       TEXT,
    outbox_id    TEXT NOT NULL REFERENCES outbox (outbox_id),
    created_at   INTEGER NOT NULL,
    updated_at   INTEGER NOT NULL,
    completed_at INTEGER
) STRICT;
CREATE INDEX tasks_by_session ON tasks (station_id, session_id, created_at);
";

/// The conflict radar input (M3 6-7, DL-306; PLAN §10 `file_touches`): the
/// latest touch of a file by a session, keyed by the repository group.
/// Renumbered from V7 to V8 at the lane A merge (mail and tasks came first).
const V8: &str = "
CREATE TABLE file_touches (
    group_key      TEXT    NOT NULL,
    file           TEXT    NOT NULL,
    repo           TEXT,
    station_id     TEXT    NOT NULL,
    session_id     TEXT    NOT NULL,
    session_title  TEXT,
    path           TEXT    NOT NULL,
    touched_at     INTEGER NOT NULL,
    PRIMARY KEY (group_key, file, station_id, session_id)
) STRICT;
CREATE INDEX file_touches_by_time ON file_touches (touched_at);
";

/// Coordinator grants (M3 6-5, ADR-006 §4, PLAN §10 table `tokens`) and the
/// lineage of the sessions that a grant started (§6.3 `interrupt`/`stop`).
/// `spawn_used` is a durable lifetime counter (§6.4): receipt pruning never
/// changes it. Renumbered from V8 to V9 at the merge of lane C's V8.
const V9: &str = "
CREATE TABLE tokens (
    grant_id                TEXT    PRIMARY KEY,
    station_id              TEXT    NOT NULL,
    session_id              TEXT    NOT NULL,
    classes                 TEXT    NOT NULL,
    stations                TEXT    NOT NULL,
    spawn_cap               INTEGER NOT NULL CHECK (spawn_cap BETWEEN 0 AND 50),
    spawn_used              INTEGER NOT NULL DEFAULT 0 CHECK (spawn_used >= 0),
    max_modes               TEXT    NOT NULL,
    idle_ttl_ms             INTEGER NOT NULL CHECK (idle_ttl_ms BETWEEN 1 AND 86400000),
    created_at              INTEGER NOT NULL,
    expires_at              INTEGER NOT NULL,
    state                   TEXT    NOT NULL CHECK (state IN ('active', 'revoked', 'expired')),
    revoked_reason          TEXT,
    bound_generation        INTEGER,
    bound_native_id         TEXT,
    bound_provider_instance TEXT,
    binding_id              TEXT,
    last_used_at            INTEGER
) STRICT;
CREATE INDEX tokens_by_caller ON tokens (station_id, session_id, created_at);
CREATE TABLE coordinator_lineage (
    command_id      TEXT    PRIMARY KEY,
    grant_id        TEXT    NOT NULL REFERENCES tokens (grant_id),
    station_id      TEXT    NOT NULL,
    session_id      TEXT,
    start_outbox_id TEXT,
    created_at      INTEGER NOT NULL
) STRICT;
CREATE INDEX lineage_by_session ON coordinator_lineage (station_id, session_id);
";

/// The owner's pending renewal of an idle-expired grant (ADR-006 §5.5,
/// test 25b): the generation that the station must acknowledge with
/// `coordinator.bound`. `NULL` when no renewal waits.
const V10: &str = "ALTER TABLE tokens ADD COLUMN renew_generation INTEGER;";

/// The owner's start command of a grant (ADR-006 §4, DL-217): a grant
/// created with a session start has no session id until the station
/// names the session; a resend of the same start finds the same grant.
const V11: &str = "
ALTER TABLE tokens ADD COLUMN start_command_id TEXT;
CREATE UNIQUE INDEX tokens_by_start_command ON tokens (start_command_id)
    WHERE start_command_id IS NOT NULL;
";

/// The archived (cleared) sessions of the board (DL-149) and the ids of the
/// archive mutations that were applied (never pruned: a stale resend of an
/// old mutation must stay a no-op).
const V12: &str = "
CREATE TABLE archived_sessions (
    station_id TEXT    NOT NULL,
    session_id TEXT    NOT NULL,
    archived   INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    PRIMARY KEY (station_id, session_id)
);
CREATE TABLE archive_mutations (
    mutation_id TEXT    PRIMARY KEY,
    applied_at  INTEGER NOT NULL
);
";

/// The projects of the board (R3, DL-164): a repository on a station with
/// its forge and tracker bindings (JSON) and the prompts for sessions that
/// start from a ticket or a review. No token is ever stored here.
const V18: &str = "
CREATE TABLE projects (
    project_id      TEXT    PRIMARY KEY,
    name            TEXT    NOT NULL,
    station_id      TEXT    NOT NULL,
    path            TEXT    NOT NULL,
    forge           TEXT,
    trackers        TEXT    NOT NULL DEFAULT '[]',
    prompt_template TEXT,
    review_prompt   TEXT,
    created_at      INTEGER NOT NULL
);
CREATE INDEX projects_by_station ON projects (station_id);
";

/// The `deck.db` migrations.
#[must_use]
pub fn migrations() -> Migrations<'static> {
    Migrations::new(vec![
        M::up(V1),
        M::up(fleetdeck_link::LINK_EPOCHS_SQL),
        M::up(V3),
        M::up(V4),
        M::up(V5),
        M::up(V6),
        M::up(V7),
        M::up(V8),
        M::up(V9),
        M::up(V10),
        M::up(V11),
        M::up(V12),
        M::up(V18),
    ])
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_migrations_are_valid() {
        super::migrations().validate().unwrap();
    }
}
