//! The V36 migration (R13 P5b, task F): the recording columns of
//! `capture_ops` and the `capture_garbage` table. Forward only: no existing
//! row is rewritten, and every old row reads `operation_kind = 'screenshot'`.
//! To reverse it, drop the table, the index and the added columns.
//!
//! `reserved_bytes` changes meaning for a committed recording: it holds the
//! remaining staging copy and drops to 0 only after the durable staging
//! removal. The quota is `SUM(committed capture sizes) + SUM(reserved_bytes) +
//! SUM(capture_garbage.bytes)`.

pub(super) const V36: &str = "
ALTER TABLE capture_ops ADD COLUMN operation_kind TEXT NOT NULL DEFAULT 'screenshot'
  CHECK (operation_kind IN ('screenshot','recording'));
ALTER TABLE capture_ops ADD COLUMN requested_seconds INTEGER
  CHECK (requested_seconds IS NULL OR requested_seconds BETWEEN 1 AND 120);
ALTER TABLE capture_ops ADD COLUMN link_epoch TEXT;            -- local_desktop_epoch of the start link
ALTER TABLE capture_ops ADD COLUMN phase TEXT
  CHECK (phase IS NULL OR phase IN ('preparing','recording','stopping','finalizing'));
ALTER TABLE capture_ops ADD COLUMN stop_intent TEXT NOT NULL DEFAULT 'none'
  CHECK (stop_intent IN ('none','save','discard'));
ALTER TABLE capture_ops ADD COLUMN stop_reason TEXT;           -- snake_case, first discard wins
ALTER TABLE capture_ops ADD COLUMN recorder_started_at INTEGER;
ALTER TABLE capture_ops ADD COLUMN ended_at INTEGER;
ALTER TABLE capture_ops ADD COLUMN duration_ms INTEGER;
ALTER TABLE capture_ops ADD COLUMN width INTEGER;
ALTER TABLE capture_ops ADD COLUMN height INTEGER;
CREATE INDEX capture_ops_artifact ON capture_ops (artifact_id);
CREATE TABLE capture_garbage (         -- counted until its durable removal
    area  TEXT NOT NULL CHECK (area IN ('staging','artifacts')),
    name  TEXT NOT NULL,
    bytes INTEGER NOT NULL CHECK (bytes >= 0),
    found_at INTEGER NOT NULL,
    PRIMARY KEY (area, name)
) STRICT;
";
