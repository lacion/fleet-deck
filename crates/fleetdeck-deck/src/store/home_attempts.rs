//! The home attempts of the deck (design §8.2, §8.4; FG-5 phase 2, M5): the
//! stored `project.ensure_home` requests that the sender delivers, and what
//! the answers of the station do to the attempt and to its home.
//!
//! - A row in state `sent` with a `next_send_at` is due; with no
//!   `next_send_at` it waits for the next link start of its station.
//! - An answer applies only through the fence: `(attempt_id, generation)`
//!   equal the `project_homes` row (decision B6c).

use fleetdeck_proto::fleet_git::{
    EnsureHomeParams, GitHomeState, HomeAttemptState, HomeStatus, code,
};
use rusqlite::{Connection, OptionalExtension, params};

use super::project_homes::{HomeRow, become_ready, home_on, set_state};
use super::{DeckStore, Result, call, to_i64, to_u64};

/// The longest wait between two sends of one attempt (design §8.4).
const BACKOFF_CAP_MS: i64 = 60_000;

/// The wait after the `n`th send: 2, 4, 8, ... 60 s.
#[must_use]
pub fn backoff_ms(sends: u64) -> i64 {
    let shift = u32::try_from(sends).unwrap_or(u32::MAX).min(16);
    (1_000_i64 << shift).min(BACKOFF_CAP_MS)
}

/// One row of `home_attempts`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttemptRow {
    pub attempt_id: String,
    pub command_id: String,
    pub project_id: String,
    pub station_id: String,
    pub generation: u64,
    pub request: EnsureHomeParams,
    pub retry_of: Option<String>,
    pub state: String,
    pub send_attempts: u64,
    pub next_send_at: Option<i64>,
}

const ATTEMPT_COLUMNS: &str = "attempt_id, request_command_id, project_id, station_id, generation, request, retry_of, state, send_attempts, next_send_at";

fn read(r: &rusqlite::Row<'_>) -> Result<AttemptRow> {
    Ok(AttemptRow {
        attempt_id: r.get(0)?,
        command_id: r.get(1)?,
        project_id: r.get(2)?,
        station_id: r.get(3)?,
        generation: to_u64(r.get(4)?)?,
        request: serde_json::from_str(&r.get::<_, String>(5)?)?,
        retry_of: r.get(6)?,
        state: r.get(7)?,
        send_attempts: to_u64(r.get(8)?)?,
        next_send_at: r.get(9)?,
    })
}

pub(super) fn find(conn: &Connection, attempt_id: &str) -> Result<Option<AttemptRow>> {
    conn.query_row(
        &format!("SELECT {ATTEMPT_COLUMNS} FROM home_attempts WHERE attempt_id = ?1"),
        [attempt_id],
        |r| Ok(read(r)),
    )
    .optional()?
    .transpose()
}

pub(super) fn find_by_command(conn: &Connection, command_id: &str) -> Result<Option<AttemptRow>> {
    conn.query_row(
        &format!("SELECT {ATTEMPT_COLUMNS} FROM home_attempts WHERE request_command_id = ?1"),
        [command_id],
        |r| Ok(read(r)),
    )
    .optional()?
    .transpose()
}

/// Stores a new attempt of `station` in state `sent`, due now.
pub(super) fn insert(
    conn: &Connection,
    station: &str,
    request: &EnsureHomeParams,
    command_id: &str,
    now: i64,
) -> Result<()> {
    conn.execute(
        "INSERT INTO home_attempts (attempt_id, request_command_id, project_id, station_id,
             generation, request_digest, request, retry_of, state, next_send_at, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'sent', ?9, ?9)",
        params![
            request.attempt_id,
            command_id,
            request.project_id,
            station,
            to_i64(request.generation)?,
            request.request_digest,
            serde_json::to_string(request)?,
            request.retry_of,
            now,
        ],
    )?;
    Ok(())
}

/// Does the home row of the attempt still name it with its generation?
fn names(home: Option<&HomeRow>, attempt: &AttemptRow) -> bool {
    home.is_some_and(|h| {
        h.attempt_id.as_deref() == Some(attempt.attempt_id.as_str())
            && h.generation == attempt.generation
    })
}

/// What an answer of the station did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Applied {
    /// The fence dropped the answer (`home_status_stale`).
    Stale,
    /// The answer changed nothing.
    Unchanged,
    /// The attempt and its home moved on.
    Changed {
        /// The home is `ready` now: the station gets a new snapshot.
        ready: bool,
    },
}

fn end_attempt(
    conn: &Connection,
    attempt: &str,
    state: &str,
    (path, code, message): (Option<&str>, Option<&str>, Option<&str>),
    now: i64,
) -> Result<()> {
    conn.execute(
        "UPDATE home_attempts SET state = ?2, path = ?3, code = ?4, message = ?5,
             next_send_at = NULL, ended_at = ?6
         WHERE attempt_id = ?1",
        params![attempt, state, path, code, message, now],
    )?;
    Ok(())
}

fn fail_home(
    conn: &Connection,
    home: &HomeRow,
    code: &str,
    message: Option<&str>,
    reserved: Option<&str>,
    now: i64,
) -> Result<()> {
    if let Some(reserved) = reserved {
        conn.execute(
            "UPDATE project_homes SET reserved_path = ?3
             WHERE project_id = ?1 AND station_id = ?2",
            params![home.project_id, home.station_id, reserved],
        )?;
    }
    set_state(
        conn,
        (&home.project_id, &home.station_id),
        GitHomeState::Failed,
        Some(code),
        message,
        now,
    )
}

/// Applies a `HomeStatus` of the station through the fence (design §8.4,
/// decisions B6c, B6d).
fn apply_status(conn: &Connection, status: &HomeStatus, now: i64) -> Result<Applied> {
    let Some(attempt) = find(conn, &status.attempt_id)? else {
        return Ok(Applied::Stale);
    };
    let home = home_on(conn, &attempt.project_id, &attempt.station_id)?;
    let Some(home) = home.filter(|h| names(Some(h), &attempt) && h.generation == status.generation)
    else {
        return Ok(Applied::Stale);
    };
    let ids = (home.project_id.as_str(), home.station_id.as_str());
    let reserved = status.reserved_path.as_deref();
    match status.state {
        HomeAttemptState::Cloning => {
            end_attempt_open(conn, &attempt.attempt_id, "cloning")?;
            if let Some(reserved) = reserved {
                conn.execute(
                    "UPDATE project_homes SET reserved_path = ?3
                     WHERE project_id = ?1 AND station_id = ?2",
                    params![ids.0, ids.1, reserved],
                )?;
            }
            Ok(Applied::Changed { ready: false })
        }
        HomeAttemptState::Ready | HomeAttemptState::Adopted => {
            let Some(path) = status.path.as_deref() else {
                tracing::warn!(attempt = %attempt.attempt_id, "home_status_without_path");
                return Ok(Applied::Unchanged);
            };
            let name = if status.state == HomeAttemptState::Ready {
                "ready"
            } else {
                "adopted"
            };
            end_attempt(
                conn,
                &attempt.attempt_id,
                name,
                (Some(path), None, None),
                now,
            )?;
            conn.execute(
                "UPDATE project_homes SET path = ?3, canonical_path = ?3,
                     reserved_path = COALESCE(?4, reserved_path)
                 WHERE project_id = ?1 AND station_id = ?2",
                params![ids.0, ids.1, path, reserved],
            )?;
            let sync: bool = conn.query_row(
                "SELECT COALESCE(json_extract(git, '$.sync') = 1, 0) FROM projects
                 WHERE project_id = ?1",
                [ids.0],
                |r| r.get(0),
            )?;
            if sync {
                become_ready(conn, ids, path, now)?;
            } else {
                set_state(conn, ids, GitHomeState::Ready, None, None, now)?;
            }
            Ok(Applied::Changed { ready: true })
        }
        HomeAttemptState::Failed | HomeAttemptState::Interrupted => {
            let (name, fallback) = if status.state == HomeAttemptState::Failed {
                ("failed", code::CLONE_FAILED)
            } else {
                ("interrupted", code::CLONE_INTERRUPTED)
            };
            let why = status.code.as_deref().unwrap_or(fallback);
            let text = status.message.as_deref();
            end_attempt(
                conn,
                &attempt.attempt_id,
                name,
                (None, Some(why), text),
                now,
            )?;
            fail_home(conn, &home, why, text, reserved, now)?;
            Ok(Applied::Changed { ready: false })
        }
        HomeAttemptState::Cleared => {
            let text = status
                .message
                .as_deref()
                .unwrap_or("The reserved folder no longer exists. Retry or remove the home.");
            end_attempt(
                conn,
                &attempt.attempt_id,
                "cleared",
                (None, Some(code::DESTINATION_CLEARED), Some(text)),
                now,
            )?;
            fail_home(
                conn,
                &home,
                code::DESTINATION_CLEARED,
                Some(text),
                reserved,
                now,
            )?;
            Ok(Applied::Changed { ready: false })
        }
        HomeAttemptState::Unknown => {
            tracing::warn!(attempt = %attempt.attempt_id, "home_status_unknown_state");
            Ok(Applied::Unchanged)
        }
    }
}

/// An answer moved the attempt to a state that is not over: no more sends.
fn end_attempt_open(conn: &Connection, attempt: &str, state: &str) -> Result<()> {
    conn.execute(
        "UPDATE home_attempts SET state = ?2, next_send_at = NULL WHERE attempt_id = ?1",
        params![attempt, state],
    )?;
    Ok(())
}

/// The error codes of `project.ensure_home` and `project.home_status` that
/// end the attempt (decisions B6c, B15).
#[must_use]
pub fn fails_attempt(error_code: &str) -> bool {
    matches!(
        error_code,
        "attempt_reused"
            | "retry_refused"
            | "stale_generation"
            | "invalid_params"
            | "incomplete_folder"
            | "destination_reserved"
            | "clone_root_missing"
            | "home_conflict"
            | "clone_failed"
            | "forbidden"
    )
}

/// A cloning home that the poller asks about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloningHome {
    pub project_id: String,
    pub station_id: String,
    pub attempt_id: String,
}

/// A due attempt for the sender.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Due {
    pub attempt_id: String,
    pub station_id: String,
    pub request: EnsureHomeParams,
    pub send_attempts: u64,
}

impl DeckStore {
    /// The attempts in state `sent` that are due at `now`. A row whose home
    /// no longer names it becomes `superseded` and is not returned.
    ///
    /// # Errors
    /// A database or stored-JSON error.
    pub async fn home_attempts_due(&self, now: i64) -> Result<Vec<Due>> {
        call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            let ids: Vec<String> = {
                let mut stmt = tx.prepare(
                    "SELECT attempt_id FROM home_attempts
                     WHERE state = 'sent' AND next_send_at IS NOT NULL AND next_send_at <= ?1
                     ORDER BY next_send_at, created_at",
                )?;
                stmt.query_map([now], |r| r.get(0))?
                    .collect::<rusqlite::Result<_>>()?
            };
            let mut due = Vec::new();
            for id in ids {
                let Some(attempt) = find(&tx, &id)? else {
                    continue;
                };
                let home = home_on(&tx, &attempt.project_id, &attempt.station_id)?;
                if !names(home.as_ref(), &attempt) {
                    end_attempt(&tx, &id, "superseded", (None, None, None), now)?;
                    continue;
                }
                due.push(Due {
                    attempt_id: id,
                    station_id: attempt.station_id,
                    request: attempt.request,
                    send_attempts: attempt.send_attempts,
                });
            }
            tx.commit()?;
            Ok(due)
        })
        .await
    }

    /// The earliest `next_send_at` of an attempt in state `sent`.
    ///
    /// # Errors
    /// A database error.
    pub async fn home_attempts_next_due(&self) -> Result<Option<i64>> {
        call(&self.inner.db, |conn| {
            Ok(conn.query_row(
                "SELECT MIN(next_send_at) FROM home_attempts WHERE state = 'sent'",
                [],
                |r| r.get(0),
            )?)
        })
        .await
    }

    /// A send failed (a timeout, a transport error, a link that is down, an
    /// error that does not end the attempt): `send_attempts + 1` and the next
    /// send after the backoff.
    ///
    /// # Errors
    /// A database error.
    pub async fn home_attempt_backoff(&self, attempt_id: String, now: i64) -> Result<()> {
        call(&self.inner.db, move |conn| {
            let sends: i64 = conn.query_row(
                "SELECT send_attempts + 1 FROM home_attempts WHERE attempt_id = ?1",
                [&attempt_id],
                |r| r.get(0),
            )?;
            conn.execute(
                "UPDATE home_attempts SET send_attempts = ?2, next_send_at = ?3
                 WHERE attempt_id = ?1 AND state = 'sent'",
                params![attempt_id, sends, now + backoff_ms(to_u64(sends)?)],
            )?;
            Ok(())
        })
        .await
    }

    /// The station has no `fleet_git.homes.v1`: the attempt stays `sent`, is
    /// not sent and is due again at the next link start (decision B6c).
    ///
    /// # Errors
    /// A database error.
    pub async fn home_attempt_wait_for_link(&self, attempt_id: String) -> Result<()> {
        call(&self.inner.db, move |conn| {
            conn.execute(
                "UPDATE home_attempts SET next_send_at = NULL
                 WHERE attempt_id = ?1 AND state = 'sent'",
                [&attempt_id],
            )?;
            Ok(())
        })
        .await
    }

    /// A link of `station` started: every attempt in state `sent` is due.
    ///
    /// # Errors
    /// A database error.
    pub async fn home_attempts_link_start(&self, station: String, now: i64) -> Result<()> {
        call(&self.inner.db, move |conn| {
            conn.execute(
                "UPDATE home_attempts SET next_send_at = ?2
                 WHERE station_id = ?1 AND state = 'sent'",
                params![station, now],
            )?;
            Ok(())
        })
        .await
    }

    /// The homes in state `cloning` whose attempt is `cloning` (an attempt in
    /// `sent` is not polled).
    ///
    /// # Errors
    /// A database error.
    pub async fn cloning_homes(&self) -> Result<Vec<CloningHome>> {
        call(&self.inner.db, |conn| {
            let mut stmt = conn.prepare(
                "SELECT h.project_id, h.station_id, h.attempt_id FROM project_homes h
                 JOIN home_attempts a ON a.attempt_id = h.attempt_id
                 WHERE h.state = 'cloning' AND a.state = 'cloning'
                 ORDER BY h.project_id, h.station_id",
            )?;
            let rows = stmt
                .query_map([], |r| {
                    Ok(CloningHome {
                        project_id: r.get(0)?,
                        station_id: r.get(1)?,
                        attempt_id: r.get(2)?,
                    })
                })?
                .collect::<rusqlite::Result<_>>()?;
            Ok(rows)
        })
        .await
    }

    /// Applies an answer of the station through the fence.
    ///
    /// # Errors
    /// A database error.
    pub async fn home_status_apply(&self, status: HomeStatus) -> Result<Applied> {
        let now = self.now_ms();
        let out = call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            let applied = apply_status(&tx, &status, now)?;
            if applied != Applied::Stale
                && let Some(attempt) = find(&tx, &status.attempt_id)?
            {
                super::projects_legacy::set_copy(&tx, &attempt.project_id)?;
            }
            tx.commit()?;
            Ok(applied)
        })
        .await;
        self.board_changed();
        out
    }

    /// A link error that ends the attempt (`fails_attempt`): the attempt and,
    /// through the fence, its home become `failed` with the code and the text.
    ///
    /// # Errors
    /// A database error.
    pub async fn home_attempt_fail(
        &self,
        attempt_id: String,
        error_code: String,
        message: String,
    ) -> Result<Applied> {
        let now = self.now_ms();
        let out = call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            let Some(attempt) = find(&tx, &attempt_id)? else {
                return Ok(Applied::Stale);
            };
            let home = home_on(&tx, &attempt.project_id, &attempt.station_id)?;
            let Some(home) = home.filter(|h| names(Some(h), &attempt)) else {
                return Ok(Applied::Stale);
            };
            end_attempt(
                &tx,
                &attempt_id,
                "failed",
                (None, Some(&error_code), Some(&message)),
                now,
            )?;
            fail_home(&tx, &home, &error_code, Some(&message), None, now)?;
            tx.commit()?;
            Ok(Applied::Changed { ready: false })
        })
        .await;
        self.board_changed();
        out
    }

    /// The station does not know a `cloning` attempt: it goes back to `sent`
    /// and is due now (decision B6c).
    ///
    /// # Errors
    /// A database error.
    pub async fn home_attempt_unknown(&self, attempt_id: String) -> Result<()> {
        let now = self.now_ms();
        call(&self.inner.db, move |conn| {
            conn.execute(
                "UPDATE home_attempts SET state = 'sent', next_send_at = ?2
                 WHERE attempt_id = ?1 AND state = 'cloning'",
                params![attempt_id, now],
            )?;
            Ok(())
        })
        .await
    }

    /// The state of an attempt (tests and the board).
    ///
    /// # Errors
    /// A database error.
    pub async fn home_attempt_state(&self, attempt_id: String) -> Result<Option<String>> {
        call(&self.inner.db, move |conn| {
            Ok(find(conn, &attempt_id)?.map(|a| a.state))
        })
        .await
    }
}
