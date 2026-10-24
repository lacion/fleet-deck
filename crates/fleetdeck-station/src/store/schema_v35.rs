//! The V35 migration (R13 P6, task F1): the durable claim of each coordinator
//! desktop mutation on the target (`notify`, `preview_open`, `preview_close`)
//! and the coordinator context of a capture operation. A `desktop_ops` row is
//! kept for ever: a claimed operation never runs again after a restart or a
//! reconnect. Forward only; no existing row is rewritten. To reverse it, drop
//! `desktop_ops` and the `capture_ops.coordinator` column.

pub(super) const V35: &str = "
CREATE TABLE desktop_ops (               -- R13 P6: one row per coordinator desktop mutation, kept forever
    deck_fp        TEXT    NOT NULL,     -- the authenticated deck fingerprint (64 hex)
    operation_id   TEXT    NOT NULL,     -- the deck's operation ULID, equal to the authority action_id
    action_hash    TEXT    NOT NULL,
    kind           TEXT    NOT NULL CHECK (kind IN ('notify','preview_open','preview_close')),
    grant_id       TEXT    NOT NULL,
    caller_station TEXT    NOT NULL,
    caller_session TEXT    NOT NULL,
    state          TEXT    NOT NULL CHECK (state IN ('claimed','done','failed')),
    result         TEXT,
    code           TEXT,
    claimed_at     INTEGER NOT NULL,
    updated_at     INTEGER NOT NULL,
    PRIMARY KEY (deck_fp, operation_id)
) STRICT;
ALTER TABLE capture_ops ADD COLUMN coordinator TEXT;   -- CoordinatorContext JSON; NULL = owner
";
