//! The secret files of a project (Fleet Sync FS-3, migration V47, design
//! `docs/v2/design/fleet-sync.md` §3.2, revision 2 R5, R6, R10).
//!
//! `secret_files` holds the list of a project with the salt of each file and
//! the display facts of the last run. `secret_file_homes` holds, for each
//! home station, the digest that the relay last saw or wrote there, and
//! `secret_files.accepted_digest` holds the digest of the winner of the last
//! run that had one (pre-PR finding 3). The tables never hold a content. A
//! digest is a keyed hash and is never logged.
//!
//! Forward only. To reverse the migration (V47), drop `secret_file_homes`
//! and then `secret_files`; the rows are derived state that the next run
//! rebuilds (the next run is a first sync for each file).

use fleetdeck_proto::fleet_git_secrets::{SecretFileReport, SecretOutcome};
use rusqlite::{OptionalExtension, Row, params};

use super::{DeckStore, Result, call};

/// The migration (V47).
pub(super) const SQL: &str = "
CREATE TABLE secret_files (
    project_id     TEXT    NOT NULL REFERENCES projects (project_id) ON DELETE CASCADE,
    path           TEXT    NOT NULL,
    position       INTEGER NOT NULL,
    salt           BLOB    NOT NULL,          -- 32 bytes (R6)
    last_digest    TEXT,                      -- the digest of the last complete sync (display)
    last_source    TEXT,                      -- the station id of the last winner (display)
    last_synced_at INTEGER,
    last_report    TEXT    NOT NULL DEFAULT '{}', -- SecretFileReport JSON of the last run (no content)
    accepted_digest TEXT,                     -- the digest of the winner of the last run that had one
    PRIMARY KEY (project_id, path)
) STRICT;
CREATE TABLE secret_file_homes (              -- the digest that the relay last saw or wrote on a station
    project_id  TEXT NOT NULL,
    path        TEXT NOT NULL,
    station_id  TEXT NOT NULL,
    seen_digest TEXT NOT NULL,
    PRIMARY KEY (project_id, path, station_id),
    FOREIGN KEY (project_id, path) REFERENCES secret_files (project_id, path) ON DELETE CASCADE
) STRICT;
";

/// One listed secret file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretFileRow {
    pub path: String,
    /// 32 bytes.
    pub salt: Vec<u8>,
    pub last_digest: Option<String>,
    pub last_source: Option<String>,
    pub last_synced_at: Option<i64>,
    pub last_report: SecretFileReport,
    /// The digest of the winner of the last run that had a winner. When no
    /// copy changed, a copy with this digest wins again.
    pub accepted: Option<String>,
    /// `(station id, digest)` that the relay last saw or wrote there.
    pub seen: Vec<(String, String)>,
}

/// The report of a file that has not run yet.
fn not_run(path: &str) -> SecretFileReport {
    SecretFileReport {
        path: path.to_owned(),
        synced_at: None,
        source: None,
        conflict: false,
        outcome: SecretOutcome::NotRun,
        stations: Vec::new(),
    }
}

const COLUMNS: &str =
    "path, salt, last_digest, last_source, last_synced_at, last_report, accepted_digest";

struct Raw {
    path: String,
    salt: Vec<u8>,
    last_digest: Option<String>,
    last_source: Option<String>,
    last_synced_at: Option<i64>,
    last_report: String,
    accepted: Option<String>,
}

fn raw(r: &Row<'_>) -> rusqlite::Result<Raw> {
    Ok(Raw {
        path: r.get(0)?,
        salt: r.get(1)?,
        last_digest: r.get(2)?,
        last_source: r.get(3)?,
        last_synced_at: r.get(4)?,
        last_report: r.get(5)?,
        accepted: r.get(6)?,
    })
}

impl Raw {
    fn parse(self, seen: Vec<(String, String)>) -> Result<SecretFileRow> {
        let last_report = if self.last_report == "{}" {
            not_run(&self.path)
        } else {
            serde_json::from_str(&self.last_report)?
        };
        Ok(SecretFileRow {
            path: self.path,
            salt: self.salt,
            last_digest: self.last_digest,
            last_source: self.last_source,
            last_synced_at: self.last_synced_at,
            last_report,
            accepted: self.accepted,
            seen,
        })
    }
}

fn seen_of(
    conn: &rusqlite::Connection,
    project: &str,
    path: &str,
) -> Result<Vec<(String, String)>> {
    let mut stmt = conn.prepare(
        "SELECT station_id, seen_digest FROM secret_file_homes
         WHERE project_id = ?1 AND path = ?2 ORDER BY station_id",
    )?;
    let rows = stmt.query_map(params![project, path], |r| Ok((r.get(0)?, r.get(1)?)))?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

impl DeckStore {
    /// The secret files of `project`, in list order.
    ///
    /// # Errors
    /// A database or stored-JSON error.
    pub async fn secret_files(&self, project: String) -> Result<Vec<SecretFileRow>> {
        call(&self.inner.db, move |conn| {
            let raws = {
                let mut stmt = conn.prepare(&format!(
                    "SELECT {COLUMNS} FROM secret_files WHERE project_id = ?1 ORDER BY position"
                ))?;
                let rows = stmt.query_map([&project], raw)?;
                rows.collect::<rusqlite::Result<Vec<_>>>()?
            };
            raws.into_iter()
                .map(|r| {
                    let seen = seen_of(conn, &project, &r.path)?;
                    r.parse(seen)
                })
                .collect()
        })
        .await
    }

    /// One secret file of `project`.
    ///
    /// # Errors
    /// A database or stored-JSON error.
    pub async fn secret_file(
        &self,
        project: String,
        path: String,
    ) -> Result<Option<SecretFileRow>> {
        call(&self.inner.db, move |conn| {
            let found = conn
                .query_row(
                    &format!(
                        "SELECT {COLUMNS} FROM secret_files WHERE project_id = ?1 AND path = ?2"
                    ),
                    params![project, path],
                    raw,
                )
                .optional()?;
            found
                .map(|r| {
                    let seen = seen_of(conn, &project, &r.path)?;
                    r.parse(seen)
                })
                .transpose()
        })
        .await
    }

    /// Sets the list of `project` to `paths` in one transaction: a removed
    /// path loses its row (and its seen digests), a new path gets a new
    /// 32-byte salt, a kept path keeps its salt and its facts. `None`: the
    /// project does not exist.
    ///
    /// # Errors
    /// A database error, or `Corrupt` when the random source fails.
    pub async fn secret_files_set(
        &self,
        project: String,
        paths: Vec<String>,
    ) -> Result<Option<()>> {
        let mut salts = Vec::with_capacity(paths.len());
        for _ in &paths {
            salts.push(new_salt()?);
        }
        call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            let known: Option<i64> = tx
                .query_row(
                    "SELECT 1 FROM projects WHERE project_id = ?1",
                    [&project],
                    |r| r.get(0),
                )
                .optional()?;
            if known.is_none() {
                return Ok(None);
            }
            let existing: Vec<String> = {
                let mut stmt = tx.prepare("SELECT path FROM secret_files WHERE project_id = ?1")?;
                let rows = stmt.query_map([&project], |r| r.get(0))?;
                rows.collect::<rusqlite::Result<Vec<_>>>()?
            };
            for path in existing.iter().filter(|p| !paths.contains(p)) {
                tx.execute(
                    "DELETE FROM secret_files WHERE project_id = ?1 AND path = ?2",
                    params![project, path],
                )?;
            }
            for (position, (path, salt)) in paths.iter().zip(&salts).enumerate() {
                let position = i64::try_from(position)
                    .map_err(|_| super::DeckError::Corrupt("secret file position".into()))?;
                tx.execute(
                    "INSERT INTO secret_files (project_id, path, position, salt)
                     VALUES (?1, ?2, ?3, ?4)
                     ON CONFLICT (project_id, path) DO UPDATE SET position = excluded.position",
                    params![project, path, position, salt.as_slice()],
                )?;
            }
            tx.commit()?;
            Ok(Some(()))
        })
        .await
    }

    /// Stores the report of a run of one file. With `baseline`
    /// (`(digest, source)`) it also stores the display facts of a complete
    /// run. It changes nothing and answers `false` when the row is gone or
    /// its salt is not `salt` (R6).
    ///
    /// # Errors
    /// A database or JSON error.
    pub async fn secret_file_done(
        &self,
        project: String,
        path: String,
        salt: Vec<u8>,
        baseline: Option<(String, String)>,
        report: SecretFileReport,
    ) -> Result<bool> {
        let now = self.now_ms();
        let text = serde_json::to_string(&report)?;
        call(&self.inner.db, move |conn| {
            let changed = match baseline {
                Some((digest, source)) => conn.execute(
                    "UPDATE secret_files SET last_report = ?4, last_digest = ?5,
                            last_source = ?6, last_synced_at = ?7
                     WHERE project_id = ?1 AND path = ?2 AND salt = ?3",
                    params![project, path, salt, text, digest, source, now],
                )?,
                None => conn.execute(
                    "UPDATE secret_files SET last_report = ?4
                     WHERE project_id = ?1 AND path = ?2 AND salt = ?3",
                    params![project, path, salt, text],
                )?,
            };
            Ok(changed == 1)
        })
        .await
    }

    /// Sets the accepted digest of a file (`None`: it stays as it is) and the
    /// seen digest of each `(station id, digest)`, in one transaction. It
    /// changes nothing and answers `false` when the row is gone or its salt
    /// is not `salt` (R6).
    ///
    /// # Errors
    /// A database error.
    pub async fn secret_seen_set(
        &self,
        project: String,
        path: String,
        salt: Vec<u8>,
        accepted: Option<String>,
        seen: Vec<(String, String)>,
    ) -> Result<bool> {
        call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            let same: Option<i64> = tx
                .query_row(
                    "SELECT 1 FROM secret_files WHERE project_id = ?1 AND path = ?2 AND salt = ?3",
                    params![project, path, salt],
                    |r| r.get(0),
                )
                .optional()?;
            if same.is_none() {
                return Ok(false);
            }
            if let Some(digest) = &accepted {
                tx.execute(
                    "UPDATE secret_files SET accepted_digest = ?4
                     WHERE project_id = ?1 AND path = ?2 AND salt = ?3",
                    params![project, path, salt, digest],
                )?;
            }
            for (station, digest) in &seen {
                tx.execute(
                    "INSERT INTO secret_file_homes (project_id, path, station_id, seen_digest)
                     VALUES (?1, ?2, ?3, ?4)
                     ON CONFLICT (project_id, path, station_id)
                     DO UPDATE SET seen_digest = excluded.seen_digest",
                    params![project, path, station, digest],
                )?;
            }
            tx.commit()?;
            Ok(true)
        })
        .await
    }

    /// The projects with at least one secret file and at least two homes
    /// that have a path.
    ///
    /// # Errors
    /// A database error.
    pub async fn secret_projects(&self) -> Result<Vec<String>> {
        call(&self.inner.db, |conn| {
            let mut stmt = conn.prepare(
                "SELECT DISTINCT f.project_id FROM secret_files f
                 WHERE (SELECT COUNT(*) FROM project_homes h
                        WHERE h.project_id = f.project_id AND h.path IS NOT NULL) >= 2
                 ORDER BY f.project_id",
            )?;
            let rows = stmt.query_map([], |r| r.get(0))?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })
        .await
    }
}

/// A new salt: 32 bytes from the system random source.
fn new_salt() -> Result<[u8; 32]> {
    use ring::rand::SecureRandom;
    let mut salt = [0u8; 32];
    ring::rand::SystemRandom::new()
        .fill(&mut salt)
        .map_err(|_| super::DeckError::Corrupt("the random source failed".into()))?;
    Ok(salt)
}
