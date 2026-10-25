//! The V41 migration (knobs K4, `docs/v2/design/knobs.md` section 3,
//! "Recording length, storage"): a recording can last up to 3600 seconds.
//! SQLite cannot change a CHECK, so `desktop_captures` and
//! `coordinator_desktop_ops` are rebuilt in the pattern of V38
//! (`coordinator_input_any_schema.rs`): a new table, an insert with explicit
//! columns, a drop, a rename and the indexes again. Every column, row, value,
//! other constraint and index stays as V32, V34, V35 and V38 made them; only
//! `requested_seconds` and `seconds` change from `BETWEEN 1 AND 120` to
//! `BETWEEN 1 AND 3600`. No table has a foreign key to either of them, and
//! neither has a foreign key to another table; the deferral of the check
//! (`defer_foreign_keys`) is as in V38.
//!
//! Forward only. To reverse it, rebuild the two tables with the old checks
//! after the rows over 120 seconds are gone.
//!
//! The deck list numbers it V41 because V40 (MCP P0) was already taken when
//! the knobs K4 work started.

pub(crate) const SQL: &str = "
PRAGMA defer_foreign_keys = ON;
CREATE TABLE desktop_captures_new (
    station_id        TEXT    NOT NULL,
    capture_id        TEXT    NOT NULL,
    action_hash       TEXT    NOT NULL,
    state             TEXT    NOT NULL,
    monitor           TEXT    NOT NULL,
    published         TEXT,
    indexed           INTEGER NOT NULL DEFAULT 0,
    code              TEXT,
    message           TEXT,
    created_at        INTEGER NOT NULL,
    updated_at        INTEGER NOT NULL,
    coordinator       TEXT,
    operation_kind    TEXT    NOT NULL DEFAULT 'screenshot'
        CHECK (operation_kind IN ('screenshot','recording')),
    requested_seconds INTEGER
        CHECK (requested_seconds IS NULL OR requested_seconds BETWEEN 1 AND 3600),
    owner_login       TEXT,
    link_epoch        TEXT,
    stop_intent       TEXT    NOT NULL DEFAULT 'none'
        CHECK (stop_intent IN ('none','save','discard')),
    stop_reason       TEXT,
    started_at        INTEGER,
    audited_terminal  INTEGER NOT NULL DEFAULT 0
        CHECK (audited_terminal IN (0,1)),
    PRIMARY KEY (station_id, capture_id)
) STRICT;
INSERT INTO desktop_captures_new (station_id, capture_id, action_hash, state, monitor, published, indexed, code, message, created_at, updated_at, coordinator, operation_kind, requested_seconds, owner_login, link_epoch, stop_intent, stop_reason, started_at, audited_terminal)
    SELECT station_id, capture_id, action_hash, state, monitor, published, indexed, code, message, created_at, updated_at, coordinator, operation_kind, requested_seconds, owner_login, link_epoch, stop_intent, stop_reason, started_at, audited_terminal
    FROM desktop_captures;
DROP TABLE desktop_captures;
ALTER TABLE desktop_captures_new RENAME TO desktop_captures;
CREATE INDEX desktop_captures_recording ON desktop_captures (station_id, state)
    WHERE operation_kind = 'recording';
CREATE TABLE coordinator_desktop_ops_new (
    operation_id TEXT PRIMARY KEY,
    grant_id TEXT NOT NULL,
    request_key TEXT NOT NULL,
    payload_hash TEXT NOT NULL,
    tool TEXT NOT NULL CHECK (tool IN ('wall_show','notify','preview_open','window_close','screenshot','record_monitor','key_press','type_text','pointer_move','pointer_click')),
    class TEXT NOT NULL CHECK (class IN ('desktop','capture','input')),
    binding_id TEXT NOT NULL,
    generation INTEGER NOT NULL,
    caller_station TEXT NOT NULL,
    caller_session TEXT NOT NULL,
    target_station TEXT NOT NULL,
    action_hash TEXT,
    resource TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('prepared','attempted','done','failed','unknown')),
    result TEXT,
    code TEXT,
    charged TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    seconds INTEGER CHECK (seconds IS NULL OR seconds BETWEEN 1 AND 3600),
    input_length INTEGER CHECK (input_length IS NULL OR input_length BETWEEN 0 AND 2000),
    attempted_at INTEGER,
    finished_at INTEGER,
    exposure_addrs TEXT,
    UNIQUE (grant_id, request_key)
) STRICT;
INSERT INTO coordinator_desktop_ops_new (operation_id, grant_id, request_key, payload_hash, tool, class, binding_id, generation, caller_station, caller_session, target_station, action_hash, resource, state, result, code, charged, created_at, updated_at, seconds, input_length, attempted_at, finished_at, exposure_addrs)
    SELECT operation_id, grant_id, request_key, payload_hash, tool, class, binding_id, generation, caller_station, caller_session, target_station, action_hash, resource, state, result, code, charged, created_at, updated_at, seconds, input_length, attempted_at, finished_at, exposure_addrs
    FROM coordinator_desktop_ops;
DROP TABLE coordinator_desktop_ops;
ALTER TABLE coordinator_desktop_ops_new RENAME TO coordinator_desktop_ops;
CREATE INDEX coordinator_desktop_ops_state ON coordinator_desktop_ops (state);
";
