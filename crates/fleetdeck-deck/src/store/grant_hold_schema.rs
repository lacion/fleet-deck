//! V51 (B26): the last `station.info` incarnation and version of each
//! station, and the grants that the deck holds while their station is
//! offline. To reverse it, drop the table and the two columns.

pub(crate) const SQL: &str = "
ALTER TABLE stations ADD COLUMN info_incarnation TEXT;
ALTER TABLE stations ADD COLUMN info_version TEXT;
CREATE TABLE grant_holds (
    grant_id   TEXT    PRIMARY KEY,
    station_id TEXT    NOT NULL REFERENCES stations(station_id) ON DELETE CASCADE,
    since      INTEGER NOT NULL
) STRICT;
CREATE INDEX grant_holds_by_station ON grant_holds (station_id);
";
