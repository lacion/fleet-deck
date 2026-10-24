//! The V38 migration (R13 P7b, task F; `docs/v2/design/omarchy-p7b.md` §3.1
//! and §3.3): the click allowance and the `any_window` scope of a grant, the
//! `pointer_click` tool of an operation, and the `window_list` rate bucket.
//!
//! SQLite cannot change a CHECK, so three tables are rebuilt. `tokens` gets
//! the scope check `own_windows` or `any_window` and the new last column
//! `allow_click` (default 0, so every existing grant has no click allowance);
//! every other column, value, constraint, index and the relationship of
//! `coordinator_lineage.grant_id` stay. Foreign keys are on during a
//! migration, so the rebuild defers their check to the end of the migration
//! transaction (`defer_foreign_keys`). The rows go through a copy table and
//! come back by an insert into a table that is named `tokens`: a rename does
//! not clear the deferred violations that the drop makes, an insert does. `coordinator_desktop_ops` and
//! `coordinator_desktop_rate` are rebuilt in the pattern of V37 with the new
//! tool and the new bucket; every row, value, unique constraint and index
//! stays. No session is deleted (V37 did that, once).
//!
//! Forward only. To reverse it, rebuild the three tables with the V37 checks
//! after the `any_window` grants, the `pointer_click` operations and the
//! `window_list` rate rows are gone, and drop the column `allow_click`.

pub(crate) const SQL: &str = "
PRAGMA defer_foreign_keys = ON;
CREATE TABLE tokens_v37_copy AS SELECT * FROM tokens;
DROP TABLE tokens;
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
    last_used_at            INTEGER,
    renew_generation        INTEGER,
    start_command_id        TEXT,
    scope                   TEXT    NOT NULL DEFAULT 'lineage' CHECK (scope IN ('lineage', 'fleet')),
    answer_cap              INTEGER CHECK (answer_cap IS NULL OR answer_cap BETWEEN 0 AND 5000),
    answer_used             INTEGER NOT NULL DEFAULT 0 CHECK (answer_used >= 0),
    desktop_cap             INTEGER CHECK (desktop_cap IS NULL OR desktop_cap BETWEEN 0 AND 5000),
    desktop_used            INTEGER NOT NULL DEFAULT 0 CHECK (desktop_used >= 0),
    capture_cap             INTEGER CHECK (capture_cap IS NULL OR capture_cap BETWEEN 0 AND 5000),
    capture_used            INTEGER NOT NULL DEFAULT 0 CHECK (capture_used >= 0),
    image_byte_cap          INTEGER CHECK (image_byte_cap IS NULL OR (image_byte_cap BETWEEN 0 AND 1073741824 AND image_byte_cap % 1048576 = 0)),
    image_byte_used         INTEGER NOT NULL DEFAULT 0 CHECK (image_byte_used >= 0),
    desktop_attempts        INTEGER NOT NULL DEFAULT 0 CHECK (desktop_attempts >= 0),
    recording_cap           INTEGER NOT NULL DEFAULT 0 CHECK (recording_cap BETWEEN 0 AND 5000),
    recording_used          INTEGER NOT NULL DEFAULT 0 CHECK (recording_used >= 0),
    recording_second_cap    INTEGER NOT NULL DEFAULT 0 CHECK (recording_second_cap BETWEEN 0 AND 600000),
    recording_second_used   INTEGER NOT NULL DEFAULT 0 CHECK (recording_second_used >= 0),
    allow_input             INTEGER NOT NULL DEFAULT 0 CHECK (allow_input IN (0, 1)),
    input_scope             TEXT    CHECK (input_scope IS NULL OR input_scope IN ('own_windows', 'any_window')),
    input_cap               INTEGER NOT NULL DEFAULT 0 CHECK (input_cap BETWEEN 0 AND 5000),
    input_used              INTEGER NOT NULL DEFAULT 0 CHECK (input_used >= 0),
    input_character_cap     INTEGER NOT NULL DEFAULT 0 CHECK (input_character_cap BETWEEN 0 AND 1000000),
    input_character_used    INTEGER NOT NULL DEFAULT 0 CHECK (input_character_used >= 0),
    allow_click             INTEGER NOT NULL DEFAULT 0 CHECK (allow_click IN (0, 1))
) STRICT;
INSERT INTO tokens (grant_id, station_id, session_id, classes, stations, spawn_cap, spawn_used, max_modes, idle_ttl_ms, created_at, expires_at, state, revoked_reason, bound_generation, bound_native_id, bound_provider_instance, binding_id, last_used_at, renew_generation, start_command_id, scope, answer_cap, answer_used, desktop_cap, desktop_used, capture_cap, capture_used, image_byte_cap, image_byte_used, desktop_attempts, recording_cap, recording_used, recording_second_cap, recording_second_used, allow_input, input_scope, input_cap, input_used, input_character_cap, input_character_used)
    SELECT grant_id, station_id, session_id, classes, stations, spawn_cap, spawn_used, max_modes, idle_ttl_ms, created_at, expires_at, state, revoked_reason, bound_generation, bound_native_id, bound_provider_instance, binding_id, last_used_at, renew_generation, start_command_id, scope, answer_cap, answer_used, desktop_cap, desktop_used, capture_cap, capture_used, image_byte_cap, image_byte_used, desktop_attempts, recording_cap, recording_used, recording_second_cap, recording_second_used, allow_input, input_scope, input_cap, input_used, input_character_cap, input_character_used
    FROM tokens_v37_copy;
DROP TABLE tokens_v37_copy;
CREATE INDEX tokens_by_caller ON tokens (station_id, session_id, created_at);
CREATE UNIQUE INDEX tokens_by_start_command ON tokens (start_command_id)
    WHERE start_command_id IS NOT NULL;
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
    seconds INTEGER CHECK (seconds IS NULL OR seconds BETWEEN 1 AND 120),
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
CREATE TABLE coordinator_desktop_rate_new (
    scope_key TEXT NOT NULL,
    bucket TEXT NOT NULL CHECK (bucket IN ('mutation','screenshot','query','image','notify','input','window_list')),
    at INTEGER NOT NULL
) STRICT;
INSERT INTO coordinator_desktop_rate_new (scope_key, bucket, at)
    SELECT scope_key, bucket, at FROM coordinator_desktop_rate;
DROP TABLE coordinator_desktop_rate;
ALTER TABLE coordinator_desktop_rate_new RENAME TO coordinator_desktop_rate;
CREATE INDEX coordinator_desktop_rate_key ON coordinator_desktop_rate (scope_key, bucket, at);
";
