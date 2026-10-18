//! The V36 migration (R13 P5b-2, task Q-F): the recording caps and counters
//! of a grant, and the rebuild of `coordinator_desktop_ops` so that its
//! `tool` check accepts `record_monitor` and the table has the nullable
//! `seconds` column. Forward only. Every existing grant reads four zeros (no
//! recording allowance, decision O12-6), and every existing operation row
//! keeps its values with `seconds` `NULL`. To reverse it, rebuild the table
//! without `seconds` and the old `tool` list (after the rows of
//! `record_monitor` are gone), and drop the four `tokens` columns.

pub(crate) const SQL: &str = "
ALTER TABLE tokens ADD COLUMN recording_cap INTEGER NOT NULL DEFAULT 0 CHECK (recording_cap BETWEEN 0 AND 5000);
ALTER TABLE tokens ADD COLUMN recording_used INTEGER NOT NULL DEFAULT 0 CHECK (recording_used >= 0);
ALTER TABLE tokens ADD COLUMN recording_second_cap INTEGER NOT NULL DEFAULT 0 CHECK (recording_second_cap BETWEEN 0 AND 600000);
ALTER TABLE tokens ADD COLUMN recording_second_used INTEGER NOT NULL DEFAULT 0 CHECK (recording_second_used >= 0);
CREATE TABLE coordinator_desktop_ops_new (
    operation_id TEXT PRIMARY KEY,
    grant_id TEXT NOT NULL,
    request_key TEXT NOT NULL,
    payload_hash TEXT NOT NULL,
    tool TEXT NOT NULL CHECK (tool IN ('wall_show','notify','preview_open','window_close','screenshot','record_monitor')),
    class TEXT NOT NULL CHECK (class IN ('desktop','capture')),
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
    UNIQUE (grant_id, request_key)
) STRICT;
INSERT INTO coordinator_desktop_ops_new (operation_id, grant_id, request_key, payload_hash, tool, class, binding_id, generation, caller_station, caller_session, target_station, action_hash, resource, state, result, code, charged, created_at, updated_at, seconds)
    SELECT operation_id, grant_id, request_key, payload_hash, tool, class, binding_id, generation, caller_station, caller_session, target_station, action_hash, resource, state, result, code, charged, created_at, updated_at, NULL
    FROM coordinator_desktop_ops;
DROP TABLE coordinator_desktop_ops;
ALTER TABLE coordinator_desktop_ops_new RENAME TO coordinator_desktop_ops;
CREATE INDEX coordinator_desktop_ops_state ON coordinator_desktop_ops (state);
";
