//! The V52 migration (B27, `docs/v2/design/security-options.md` §3.3): the
//! `tokens` checks that hold the grant limits are wider, so that a grant can
//! have a spawn cap or an answer cap up to `4294967295` ("no limit") and an
//! idle time up to 7 days.
//!
//! SQLite cannot change a CHECK, so `tokens` is rebuilt in the pattern of V38
//! (`coordinator_input_any_schema.rs`): the foreign key check is deferred to
//! the end of the migration transaction (`defer_foreign_keys`), the rows go
//! through a copy table and come back by an insert into a table that is named
//! `tokens`. The new table is the V38 table plus the V43 column `kind` as the
//! last column, with three checks changed (`spawn_cap`, `idle_ttl_ms`,
//! `answer_cap`). Every other column, default, check, row and index stays.
//! The number V52 is reserved by the design; it follows V51 (B26, the grant
//! holds).
//!
//! Forward only. To reverse it, revoke and delete every grant above the old
//! checks (a spawn cap or an answer cap over 50 or 5000, an idle time over
//! 24 h), then rebuild `tokens` with the V38 checks and the column `kind`.

pub(crate) const SQL: &str = "
PRAGMA defer_foreign_keys = ON;
CREATE TABLE tokens_v51_copy AS SELECT * FROM tokens;
DROP TABLE tokens;
CREATE TABLE tokens (
    grant_id                TEXT    PRIMARY KEY,
    station_id              TEXT    NOT NULL,
    session_id              TEXT    NOT NULL,
    classes                 TEXT    NOT NULL,
    stations                TEXT    NOT NULL,
    spawn_cap               INTEGER NOT NULL CHECK (spawn_cap BETWEEN 0 AND 4294967295),
    spawn_used              INTEGER NOT NULL DEFAULT 0 CHECK (spawn_used >= 0),
    max_modes               TEXT    NOT NULL,
    idle_ttl_ms             INTEGER NOT NULL CHECK (idle_ttl_ms BETWEEN 1 AND 604800000),
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
    answer_cap              INTEGER CHECK (answer_cap IS NULL OR answer_cap BETWEEN 0 AND 4294967295),
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
    allow_click             INTEGER NOT NULL DEFAULT 0 CHECK (allow_click IN (0, 1)),
    kind                    TEXT    NOT NULL DEFAULT 'coordinator' CHECK (kind IN ('coordinator', 'worker'))
) STRICT;
INSERT INTO tokens (grant_id, station_id, session_id, classes, stations, spawn_cap, spawn_used, max_modes, idle_ttl_ms, created_at, expires_at, state, revoked_reason, bound_generation, bound_native_id, bound_provider_instance, binding_id, last_used_at, renew_generation, start_command_id, scope, answer_cap, answer_used, desktop_cap, desktop_used, capture_cap, capture_used, image_byte_cap, image_byte_used, desktop_attempts, recording_cap, recording_used, recording_second_cap, recording_second_used, allow_input, input_scope, input_cap, input_used, input_character_cap, input_character_used, allow_click, kind)
    SELECT grant_id, station_id, session_id, classes, stations, spawn_cap, spawn_used, max_modes, idle_ttl_ms, created_at, expires_at, state, revoked_reason, bound_generation, bound_native_id, bound_provider_instance, binding_id, last_used_at, renew_generation, start_command_id, scope, answer_cap, answer_used, desktop_cap, desktop_used, capture_cap, capture_used, image_byte_cap, image_byte_used, desktop_attempts, recording_cap, recording_used, recording_second_cap, recording_second_used, allow_input, input_scope, input_cap, input_used, input_character_cap, input_character_used, allow_click, kind
    FROM tokens_v51_copy;
DROP TABLE tokens_v51_copy;
CREATE INDEX tokens_by_caller ON tokens (station_id, session_id, created_at);
CREATE UNIQUE INDEX tokens_by_start_command ON tokens (start_command_id)
    WHERE start_command_id IS NOT NULL;
";
