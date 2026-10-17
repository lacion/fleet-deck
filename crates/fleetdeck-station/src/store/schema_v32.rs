//! The V32 migration (Fleet Git phase 4, design §12.3, T4.2c): the source
//! station of a moved-in session (the text of its `move_in` hold) and the
//! staging record of an upload, which exists before the `move_in_runs` row
//! does (the files come at step 3, the `MoveIn` command at step 4). Forward
//! only; no existing row is rewritten.

pub(super) const V32: &str = "
ALTER TABLE move_in_runs ADD COLUMN source_station TEXT;   -- MoveIn.from.station_id
CREATE TABLE move_staging (                  -- on the target: the upload of step 3
    move_id    TEXT PRIMARY KEY,
    path       TEXT    NOT NULL,             -- <station dir>/moves/<move_id>
    dev        INTEGER NOT NULL,
    ino        INTEGER NOT NULL,
    manifest   TEXT    NOT NULL,             -- TranscriptManifest JSON
    state      TEXT    NOT NULL,             -- uploading, ready, deleted
    created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL
) STRICT;
";
