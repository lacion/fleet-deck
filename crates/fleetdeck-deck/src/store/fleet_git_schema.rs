//! The deck migrations of Fleet Git (design §8.2). V25 (phase 1): the Git
//! settings and the remote identity of a project, the per-station snapshot
//! state, and the reserved Git names. V26 (phase 2): the homes of a project,
//! their generations and their clone attempts. V27 (phase 3): the radar and
//! the land queue. V28 (PRE-PR round 1): the last known Taildrive machine name
//! of each station. A migration only adds
//! (decision 79). To reverse one, restore the pre-upgrade backup (§3.9);
//! never drop the tables while the deck runs.

/// V25 (Fleet Git phase 1, FG-5 identity columns, FG-1 to FG-4 settings).
pub const FLEET_GIT_SQL: &str = "
ALTER TABLE projects ADD COLUMN git          TEXT    NOT NULL DEFAULT '{}';
ALTER TABLE projects ADD COLUMN git_revision INTEGER NOT NULL DEFAULT 0;
ALTER TABLE projects ADD COLUMN repo         TEXT;
ALTER TABLE projects ADD COLUMN remote_url   TEXT;
ALTER TABLE projects ADD COLUMN remote_ssh_user TEXT;
ALTER TABLE projects ADD COLUMN remote_dest  TEXT;
CREATE TABLE git_snapshots (
    station_id     TEXT PRIMARY KEY,
    revision       INTEGER NOT NULL,
    digest         TEXT    NOT NULL,
    sent_at        INTEGER,
    acked_revision INTEGER,
    checks         TEXT    NOT NULL DEFAULT '[]'
) STRICT;
CREATE TABLE slug_reservations (
    repo          TEXT    NOT NULL,
    slug          TEXT    NOT NULL,
    station_id    TEXT    NOT NULL,
    first_used_at INTEGER NOT NULL,
    PRIMARY KEY (repo, slug)
) STRICT;
";

/// V26 (Fleet Git phase 2, FG-5): the homes of a project. Each existing
/// project gets one home at position 0 with generation 1 (O16). With sync on,
/// only the oldest project of a station and folder owns it; the others are
/// `unverified` with `home_owned` (the migration never fails on duplicates).
pub const HOMES_SQL: &str = "
CREATE TABLE project_homes (
    project_id     TEXT    NOT NULL REFERENCES projects(project_id) ON DELETE CASCADE,
    station_id     TEXT    NOT NULL,
    generation     INTEGER NOT NULL,
    position       INTEGER NOT NULL CHECK (position >= 0),
    state          TEXT    NOT NULL,
    path           TEXT,
    canonical_path TEXT,
    reserved_path  TEXT,
    attempt_id     TEXT,
    sync_owner     INTEGER NOT NULL DEFAULT 0 CHECK (sync_owner IN (0, 1)),
    code           TEXT,
    message        TEXT,
    created_at     INTEGER NOT NULL,
    updated_at     INTEGER NOT NULL,
    PRIMARY KEY (project_id, station_id),
    UNIQUE (project_id, position),
    CHECK (state NOT IN ('ready', 'unverified') OR path IS NOT NULL)
) STRICT;
CREATE INDEX project_homes_station ON project_homes (station_id);
CREATE UNIQUE INDEX project_homes_sync_owner
    ON project_homes (station_id, canonical_path) WHERE sync_owner = 1;
CREATE TABLE home_generations (
    project_id      TEXT    NOT NULL,
    station_id      TEXT    NOT NULL,
    last_generation INTEGER NOT NULL,
    PRIMARY KEY (project_id, station_id)
) STRICT;
CREATE TABLE home_attempts (
    attempt_id         TEXT PRIMARY KEY,
    request_command_id TEXT    NOT NULL UNIQUE,
    project_id         TEXT    NOT NULL,
    station_id         TEXT    NOT NULL,
    generation         INTEGER NOT NULL,
    request_digest     TEXT    NOT NULL,
    request            TEXT    NOT NULL,
    retry_of           TEXT,
    state              TEXT    NOT NULL,
    path               TEXT,
    code               TEXT,
    message            TEXT,
    send_attempts      INTEGER NOT NULL DEFAULT 0,
    next_send_at       INTEGER,
    created_at         INTEGER NOT NULL,
    ended_at           INTEGER
) STRICT;
CREATE INDEX home_attempts_home ON home_attempts (project_id, station_id, generation);
CREATE INDEX home_attempts_unsent ON home_attempts (next_send_at) WHERE state = 'sent';
INSERT INTO home_generations (project_id, station_id, last_generation)
    SELECT project_id, station_id, 1 FROM projects;
INSERT INTO project_homes (project_id, station_id, generation, position, state, path, canonical_path,
                           sync_owner, created_at, updated_at)
    SELECT p.project_id, p.station_id, 1, 0, 'ready', p.path, p.path,
           CASE WHEN json_extract(p.git, '$.sync') = 1
                 AND ROW_NUMBER() OVER (PARTITION BY p.station_id, p.path,
                                        json_extract(p.git, '$.sync') = 1
                                        ORDER BY p.created_at, p.project_id) = 1
                THEN 1 ELSE 0 END,
           p.created_at, p.created_at
    FROM projects p;
UPDATE project_homes SET state = 'unverified', code = 'home_owned',
        message = 'This folder is already the home of project ' || (
            SELECT p2.name FROM project_homes h2
            JOIN projects p2 ON p2.project_id = h2.project_id
            WHERE h2.station_id = project_homes.station_id
              AND h2.canonical_path = project_homes.canonical_path AND h2.sync_owner = 1
            LIMIT 1) || ' with sync on.'
    WHERE sync_owner = 0
      AND project_id IN (SELECT project_id FROM projects WHERE json_extract(git, '$.sync') = 1);
CREATE TABLE fleet_git_prefs (key TEXT PRIMARY KEY, value TEXT NOT NULL) STRICT;
";

/// V27 (Fleet Git phase 3, FG-7 and FG-8): the radar pairs, the land queue
/// and the land outbox (design §10.3, §11.2). The three partial unique
/// indexes make a second `running` land of one project or one lock key
/// impossible.
pub const LAND_SQL: &str = "
CREATE TABLE radar_pairs (
    project_id TEXT NOT NULL,
    a_station  TEXT NOT NULL, a_session TEXT NOT NULL,
    b_station  TEXT NOT NULL, b_session TEXT NOT NULL,
    conflict   INTEGER NOT NULL,
    files      TEXT NOT NULL,
    checked_at INTEGER NOT NULL,
    PRIMARY KEY (project_id, a_station, a_session, b_station, b_session)
) STRICT;
CREATE TABLE land_queue (
    land_id              TEXT    PRIMARY KEY,
    command_id           TEXT    NOT NULL UNIQUE,
    requested_by         TEXT    NOT NULL,
    request_command_id   TEXT    NOT NULL,
    authority            TEXT,
    authority_expires_at INTEGER,
    project_id           TEXT    NOT NULL,
    lock_key             TEXT    NOT NULL,
    station_id           TEXT    NOT NULL,
    session_id           TEXT    NOT NULL,
    plan                 TEXT,
    settings_revision    INTEGER,
    state                TEXT    NOT NULL,
    phase                TEXT,
    reconciling          INTEGER NOT NULL DEFAULT 0,
    abandon_state        TEXT,
    abandon_attempts     INTEGER NOT NULL DEFAULT 0,
    abandon_next_at      INTEGER,
    abandon_write        TEXT,
    attempt              INTEGER NOT NULL DEFAULT 0,
    source_oid           TEXT, base_oid TEXT, rebased_oid TEXT,
    branch_published_oid TEXT, default_published_oid TEXT,
    pr_url               TEXT,
    files                TEXT    NOT NULL DEFAULT '[]',
    code                 TEXT, message TEXT,
    created_at           INTEGER NOT NULL, started_at INTEGER, ended_at INTEGER,
    UNIQUE (requested_by, request_command_id)
) STRICT;
CREATE INDEX land_queue_project ON land_queue (project_id, state, created_at);
CREATE UNIQUE INDEX land_queue_open_session    ON land_queue (station_id, session_id) WHERE state IN ('queued', 'running');
CREATE UNIQUE INDEX land_queue_running_project ON land_queue (project_id) WHERE state = 'running';
CREATE UNIQUE INDEX land_queue_running_lock    ON land_queue (lock_key)   WHERE state = 'running';
CREATE TABLE land_outbox (
    command_id TEXT    PRIMARY KEY REFERENCES land_queue(command_id),
    land_id    TEXT    NOT NULL,
    station_id TEXT    NOT NULL,
    payload    TEXT    NOT NULL,
    state      TEXT    NOT NULL,
    attempts   INTEGER NOT NULL DEFAULT 0,
    next_at    INTEGER NOT NULL,
    receipt    TEXT,
    updated_at INTEGER NOT NULL
) STRICT;
";

/// V28 (PRE-PR round 1, H16): the last known Taildrive machine name of each
/// station, so that a deck restart or a failed report does not make a
/// paired station look like "not a station" (design §15.5). It starts empty
/// and fills at the next `station.info` of each station.
pub const STATION_MACHINES_SQL: &str = "
CREATE TABLE station_machines (
    station_id TEXT    PRIMARY KEY REFERENCES stations(station_id) ON DELETE CASCADE,
    machine    TEXT    NOT NULL,
    updated_at INTEGER NOT NULL
) STRICT;
";

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::super::schema::migrations;

    #[test]
    fn an_existing_project_reads_the_default_git_settings_after_the_migration() {
        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        let migrations = migrations();
        migrations.to_version(&mut conn, 24).unwrap();
        conn.execute(
            "INSERT INTO projects (project_id, name, station_id, path, forge, trackers, created_at)
             VALUES ('p1', 'App', 'st-1', '/work/app', NULL, '[]', 1)",
            [],
        )
        .unwrap();
        migrations.to_latest(&mut conn).unwrap();
        let (git, revision, repo, url): (String, i64, Option<String>, Option<String>) = conn
            .query_row(
                "SELECT git, git_revision, repo, remote_url FROM projects WHERE project_id = 'p1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!((git.as_str(), revision, repo, url), ("{}", 0, None, None));
        // The two new tables exist and start empty.
        for table in ["git_snapshots", "slug_reservations"] {
            let n: i64 = conn
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
                .unwrap();
            assert_eq!(n, 0, "{table}");
        }
    }
}
