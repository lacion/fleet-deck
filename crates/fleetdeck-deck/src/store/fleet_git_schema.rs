//! The deck migration of Fleet Git phase 1 (design §8.2): the Git settings
//! and the remote identity of a project, the per-station snapshot state, and
//! the reserved Git names. The migration only adds (decision 79). To reverse
//! it, restore the pre-upgrade backup (§3.9); never drop the tables while
//! the deck runs.

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
