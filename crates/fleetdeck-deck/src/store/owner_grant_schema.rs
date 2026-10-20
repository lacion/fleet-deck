//! The V46 migration (MCP P4b §6 and §7.1): `session_owner_grant`, the one
//! desired owner grant of a session with its per-session sequence (the
//! routes of P4b-3b use it), and `required_classes`, the full class set that
//! admitted a fleet command, in the outbox, the land queue and the moves.
//! Items admitted before V46 read `NULL` and are checked as before. Forward
//! only. To reverse it, drop the table and the three columns.

pub(crate) const SQL: &str = "
CREATE TABLE session_owner_grant (
    station_id  TEXT    NOT NULL,
    session_id  TEXT    NOT NULL,
    grant_id    TEXT    NOT NULL,
    seq         INTEGER NOT NULL,
    station_seq INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (station_id, session_id)
) STRICT;
ALTER TABLE outbox ADD COLUMN required_classes TEXT;
ALTER TABLE land_queue ADD COLUMN required_classes TEXT;
ALTER TABLE session_moves ADD COLUMN required_classes TEXT;
";
