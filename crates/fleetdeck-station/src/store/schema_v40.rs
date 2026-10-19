//! The V40 migration (knobs K4, `docs/v2/design/knobs.md` section 3,
//! "Recording length, storage"): the recording length of `capture_ops` can be
//! up to 3600 seconds. SQLite cannot change a CHECK, so the table is rebuilt
//! as in V37 and V38: every column, every row, every other constraint and the
//! index `capture_ops_artifact` stay as V34, V35 and V36 made them; only
//! `requested_seconds` changes from `BETWEEN 1 AND 120` to `BETWEEN 1 AND
//! 3600`. No other table refers to `capture_ops`. Forward only. To reverse it,
//! rebuild the table with the old check after the rows over 120 seconds are
//! gone.

pub(super) const V40: &str = "
CREATE TABLE capture_ops_v40 (
    deck_fp             TEXT    NOT NULL,
    capture_id          TEXT    NOT NULL,
    action_hash         TEXT    NOT NULL,
    state               TEXT    NOT NULL,
    monitor             TEXT    NOT NULL,
    artifact_id         TEXT    NOT NULL,
    sha256              TEXT,
    code                TEXT,
    message             TEXT,
    reserved_bytes      INTEGER NOT NULL,
    started_at          INTEGER NOT NULL,
    updated_at          INTEGER NOT NULL,
    coordinator         TEXT,
    operation_kind      TEXT    NOT NULL DEFAULT 'screenshot'
        CHECK (operation_kind IN ('screenshot','recording')),
    requested_seconds   INTEGER
        CHECK (requested_seconds IS NULL OR requested_seconds BETWEEN 1 AND 3600),
    link_epoch          TEXT,
    phase               TEXT
        CHECK (phase IS NULL OR phase IN ('preparing','recording','stopping','finalizing')),
    stop_intent         TEXT    NOT NULL DEFAULT 'none'
        CHECK (stop_intent IN ('none','save','discard')),
    stop_reason         TEXT,
    recorder_started_at INTEGER,
    ended_at            INTEGER,
    duration_ms         INTEGER,
    width               INTEGER,
    height              INTEGER,
    PRIMARY KEY (deck_fp, capture_id)
) STRICT;
INSERT INTO capture_ops_v40
    (deck_fp, capture_id, action_hash, state, monitor, artifact_id, sha256, code, message,
     reserved_bytes, started_at, updated_at, coordinator, operation_kind, requested_seconds,
     link_epoch, phase, stop_intent, stop_reason, recorder_started_at, ended_at, duration_ms,
     width, height)
SELECT deck_fp, capture_id, action_hash, state, monitor, artifact_id, sha256, code, message,
       reserved_bytes, started_at, updated_at, coordinator, operation_kind, requested_seconds,
       link_epoch, phase, stop_intent, stop_reason, recorder_started_at, ended_at, duration_ms,
       width, height
FROM capture_ops;
DROP TABLE capture_ops;
ALTER TABLE capture_ops_v40 RENAME TO capture_ops;
CREATE INDEX capture_ops_artifact ON capture_ops (artifact_id);
";
