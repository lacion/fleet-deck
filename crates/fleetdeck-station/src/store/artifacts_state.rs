//! The artifact columns of `session_git`, the stored artifact-share
//! authorization (V29 `artifact_auth`) and the reads that the exposure check
//! of a Taildrive share needs (design §15.4, §15.5, FG-12). Each [`Store`]
//! method runs one transaction.

use fleetdeck_proto::SessionId;
use fleetdeck_proto::fleet_git::ArtifactAuthorization;
use rusqlite::{Connection, OptionalExtension, Row, params};

use super::{Result, Store, StoreError, call, git_state};

fn to_i64(v: u64) -> i64 {
    i64::try_from(v).unwrap_or(i64::MAX)
}

fn to_u64(v: Option<i64>) -> Option<u64> {
    v.and_then(|v| u64::try_from(v).ok())
}

/// The artifact data of one `session_git` row.
#[derive(Debug, Clone, PartialEq)]
pub struct ArtifactsRow {
    pub session_id: SessionId,
    pub project_id: String,
    /// The session folder with the trailing `/` (the text of the variable).
    pub path: Option<String>,
    pub dev: Option<u64>,
    pub ino: Option<u64>,
    pub url: Option<String>,
    /// The newest authorization: display and audit, and the share that the
    /// session started with.
    pub auth: Option<ArtifactAuthorization>,
    /// The session has not ended (`ended_at` is NULL).
    pub live: bool,
}

/// What a start or a refresh records for a session.
#[derive(Debug, Clone, PartialEq)]
pub struct ArtifactsRecord {
    pub path: Option<String>,
    pub dev: Option<u64>,
    pub ino: Option<u64>,
    pub url: Option<String>,
    pub auth: ArtifactAuthorization,
}

/// The paths that make a share exposed, as the store has them (design
/// §15.4). The strings are not canonical yet.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExposureInputs {
    /// Every home: `station_homes`, `git_projects.canonical_path`, and the
    /// folder of a home attempt that is live or reserved.
    pub homes: Vec<String>,
    /// Every session `worktree` and every `session_git.repo_root`.
    pub worktrees: Vec<String>,
    /// A snapshot of a deck with `fleet_git.v1` was received.
    pub snapshot_seen: bool,
}

const COLUMNS: &str = "session_id, project_id, artifacts_path, artifacts_dev, artifacts_ino,
     artifacts_url, artifact_auth, ended_at";

fn row_of(r: &Row<'_>) -> rusqlite::Result<ArtifactsRow> {
    let auth: Option<String> = r.get(6)?;
    let session: String = r.get(0)?;
    Ok(ArtifactsRow {
        session_id: SessionId::from(session),
        project_id: r.get(1)?,
        path: r.get(2)?,
        dev: to_u64(r.get(3)?),
        ino: to_u64(r.get(4)?),
        url: r.get(5)?,
        auth: auth.and_then(|text| serde_json::from_str(&text).ok()),
        live: r.get::<_, Option<i64>>(7)?.is_none(),
    })
}

fn strings(conn: &Connection, sql: &str) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

fn json_of(auth: &ArtifactAuthorization) -> Result<String> {
    serde_json::to_string(auth)
        .map_err(|e| StoreError::Corrupt(format!("artifact authorization: {e}")))
}

impl Store {
    /// The artifact data of a session, `None` without a `session_git` row.
    pub(crate) async fn artifacts_row(
        &self,
        session_id: SessionId,
    ) -> Result<Option<ArtifactsRow>> {
        call(&self.inner.db, move |conn| {
            Ok(conn
                .query_row(
                    &format!("SELECT {COLUMNS} FROM session_git WHERE session_id = ?1"),
                    params![session_id.as_str()],
                    row_of,
                )
                .optional()?)
        })
        .await
    }

    /// The live sessions that have an artifact folder or address.
    pub(crate) async fn artifacts_live(&self) -> Result<Vec<ArtifactsRow>> {
        call(&self.inner.db, |conn| {
            let mut stmt = conn.prepare(&format!(
                "SELECT {COLUMNS} FROM session_git
                 WHERE ended_at IS NULL
                   AND (artifacts_path IS NOT NULL OR artifacts_url IS NOT NULL)
                 ORDER BY session_id"
            ))?;
            let rows = stmt.query_map([], row_of)?;
            Ok(rows.collect::<std::result::Result<_, _>>()?)
        })
        .await
    }

    /// Records the folder or the address of a session and its authorization.
    pub(crate) async fn artifacts_record(
        &self,
        session_id: SessionId,
        record: ArtifactsRecord,
    ) -> Result<()> {
        let auth = json_of(&record.auth)?;
        call(&self.inner.db, move |conn| {
            conn.execute(
                "UPDATE session_git SET artifacts_path = ?2, artifacts_dev = ?3,
                   artifacts_ino = ?4, artifacts_url = ?5, artifact_auth = ?6
                 WHERE session_id = ?1",
                params![
                    session_id.as_str(),
                    record.path,
                    record.dev.map(to_i64),
                    record.ino.map(to_i64),
                    record.url,
                    auth
                ],
            )?;
            Ok(())
        })
        .await
    }

    /// Stores the newest authorization of a session (display and audit).
    pub(crate) async fn artifacts_set_auth(
        &self,
        session_id: SessionId,
        auth: ArtifactAuthorization,
    ) -> Result<()> {
        let auth = json_of(&auth)?;
        call(&self.inner.db, move |conn| {
            conn.execute(
                "UPDATE session_git SET artifact_auth = ?2 WHERE session_id = ?1",
                params![session_id.as_str(), auth],
            )?;
            Ok(())
        })
        .await
    }

    /// The store side of the exposure check (design §15.4).
    pub(crate) async fn exposure_inputs(&self) -> Result<ExposureInputs> {
        call(&self.inner.db, |conn| {
            let mut homes = strings(conn, "SELECT path FROM station_homes")?;
            homes.extend(strings(
                conn,
                "SELECT canonical_path FROM git_projects WHERE canonical_path IS NOT NULL",
            )?);
            homes.extend(strings(
                conn,
                "SELECT path FROM home_attempts
                 WHERE path IS NOT NULL AND state IN ('ready', 'adopted') AND superseded = 0",
            )?);
            // The reserved folder of a clone that is running or did not end well.
            homes.extend(strings(
                conn,
                "SELECT dest_path FROM home_attempts
                 WHERE state IN ('cloning', 'failed', 'interrupted')",
            )?);
            let mut worktrees = strings(
                conn,
                "SELECT worktree FROM sessions WHERE worktree IS NOT NULL",
            )?;
            worktrees.extend(strings(conn, "SELECT repo_root FROM session_git")?);
            Ok(ExposureInputs {
                homes,
                worktrees,
                snapshot_seen: git_state::snapshot_header(conn)?.is_some(),
            })
        })
        .await
    }
}

#[cfg(test)]
#[path = "artifacts_state_tests.rs"]
mod tests;
