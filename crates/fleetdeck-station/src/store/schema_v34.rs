//! The V34 migration (R13 P5a, task D2): the durable record of each owner
//! screenshot and the provenance column of an artifact version. A
//! `capture_ops` row is kept for ever: it answers a repeated request and
//! `desktop.capture_status` without a second screenshot. Forward only; no
//! existing row is rewritten.

pub(super) const V34: &str = "
CREATE TABLE capture_ops (               -- R13 P5a: one row per owner screenshot, kept forever
    deck_fp        TEXT    NOT NULL,     -- the authenticated deck fingerprint (64 hex)
    capture_id     TEXT    NOT NULL,
    action_hash    TEXT    NOT NULL,
    state          TEXT    NOT NULL,     -- running, committed, failed
    monitor        TEXT    NOT NULL,
    artifact_id    TEXT    NOT NULL,     -- allocated before the file move (§6.2)
    sha256         TEXT,
    code           TEXT,
    message        TEXT,
    reserved_bytes INTEGER NOT NULL,
    started_at     INTEGER NOT NULL,
    updated_at     INTEGER NOT NULL,
    PRIMARY KEY (deck_fp, capture_id)
) STRICT;
ALTER TABLE artifact_versions ADD COLUMN capture TEXT;   -- CaptureProvenance JSON
";
