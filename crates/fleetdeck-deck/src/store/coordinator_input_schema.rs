//! The V37 migration (R13 P7, task F; `docs/v2/design/omarchy-p7.md` §3.5 and
//! §7.6): the input allowance, scope, caps and counters of a grant; the
//! rebuild of `coordinator_desktop_ops` so that its `tool` check accepts
//! `key_press`, `type_text` and `pointer_move`, its `class` check accepts
//! `input`, and the table has the nullable columns `input_length`,
//! `attempted_at`, `finished_at` and `exposure_addrs`; the rebuild of
//! `coordinator_desktop_rate` so that its `bucket` check accepts `input`; the
//! last address of an owner login session and of a member session; and the
//! deletion of every existing owner login session and member session (§7.6:
//! no session without a recorded address survives, so every browser signs in
//! once after the upgrade).
//!
//! Forward only. Every existing grant reads input disabled (allowance 0,
//! scope `NULL`, caps and usage 0); every existing operation row keeps its
//! values, its recording `seconds` included, with the four new columns
//! `NULL`; every existing rate row stays. To reverse it, rebuild the two
//! tables without the new values and columns (after the rows of the three
//! tools and of the `input` class and bucket are gone), drop the six `tokens`
//! columns and the two pairs of session columns. The deleted sessions cannot be
//! restored: a browser signs in again.

pub(crate) const SQL: &str = "
ALTER TABLE tokens ADD COLUMN allow_input INTEGER NOT NULL DEFAULT 0 CHECK (allow_input IN (0, 1));
ALTER TABLE tokens ADD COLUMN input_scope TEXT CHECK (input_scope IS NULL OR input_scope = 'own_windows');
ALTER TABLE tokens ADD COLUMN input_cap INTEGER NOT NULL DEFAULT 0 CHECK (input_cap BETWEEN 0 AND 5000);
ALTER TABLE tokens ADD COLUMN input_used INTEGER NOT NULL DEFAULT 0 CHECK (input_used >= 0);
ALTER TABLE tokens ADD COLUMN input_character_cap INTEGER NOT NULL DEFAULT 0 CHECK (input_character_cap BETWEEN 0 AND 1000000);
ALTER TABLE tokens ADD COLUMN input_character_used INTEGER NOT NULL DEFAULT 0 CHECK (input_character_used >= 0);
CREATE TABLE coordinator_desktop_ops_new (
    operation_id TEXT PRIMARY KEY,
    grant_id TEXT NOT NULL,
    request_key TEXT NOT NULL,
    payload_hash TEXT NOT NULL,
    tool TEXT NOT NULL CHECK (tool IN ('wall_show','notify','preview_open','window_close','screenshot','record_monitor','key_press','type_text','pointer_move')),
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
    SELECT operation_id, grant_id, request_key, payload_hash, tool, class, binding_id, generation, caller_station, caller_session, target_station, action_hash, resource, state, result, code, charged, created_at, updated_at, seconds, NULL, NULL, NULL, NULL
    FROM coordinator_desktop_ops;
DROP TABLE coordinator_desktop_ops;
ALTER TABLE coordinator_desktop_ops_new RENAME TO coordinator_desktop_ops;
CREATE INDEX coordinator_desktop_ops_state ON coordinator_desktop_ops (state);
CREATE TABLE coordinator_desktop_rate_new (
    scope_key TEXT NOT NULL,
    bucket TEXT NOT NULL CHECK (bucket IN ('mutation','screenshot','query','image','notify','input')),
    at INTEGER NOT NULL
) STRICT;
INSERT INTO coordinator_desktop_rate_new (scope_key, bucket, at)
    SELECT scope_key, bucket, at FROM coordinator_desktop_rate;
DROP TABLE coordinator_desktop_rate;
ALTER TABLE coordinator_desktop_rate_new RENAME TO coordinator_desktop_rate;
CREATE INDEX coordinator_desktop_rate_key ON coordinator_desktop_rate (scope_key, bucket, at);
ALTER TABLE login_sessions ADD COLUMN last_addr TEXT;
ALTER TABLE login_sessions ADD COLUMN last_addr_seen_at INTEGER;
ALTER TABLE member_sessions ADD COLUMN last_addr TEXT;
ALTER TABLE member_sessions ADD COLUMN last_addr_seen_at INTEGER;
DELETE FROM login_sessions;
DELETE FROM member_sessions;
";
