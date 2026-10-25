//! The V34 migration (R13 P6, task F1): the grant caps and counters, the
//! coordinator desktop operation ledger, the rejected request keys, the
//! rolling rate windows, and the coordinator provenance of a capture and of
//! a wall directive. Forward only: no existing row is rewritten, and every
//! new column of an existing table is `NULL` or `0`. To reverse it, drop the
//! three tables and the added columns.

pub(crate) const SQL: &str = "
ALTER TABLE tokens ADD COLUMN desktop_cap INTEGER CHECK (desktop_cap IS NULL OR desktop_cap BETWEEN 0 AND 5000);
ALTER TABLE tokens ADD COLUMN desktop_used INTEGER NOT NULL DEFAULT 0 CHECK (desktop_used >= 0);
ALTER TABLE tokens ADD COLUMN capture_cap INTEGER CHECK (capture_cap IS NULL OR capture_cap BETWEEN 0 AND 5000);
ALTER TABLE tokens ADD COLUMN capture_used INTEGER NOT NULL DEFAULT 0 CHECK (capture_used >= 0);
ALTER TABLE tokens ADD COLUMN image_byte_cap INTEGER CHECK (image_byte_cap IS NULL OR (image_byte_cap BETWEEN 0 AND 1073741824 AND image_byte_cap % 1048576 = 0));
ALTER TABLE tokens ADD COLUMN image_byte_used INTEGER NOT NULL DEFAULT 0 CHECK (image_byte_used >= 0);
ALTER TABLE tokens ADD COLUMN desktop_attempts INTEGER NOT NULL DEFAULT 0 CHECK (desktop_attempts >= 0);
CREATE TABLE coordinator_desktop_ops (
    operation_id TEXT PRIMARY KEY,
    grant_id TEXT NOT NULL,
    request_key TEXT NOT NULL,
    payload_hash TEXT NOT NULL,
    tool TEXT NOT NULL CHECK (tool IN ('wall_show','notify','preview_open','window_close','screenshot')),
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
    UNIQUE (grant_id, request_key)
) STRICT;
CREATE INDEX coordinator_desktop_ops_state ON coordinator_desktop_ops (state);
CREATE TABLE coordinator_desktop_rejects (
    grant_id TEXT NOT NULL,
    request_key TEXT NOT NULL,
    payload_hash TEXT NOT NULL,
    code TEXT NOT NULL,
    at INTEGER NOT NULL,
    PRIMARY KEY (grant_id, request_key)
) STRICT;
CREATE TABLE coordinator_desktop_rate (
    scope_key TEXT NOT NULL,
    bucket TEXT NOT NULL CHECK (bucket IN ('mutation','screenshot','query','image','notify')),
    at INTEGER NOT NULL
) STRICT;
CREATE INDEX coordinator_desktop_rate_key ON coordinator_desktop_rate (scope_key, bucket, at);
ALTER TABLE desktop_captures ADD COLUMN coordinator TEXT;
ALTER TABLE display_directives ADD COLUMN sender_generation INTEGER;
ALTER TABLE display_directives ADD COLUMN sender_station TEXT;
ALTER TABLE display_directives ADD COLUMN sender_session TEXT;
";
