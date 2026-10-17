//! The home attempts of `project.ensure_home` (design §3.5 V29, §8.4, FG-5):
//! one row for each attempt to make a home on this station. The folder
//! reservation is the unique index `home_attempts_reserved`. Each [`Store`]
//! method runs one transaction.

use fleetdeck_proto::fleet_git::{HomeAttemptState, code};
use rusqlite::{Connection, OptionalExtension, Row, params};

use super::{Result, Store, call, git_state};

/// The code that frees a reservation whose folder is gone (design §8.4 step 5).
const DESTINATION_CLEARED: &str = code::DESTINATION_CLEARED;

/// One row of `home_attempts`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HomeAttemptRow {
    pub attempt_id: String,
    pub project_id: String,
    pub generation: u64,
    pub request_digest: String,
    pub retry_of: Option<String>,
    pub dest_path: String,
    pub url: String,
    pub remote_name: String,
    pub state: HomeAttemptState,
    pub superseded: bool,
    pub path: Option<String>,
    pub code: Option<String>,
    pub message: Option<String>,
}

/// The values of a new attempt (design §8.4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewHomeAttempt {
    pub attempt_id: String,
    pub project_id: String,
    pub generation: u64,
    pub request_digest: String,
    pub retry_of: Option<String>,
    pub dest_path: String,
    pub url: String,
    pub remote_name: String,
}

/// A refusal of steps 1 to 5: a wire code and the text for the board.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HomeRefusal {
    pub code: &'static str,
    pub text: String,
}

fn refuse(code: &'static str, text: impl Into<String>) -> HomeRefusal {
    HomeRefusal {
        code,
        text: text.into(),
    }
}

/// The result of steps 1 to 5.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BeginHome {
    /// Step 1 (the same attempt) or step 3 (the attempt of this
    /// generation): nothing starts again.
    Existing(HomeAttemptRow),
    /// A new attempt in state `cloning`, with its folder reserved.
    Created(HomeAttemptRow),
    Refused(HomeRefusal),
}

/// How an attempt ends (steps 6 and 7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HomeOutcome {
    pub state: HomeAttemptState,
    pub path: Option<String>,
    pub dev: Option<u64>,
    pub ino: Option<u64>,
    pub code: Option<String>,
    pub message: Option<String>,
}

fn state_text(state: HomeAttemptState) -> &'static str {
    match state {
        HomeAttemptState::Cloning => "cloning",
        HomeAttemptState::Ready => "ready",
        HomeAttemptState::Adopted => "adopted",
        HomeAttemptState::Failed => "failed",
        HomeAttemptState::Interrupted => "interrupted",
        HomeAttemptState::Cleared => "cleared",
        HomeAttemptState::Unknown => "unknown",
    }
}

fn state_of(text: &str) -> HomeAttemptState {
    match text {
        "cloning" => HomeAttemptState::Cloning,
        "ready" => HomeAttemptState::Ready,
        "adopted" => HomeAttemptState::Adopted,
        "failed" => HomeAttemptState::Failed,
        "interrupted" => HomeAttemptState::Interrupted,
        "cleared" => HomeAttemptState::Cleared,
        _ => HomeAttemptState::Unknown,
    }
}

fn to_i64(v: u64) -> i64 {
    i64::try_from(v).unwrap_or(i64::MAX)
}

const COLUMNS: &str = "attempt_id, project_id, generation, request_digest, retry_of, dest_path,
     url, remote_name, state, superseded, path, code, message";

fn row_of(r: &Row<'_>) -> rusqlite::Result<HomeAttemptRow> {
    let generation: i64 = r.get(2)?;
    let state: String = r.get(8)?;
    let superseded: i64 = r.get(9)?;
    Ok(HomeAttemptRow {
        attempt_id: r.get(0)?,
        project_id: r.get(1)?,
        generation: u64::try_from(generation).unwrap_or(0),
        request_digest: r.get(3)?,
        retry_of: r.get(4)?,
        dest_path: r.get(5)?,
        url: r.get(6)?,
        remote_name: r.get(7)?,
        state: state_of(&state),
        superseded: superseded != 0,
        path: r.get(10)?,
        code: r.get(11)?,
        message: r.get(12)?,
    })
}

fn by_id(conn: &Connection, attempt_id: &str) -> Result<Option<HomeAttemptRow>> {
    Ok(conn
        .query_row(
            &format!("SELECT {COLUMNS} FROM home_attempts WHERE attempt_id = ?1"),
            params![attempt_id],
            row_of,
        )
        .optional()?)
}

/// The newest attempt of `(project_id, generation)`.
fn newest(conn: &Connection, project_id: &str, generation: u64) -> Result<Option<HomeAttemptRow>> {
    Ok(conn
        .query_row(
            &format!(
                "SELECT {COLUMNS} FROM home_attempts WHERE project_id = ?1 AND generation = ?2
                 ORDER BY started_at DESC, rowid DESC LIMIT 1"
            ),
            params![project_id, to_i64(generation)],
            row_of,
        )
        .optional()?)
}

/// Does anything exist at `path`, not even a dangling link? Only `ENOENT`
/// counts as absent (design §8.4 step 5).
fn exists(path: &str) -> bool {
    match std::fs::symlink_metadata(path) {
        Ok(_) => true,
        Err(e) => e.kind() != std::io::ErrorKind::NotFound,
    }
}

fn clear(conn: &Connection, attempt_id: &str, now: i64) -> Result<()> {
    conn.execute(
        "UPDATE home_attempts SET state = 'cleared', code = ?2, message = NULL, updated_at = ?3
         WHERE attempt_id = ?1",
        params![attempt_id, DESTINATION_CLEARED, now],
    )?;
    tracing::info!(
        attempt = attempt_id,
        "home attempt cleared: its folder is gone"
    );
    Ok(())
}

/// Steps 1 to 5 of design §8.4, inside `conn`'s transaction. A refusal
/// returns early; the caller rolls the transaction back.
fn begin(conn: &Connection, new: &NewHomeAttempt, now: i64) -> Result<BeginHome> {
    // 1. The same attempt.
    if let Some(row) = by_id(conn, &new.attempt_id)? {
        return Ok(if row.request_digest == new.request_digest {
            BeginHome::Existing(row)
        } else {
            BeginHome::Refused(refuse(
                code::ATTEMPT_REUSED,
                "This attempt id was used for another request.",
            ))
        });
    }
    // 2. The generation fence.
    let higher: Option<i64> = conn
        .query_row(
            "SELECT 1 FROM home_attempts WHERE project_id = ?1 AND generation > ?2 LIMIT 1",
            params![new.project_id, to_i64(new.generation)],
            |r| r.get(0),
        )
        .optional()?;
    if higher.is_some() {
        return Ok(BeginHome::Refused(refuse(
            code::STALE_GENERATION,
            "The home of this project changed. Start again.",
        )));
    }
    let current = newest(conn, &new.project_id, new.generation)?;
    match &new.retry_of {
        // 3. One attempt at a time.
        None => {
            if let Some(row) = current {
                return Ok(BeginHome::Existing(row));
            }
        }
        // 4. Explicit retry.
        Some(x) => {
            let retryable = current.filter(|row| {
                row.attempt_id == *x
                    && matches!(
                        row.state,
                        HomeAttemptState::Failed | HomeAttemptState::Interrupted
                    )
            });
            let Some(row) = retryable else {
                return Ok(BeginHome::Refused(refuse(
                    code::RETRY_REFUSED,
                    "Only the newest failed attempt can be retried.",
                )));
            };
            if exists(&row.dest_path) {
                return Ok(BeginHome::Refused(refuse(
                    code::INCOMPLETE_FOLDER,
                    format!(
                        "The folder {} holds an incomplete clone. Remove it, then retry.",
                        row.dest_path
                    ),
                )));
            }
            clear(conn, &row.attempt_id, now)?;
        }
    }
    // 5. Reserve the destination. First clear the failed and interrupted
    // rows of any project whose folder is gone.
    let stale: Vec<String> = {
        let mut stmt = conn.prepare(
            "SELECT attempt_id FROM home_attempts
             WHERE dest_path = ?1 AND state IN ('failed', 'interrupted') ORDER BY rowid",
        )?;
        let rows = stmt.query_map(params![new.dest_path], |r| r.get::<_, String>(0))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };
    if !stale.is_empty() && !exists(&new.dest_path) {
        for id in &stale {
            clear(conn, id, now)?;
        }
    }
    let inserted = conn.execute(
        "INSERT INTO home_attempts
           (attempt_id, project_id, generation, request_digest, retry_of, dest_path, url,
            remote_name, state, superseded, started_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'cloning', 0, ?9, ?9)",
        params![
            new.attempt_id,
            new.project_id,
            to_i64(new.generation),
            new.request_digest,
            new.retry_of,
            new.dest_path,
            new.url,
            new.remote_name,
            now,
        ],
    );
    match inserted {
        Ok(_) => {}
        Err(rusqlite::Error::SqliteFailure(e, _))
            if e.code == rusqlite::ErrorCode::ConstraintViolation =>
        {
            return Ok(BeginHome::Refused(reserved_refusal(conn, new)?));
        }
        Err(e) => return Err(e.into()),
    }
    let row = by_id(conn, &new.attempt_id)?.ok_or_else(|| {
        super::StoreError::Corrupt("a home attempt is missing after its insert".to_owned())
    })?;
    Ok(BeginHome::Created(row))
}

/// The refusal for a folder that another attempt holds (design §8.4 step 5).
fn reserved_refusal(conn: &Connection, new: &NewHomeAttempt) -> Result<HomeRefusal> {
    let holder: Option<(String, String)> = conn
        .query_row(
            "SELECT project_id, state FROM home_attempts
             WHERE dest_path = ?1
               AND (state IN ('cloning', 'failed', 'interrupted')
                    OR (state IN ('ready', 'adopted') AND superseded = 0))
             LIMIT 1",
            params![new.dest_path],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((project_id, state)) = holder else {
        return Err(super::StoreError::Corrupt(
            "a home attempt was refused with no holder of its folder".to_owned(),
        ));
    };
    if project_id == new.project_id && matches!(state.as_str(), "failed" | "interrupted") {
        return Ok(refuse(
            code::INCOMPLETE_FOLDER,
            format!(
                "The folder {} holds an incomplete clone from an earlier attempt. Remove it, then add the home again.",
                new.dest_path
            ),
        ));
    }
    // The name comes from the newest snapshot; else the project id.
    let name =
        git_state::project_row(conn, &project_id)?.map_or(project_id, |row| row.view.project_name);
    Ok(refuse(
        code::DESTINATION_RESERVED,
        format!(
            "The folder {} is reserved by project {name} on this station.",
            new.dest_path
        ),
    ))
}

impl Store {
    /// Steps 1 to 5 of design §8.4 in one transaction. A refusal changes
    /// nothing.
    pub(crate) async fn home_begin(&self, new: NewHomeAttempt) -> Result<BeginHome> {
        let now = self.now();
        call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            let result = begin(&tx, &new, now)?;
            if matches!(result, BeginHome::Refused(_)) {
                return Ok(result);
            }
            tx.commit()?;
            Ok(result)
        })
        .await
    }

    /// The attempt `attempt_id` of `project_id`.
    pub(crate) async fn home_attempt(
        &self,
        project_id: String,
        attempt_id: String,
    ) -> Result<Option<HomeAttemptRow>> {
        call(&self.inner.db, move |conn| {
            Ok(by_id(conn, &attempt_id)?.filter(|row| row.project_id == project_id))
        })
        .await
    }

    /// Ends an attempt: the state, the verified path and the code.
    pub(crate) async fn home_finish(
        &self,
        attempt_id: String,
        outcome: HomeOutcome,
    ) -> Result<Option<HomeAttemptRow>> {
        let now = self.now();
        call(&self.inner.db, move |conn| {
            conn.execute(
                "UPDATE home_attempts
                 SET state = ?2, path = ?3, dev = ?4, ino = ?5, code = ?6, message = ?7,
                     updated_at = ?8
                 WHERE attempt_id = ?1",
                params![
                    attempt_id,
                    state_text(outcome.state),
                    outcome.path,
                    outcome.dev.map(to_i64),
                    outcome.ino.map(to_i64),
                    outcome.code,
                    outcome.message,
                    now,
                ],
            )?;
            by_id(conn, &attempt_id)
        })
        .await
    }

    /// At a station start: every `cloning` attempt becomes `interrupted` with
    /// `clone_interrupted` (design §8.4, §8.7). The partial folder stays.
    pub(crate) async fn home_interrupt_cloning(&self) -> Result<usize> {
        let now = self.now();
        call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            let rows: Vec<(String, String)> = {
                let mut stmt = tx.prepare(
                    "SELECT attempt_id, dest_path FROM home_attempts WHERE state = 'cloning'
                     ORDER BY rowid",
                )?;
                let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
                rows.collect::<rusqlite::Result<Vec<_>>>()?
            };
            for (id, dest) in &rows {
                let text = format!(
                    "The clone stopped when the station restarted. Remove {dest}, then retry."
                );
                tx.execute(
                    "UPDATE home_attempts
                     SET state = 'interrupted', code = ?2, message = ?3, updated_at = ?4
                     WHERE attempt_id = ?1",
                    params![id, code::CLONE_INTERRUPTED, text, now],
                )?;
            }
            tx.commit()?;
            Ok(rows.len())
        })
        .await
    }

    /// The snapshot invalidation (design §8.4 "Remove a home", decision A16):
    /// `homes` are the `(project_id, generation)` of the snapshot. An attempt
    /// that is not `cloning` is superseded when its project has no home, or
    /// a home of a higher generation. No file is deleted.
    pub(crate) async fn home_supersede(&self, homes: Vec<(String, u64)>) -> Result<usize> {
        let now = self.now();
        call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            let rows: Vec<(String, String, u64)> = {
                let mut stmt = tx.prepare(
                    "SELECT attempt_id, project_id, generation FROM home_attempts
                     WHERE superseded = 0 AND state NOT IN ('cloning', 'cleared')
                     ORDER BY rowid",
                )?;
                let rows = stmt.query_map([], |r| {
                    let generation: i64 = r.get(2)?;
                    Ok((r.get(0)?, r.get(1)?, u64::try_from(generation).unwrap_or(0)))
                })?;
                rows.collect::<rusqlite::Result<Vec<_>>>()?
            };
            let mut count = 0;
            for (id, project, generation) in rows {
                let home = homes.iter().find(|(p, _)| *p == project).map(|(_, g)| *g);
                if home.is_some_and(|g| generation >= g) {
                    continue;
                }
                tx.execute(
                    "UPDATE home_attempts SET superseded = 1, updated_at = ?2
                     WHERE attempt_id = ?1",
                    params![id, now],
                )?;
                count += 1;
            }
            tx.commit()?;
            Ok(count)
        })
        .await
    }
}

#[cfg(test)]
#[path = "home_attempts_tests.rs"]
mod tests;
