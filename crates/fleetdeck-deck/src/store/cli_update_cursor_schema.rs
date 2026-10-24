//! V53 (B28): the audit cursor of provider updates and the attempts that the
//! deck has imported (design `cli-update.md` §3.11).
//!
//! The migration only creates tables. To reverse it, drop both tables.

/// The migration.
pub(super) const SQL: &str = "
CREATE TABLE cli_update_cursor (
    station_id  TEXT    PRIMARY KEY,
    incarnation TEXT    NOT NULL,
    seq         INTEGER NOT NULL
);

-- The attempts that the deck has imported, so a station restored from a
-- snapshot does not show an attempt twice.
CREATE TABLE cli_update_seen (
    station_id  TEXT    NOT NULL,
    attempt_id  TEXT    NOT NULL,
    imported_at INTEGER NOT NULL,
    PRIMARY KEY (station_id, attempt_id)
);
";
