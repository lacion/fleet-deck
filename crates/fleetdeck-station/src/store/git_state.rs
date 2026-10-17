//! The Fleet Git rows of the station store (design §3.5, V28): the settings
//! snapshot of the deck, the Fleet Git data of a session, and the push state
//! that survives a restart (FG-2, O11).
//!
//! Plain functions on a [`Connection`] are for the decision transaction; the
//! [`Store`] methods run one transaction each and append the `session.push`
//! events in the same transaction.

use fleetdeck_proto::fleet_git::{
    GitProjectView, GitSnapshot, HomeCheck, HomeCheckState, PushState, StationHome,
};
use fleetdeck_proto::{Event, SessionId};
use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::Serialize;

use super::{NewEvent, Result, Store, StoreError, call, log};

/// The station flag that holds the position of the push trigger task.
const CURSOR_FLAG: &str = "fleet_git_push_cursor";

/// The text of a unit enum value (`PushState::Due` is `due`).
fn text_of<T: Serialize>(value: &T) -> String {
    match serde_json::to_value(value) {
        Ok(serde_json::Value::String(s)) => s,
        _ => String::new(),
    }
}

/// A unit enum value from its text; an unknown text is `fallback`.
fn from_text<T: serde::de::DeserializeOwned>(text: &str, fallback: T) -> T {
    serde_json::from_value(serde_json::Value::String(text.to_owned())).unwrap_or(fallback)
}

/// One project of the newest snapshot, with the station's own home check.
#[derive(Debug, Clone, PartialEq)]
pub struct GitProjectRow {
    pub project_id: String,
    pub settings_revision: u64,
    pub home_generation: u64,
    pub home_path: String,
    pub canonical_path: Option<String>,
    pub check_state: HomeCheckState,
    pub check_code: Option<String>,
    pub view: GitProjectView,
}

fn project_of(r: &Row<'_>) -> rusqlite::Result<GitProjectRow> {
    let view: String = r.get(7)?;
    let view = serde_json::from_str(&view).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(7, rusqlite::types::Type::Text, Box::new(e))
    })?;
    let settings_revision: i64 = r.get(1)?;
    let home_generation: i64 = r.get(2)?;
    let state: String = r.get(5)?;
    Ok(GitProjectRow {
        project_id: r.get(0)?,
        settings_revision: u64::try_from(settings_revision).unwrap_or(0),
        home_generation: u64::try_from(home_generation).unwrap_or(0),
        home_path: r.get(3)?,
        canonical_path: r.get(4)?,
        check_state: from_text(&state, HomeCheckState::Unknown),
        check_code: r.get(6)?,
        view,
    })
}

const PROJECT_COLUMNS: &str = "project_id, settings_revision, home_generation, home_path,
     canonical_path, check_state, check_code, view";

/// The row of one project of the newest snapshot.
pub(crate) fn project_row(conn: &Connection, project_id: &str) -> Result<Option<GitProjectRow>> {
    Ok(conn
        .query_row(
            &format!("SELECT {PROJECT_COLUMNS} FROM git_projects WHERE project_id = ?1"),
            params![project_id],
            project_of,
        )
        .optional()?)
}

/// The Fleet Git data of a session (the columns of `session_git` that phase 1
/// uses).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionGitRow {
    pub session_id: SessionId,
    pub project_id: String,
    pub kind: String,
    pub slug: Option<String>,
    pub branch: Option<String>,
    pub publish_ref: Option<String>,
    pub created_branch: bool,
    pub repo_root: String,
    pub remote_dest: String,
    pub base_stale: bool,
}

/// A new `session_git` row (written by the start decision).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewSessionGit {
    pub session_id: SessionId,
    pub project_id: String,
    /// `worktree` or `in_place`.
    pub kind: &'static str,
    pub settings_revision: u64,
    pub home_generation: u64,
    pub slug: Option<String>,
    pub branch: Option<String>,
    pub publish_ref: Option<String>,
    pub created_branch: bool,
    pub repo_root: String,
    pub remote_dest: String,
}

fn to_i64(v: u64) -> i64 {
    i64::try_from(v).unwrap_or(i64::MAX)
}

/// Inserts the `session_git` row of a start.
pub(crate) fn insert_session_git(conn: &Connection, new: &NewSessionGit, now: i64) -> Result<()> {
    conn.execute(
        "INSERT INTO session_git
           (session_id, project_id, kind, settings_revision, home_generation, slug, branch,
            publish_ref, created_branch, repo_root, remote_dest, base_stale, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, 0, ?12)",
        params![
            new.session_id.as_str(),
            new.project_id,
            new.kind,
            to_i64(new.settings_revision),
            to_i64(new.home_generation),
            new.slug,
            new.branch,
            new.publish_ref,
            i64::from(new.created_branch),
            new.repo_root,
            new.remote_dest,
            now,
        ],
    )?;
    Ok(())
}

/// Inserts the push row of a session that got its own branch (state `idle`).
pub(crate) fn insert_push(
    conn: &Connection,
    session_id: &SessionId,
    (target, target_dest): (&str, &str),
    publish_ref: &str,
    now: i64,
) -> Result<()> {
    conn.execute(
        "INSERT INTO git_pushes (session_id, target, target_dest, ref, state, updated_at)
         VALUES (?1, ?2, ?3, ?4, 'idle', ?5)",
        params![session_id.as_str(), target, target_dest, publish_ref, now],
    )?;
    Ok(())
}

fn session_git_of(r: &Row<'_>) -> rusqlite::Result<SessionGitRow> {
    let created: i64 = r.get(6)?;
    let stale: i64 = r.get(9)?;
    let id: String = r.get(0)?;
    Ok(SessionGitRow {
        session_id: SessionId::from(id),
        project_id: r.get(1)?,
        kind: r.get(2)?,
        slug: r.get(3)?,
        branch: r.get(4)?,
        publish_ref: r.get(5)?,
        created_branch: created != 0,
        repo_root: r.get(7)?,
        remote_dest: r.get(8)?,
        base_stale: stale != 0,
    })
}

const SESSION_COLUMNS: &str = "session_id, project_id, kind, slug, branch, publish_ref,
     created_branch, repo_root, remote_dest, base_stale";

/// The push state of one session (the columns of `git_pushes`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushRow {
    pub session_id: SessionId,
    /// The remote name or the `wip_remote` URL, as configured.
    pub target: String,
    pub target_dest: String,
    pub ref_name: String,
    pub last_pushed_oid: Option<String>,
    pub desired_oid: Option<String>,
    pub attempt_id: Option<String>,
    pub attempt_oid: Option<String>,
    pub attempt_lease: Option<String>,
    pub attempt_url: Option<String>,
    pub state: PushState,
    pub stop_code: Option<String>,
    pub attempts: u32,
    pub next_attempt_at: Option<i64>,
    pub code: Option<String>,
    pub message: Option<String>,
}

const PUSH_COLUMNS: &str = "session_id, target, target_dest, ref, last_pushed_oid, desired_oid,
     attempt_id, attempt_oid, attempt_lease, attempt_url, state, stop_code, attempts,
     next_attempt_at, code, message";

fn push_of(r: &Row<'_>) -> rusqlite::Result<PushRow> {
    let id: String = r.get(0)?;
    let state: String = r.get(10)?;
    let attempts: i64 = r.get(12)?;
    Ok(PushRow {
        session_id: SessionId::from(id),
        target: r.get(1)?,
        target_dest: r.get(2)?,
        ref_name: r.get(3)?,
        last_pushed_oid: r.get(4)?,
        desired_oid: r.get(5)?,
        attempt_id: r.get(6)?,
        attempt_oid: r.get(7)?,
        attempt_lease: r.get(8)?,
        attempt_url: r.get(9)?,
        state: from_text(&state, PushState::Unknown),
        stop_code: r.get(11)?,
        attempts: u32::try_from(attempts).unwrap_or(u32::MAX),
        next_attempt_at: r.get(13)?,
        code: r.get(14)?,
        message: r.get(15)?,
    })
}

pub(crate) fn push_row(conn: &Connection, session_id: &SessionId) -> Result<Option<PushRow>> {
    Ok(conn
        .query_row(
            &format!("SELECT {PUSH_COLUMNS} FROM git_pushes WHERE session_id = ?1"),
            params![session_id.as_str()],
            push_of,
        )
        .optional()?)
}

/// The `session.push` event of a push row (design §3.6). `idle` is never
/// appended.
fn push_event(row: &PushRow) -> NewEvent {
    NewEvent::from(Event::SessionPush {
        state: row.state,
        ref_name: row.ref_name.clone(),
        oid: row.last_pushed_oid.clone(),
        desired_oid: row.desired_oid.clone(),
        stop_code: row.stop_code.clone(),
        attempts: row.attempts,
        next_attempt_at: row.next_attempt_at,
        code: row.code.clone(),
        text: row.message.clone(),
    })
}

/// Writes the mutable columns of `row` and appends its `session.push` event
/// (not for `idle`). The attempt columns follow `row`, so a caller that
/// changes the attempt changes them in `row` first.
pub(crate) fn save_push(
    conn: &Connection,
    row: &PushRow,
    now: i64,
    events: &mut Vec<fleetdeck_proto::EventEnvelope>,
) -> Result<()> {
    conn.execute(
        "UPDATE git_pushes SET last_pushed_oid = ?2,
           desired_oid = ?3, attempt_id = ?4, attempt_oid = ?5, attempt_lease = ?6,
           attempt_url = ?7, state = ?8, stop_code = ?9, attempts = ?10, next_attempt_at = ?11,
           code = ?12, message = ?13, updated_at = ?14
         WHERE session_id = ?1",
        params![
            row.session_id.as_str(),
            row.last_pushed_oid,
            row.desired_oid,
            row.attempt_id,
            row.attempt_oid,
            row.attempt_lease,
            row.attempt_url,
            text_of(&row.state),
            row.stop_code,
            i64::from(row.attempts),
            row.next_attempt_at,
            row.code,
            row.message,
            now,
        ],
    )?;
    if row.state != PushState::Idle {
        events.push(log::append_one(
            conn,
            &row.session_id,
            push_event(row),
            now,
        )?);
    }
    Ok(())
}

/// What a snapshot did to the station.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotResult {
    /// The snapshot is newer and now replaces the stored state.
    Stored,
    /// The same revision with the same digest: a retry (nothing changed).
    Same,
    /// A lower revision, or the same revision with another digest. The
    /// station keeps its own `revision`.
    Stale { revision: u64 },
}

/// The stored snapshot header: its revision and digest.
pub(crate) fn snapshot_header(conn: &Connection) -> Result<Option<(u64, String)>> {
    let found = conn
        .query_row(
            "SELECT revision, digest FROM git_snapshot WHERE id = 1",
            [],
            |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)),
        )
        .optional()?;
    Ok(found.map(|(rev, digest)| (u64::try_from(rev).unwrap_or(0), digest)))
}

fn compare(conn: &Connection, revision: u64, digest: &str) -> Result<Option<SnapshotResult>> {
    Ok(match snapshot_header(conn)? {
        Some((cur, _)) if revision < cur => Some(SnapshotResult::Stale { revision: cur }),
        Some((cur, stored)) if revision == cur => Some(if stored == digest {
            SnapshotResult::Same
        } else {
            SnapshotResult::Stale { revision: cur }
        }),
        _ => None,
    })
}

impl Store {
    /// Compares a snapshot with the stored one without storing it (design
    /// §3.3): `None` when it is newer.
    pub(crate) async fn git_snapshot_compare(
        &self,
        revision: u64,
        digest: String,
    ) -> Result<Option<SnapshotResult>> {
        call(&self.inner.db, move |conn| compare(conn, revision, &digest)).await
    }

    /// Replaces `git_projects` and `station_homes` with the snapshot in one
    /// transaction (design §3.3). `checks` has one entry for each project, in
    /// the order of `snapshot.projects`. A project that is no longer in the
    /// snapshot loses its row, and its push rows go to `stopped` with
    /// `stop_code = project_gone`. The revision is checked again in the
    /// transaction.
    pub(crate) async fn git_apply_snapshot(
        &self,
        snapshot: GitSnapshot,
        digest: String,
        checks: Vec<HomeCheck>,
    ) -> Result<SnapshotResult> {
        let now = self.now();
        let (result, events) = call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            if let Some(result) = compare(&tx, snapshot.revision, &digest)? {
                return Ok((result, Vec::new()));
            }
            let mut events = Vec::new();
            replace_snapshot(&tx, &snapshot, &checks, now, &mut events)?;
            tx.execute(
                "INSERT INTO git_snapshot (id, revision, digest, received_at)
                 VALUES (1, ?1, ?2, ?3)
                 ON CONFLICT (id) DO UPDATE SET revision = excluded.revision,
                   digest = excluded.digest, received_at = excluded.received_at",
                params![to_i64(snapshot.revision), digest, now],
            )?;
            tx.commit()?;
            Ok((SnapshotResult::Stored, events))
        })
        .await?;
        if let Some(last) = events.last() {
            self.publish(last.station_seq);
        }
        Ok(result)
    }

    /// Records that the fetch of a start failed (`session_git.base_stale`).
    pub(crate) async fn git_set_base_stale(&self, session_id: SessionId) -> Result<()> {
        call(&self.inner.db, move |conn| {
            conn.execute(
                "UPDATE session_git SET base_stale = 1 WHERE session_id = ?1",
                params![session_id.as_str()],
            )?;
            Ok(())
        })
        .await
    }

    /// The stored home check of every project (the answer to a retry of the
    /// newest snapshot). `seen` is not stored.
    pub(crate) async fn git_project_checks(&self) -> Result<Vec<HomeCheck>> {
        call(&self.inner.db, |conn| {
            let mut stmt = conn.prepare(&format!(
                "SELECT {PROJECT_COLUMNS} FROM git_projects ORDER BY project_id"
            ))?;
            let rows = stmt.query_map([], project_of)?;
            rows.map(|r| {
                let r = r?;
                Ok(HomeCheck {
                    project_id: r.project_id,
                    generation: r.home_generation,
                    state: r.check_state,
                    canonical_path: r.canonical_path,
                    seen: None,
                    code: r.check_code,
                })
            })
            .collect()
        })
        .await
    }

    /// The row of one project of the newest snapshot.
    pub(crate) async fn git_project(&self, project_id: String) -> Result<Option<GitProjectRow>> {
        call(&self.inner.db, move |conn| project_row(conn, &project_id)).await
    }

    /// The `session_git` row of a session.
    pub(crate) async fn git_session(&self, session_id: SessionId) -> Result<Option<SessionGitRow>> {
        call(&self.inner.db, move |conn| {
            Ok(conn
                .query_row(
                    &format!("SELECT {SESSION_COLUMNS} FROM session_git WHERE session_id = ?1"),
                    params![session_id.as_str()],
                    session_git_of,
                )
                .optional()?)
        })
        .await
    }

    /// The push row of a session.
    pub(crate) async fn git_push(&self, session_id: SessionId) -> Result<Option<PushRow>> {
        call(&self.inner.db, move |conn| push_row(conn, &session_id)).await
    }

    /// The sessions with a push row that is not `stopped` (the rescan after a
    /// gap in the event log).
    pub(crate) async fn git_push_live_sessions(&self) -> Result<Vec<SessionId>> {
        call(&self.inner.db, |conn| {
            let mut stmt = conn.prepare(
                "SELECT session_id FROM git_pushes WHERE state != 'stopped' ORDER BY session_id",
            )?;
            let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
            rows.map(|r| Ok(SessionId::from(r?))).collect()
        })
        .await
    }

    /// A trigger saw the local branch tip `tip` (design §5.4). It writes
    /// `desired_oid`, and a row in `idle` or `pushed` becomes `due` when the
    /// tip is not the last pushed commit. A `failing`, `due`, `pushing` or
    /// `stopped` row changes `desired_oid` only (no burst, decision 18).
    /// Returns the row, or `None` when the session has no push row.
    pub(crate) async fn git_push_trigger(
        &self,
        session_id: SessionId,
        tip: String,
    ) -> Result<Option<PushRow>> {
        let now = self.now();
        let (row, events) = call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            let Some(mut row) = push_row(&tx, &session_id)? else {
                return Ok((None, Vec::new()));
            };
            let before = row.clone();
            row.desired_oid = Some(tip.clone());
            if matches!(row.state, PushState::Idle | PushState::Pushed)
                && row.last_pushed_oid.as_deref() != Some(tip.as_str())
            {
                row.state = PushState::Due;
                row.next_attempt_at = Some(now);
            }
            let mut events = Vec::new();
            if row != before {
                save_push(&tx, &row, now, &mut events)?;
            }
            tx.commit()?;
            Ok((Some(row), events))
        })
        .await?;
        self.publish_events(&events);
        Ok(row)
    }

    /// "Push now" (design §5.6, §5.8): a `due` or `failing` row is due at
    /// once; `attempts` stays. Any other state is unchanged. Returns the row,
    /// or `None` when the session has no push row.
    pub(crate) async fn git_push_now(&self, session_id: SessionId) -> Result<Option<PushRow>> {
        let now = self.now();
        let (row, events) = call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            let Some(mut row) = push_row(&tx, &session_id)? else {
                return Ok((None, Vec::new()));
            };
            let mut events = Vec::new();
            if matches!(row.state, PushState::Due | PushState::Failing)
                && row.next_attempt_at != Some(now)
            {
                row.next_attempt_at = Some(now);
                save_push(&tx, &row, now, &mut events)?;
            }
            tx.commit()?;
            Ok((Some(row), events))
        })
        .await?;
        self.publish_events(&events);
        Ok(row)
    }

    /// The rows that the push loop works on now, oldest first: a `pushing`
    /// row (an attempt that a restart interrupted), and a `due` or `failing`
    /// row whose time has come, with the data of its session. Only rows of a
    /// session that has its own publication ref.
    pub(crate) async fn git_push_due(&self, now: i64) -> Result<Vec<(PushRow, SessionGitRow)>> {
        call(&self.inner.db, move |conn| {
            let mut stmt = conn.prepare(&format!(
                "SELECT {PUSH_COLUMNS} FROM git_pushes p
                 JOIN session_git s USING (session_id)
                 WHERE s.publish_ref IS NOT NULL AND s.created_branch = 1
                   AND (p.state = 'pushing'
                        OR (p.state IN ('due', 'failing')
                            AND (p.next_attempt_at IS NULL OR p.next_attempt_at <= ?1)))
                 ORDER BY COALESCE(p.next_attempt_at, 0), p.session_id"
            ))?;
            let rows = stmt.query_map(params![now], push_of)?;
            let mut out = Vec::new();
            for row in rows {
                let row = row?;
                let session = conn.query_row(
                    &format!("SELECT {SESSION_COLUMNS} FROM session_git WHERE session_id = ?1"),
                    params![row.session_id.as_str()],
                    session_git_of,
                )?;
                out.push((row, session));
            }
            Ok(out)
        })
        .await
    }

    /// Opens an attempt (design §5.5 step 2): the commit is `desired_oid`,
    /// the lease is `last_pushed_oid` (empty: the ref must not exist), the URL
    /// is the verified one. The three values never change while it is open. A
    /// row that is not `due` or `failing`, or has an attempt, is refused.
    pub(crate) async fn git_push_open_attempt(
        &self,
        session_id: SessionId,
        url: String,
    ) -> Result<PushRow> {
        let now = self.now();
        let attempt_id: String = fleetdeck_core::new_id(self.inner.ids.as_ref());
        let (row, events) = call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            let mut row = push_row(&tx, &session_id)?
                .ok_or_else(|| StoreError::UnknownSession(session_id.clone()))?;
            let desired = row.desired_oid.clone();
            let open = row.attempt_id.is_some();
            let (true, false, Some(desired)) = (
                matches!(row.state, PushState::Due | PushState::Failing),
                open,
                desired,
            ) else {
                return Err(StoreError::Corrupt(format!(
                    "the push row of {session_id} cannot open an attempt"
                )));
            };
            row.attempt_id = Some(attempt_id);
            row.attempt_oid = Some(desired);
            row.attempt_lease = Some(row.last_pushed_oid.clone().unwrap_or_default());
            row.attempt_url = Some(url);
            row.state = PushState::Pushing;
            let mut events = Vec::new();
            save_push(&tx, &row, now, &mut events)?;
            tx.commit()?;
            Ok((row, events))
        })
        .await?;
        self.publish_events(&events);
        Ok(row)
    }

    /// The attempt took effect (design §5.5 step 4): the lease moves to the
    /// attempt's commit, the attempt closes, and the row is `pushed`, or
    /// `due` when a newer tip came during the attempt.
    pub(crate) async fn git_push_applied(&self, session_id: SessionId) -> Result<Option<PushRow>> {
        let now = self.now();
        let (row, events) = call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            let Some(mut row) = push_row(&tx, &session_id)? else {
                return Ok((None, Vec::new()));
            };
            let Some(oid) = row.attempt_oid.take() else {
                return Ok((Some(row), Vec::new()));
            };
            row.last_pushed_oid = Some(oid.clone());
            row.attempt_id = None;
            row.attempt_lease = None;
            row.attempt_url = None;
            row.attempts = 0;
            row.code = None;
            row.message = None;
            if row.desired_oid.as_deref().is_none_or(|d| d == oid) {
                row.state = PushState::Pushed;
                row.next_attempt_at = None;
            } else {
                row.state = PushState::Due;
                row.next_attempt_at = Some(now);
            }
            tx.execute(
                "UPDATE git_pushes SET last_push_kind = 'push' WHERE session_id = ?1",
                params![session_id.as_str()],
            )?;
            let mut events = Vec::new();
            save_push(&tx, &row, now, &mut events)?;
            tx.commit()?;
            Ok((Some(row), events))
        })
        .await?;
        self.publish_events(&events);
        Ok(row)
    }

    /// A write failed (design §5.5 step 5): `attempts += 1`, the next
    /// attempt at `now + delay`, the code and the text. `keep_attempt` keeps
    /// an attempt whose outcome is unknown; otherwise the attempt closes
    /// (confirmed not applied, or nothing was sent).
    pub(crate) async fn git_push_failed(
        &self,
        session_id: SessionId,
        (code, text): (String, String),
        keep_attempt: bool,
        delay: impl FnOnce(u32) -> std::time::Duration + Send + 'static,
    ) -> Result<Option<PushRow>> {
        let now = self.now();
        let (row, events) = call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            let Some(mut row) = push_row(&tx, &session_id)? else {
                return Ok((None, Vec::new()));
            };
            if !keep_attempt {
                row.attempt_id = None;
                row.attempt_oid = None;
                row.attempt_lease = None;
                row.attempt_url = None;
            }
            row.attempts = row.attempts.saturating_add(1);
            let wait = i64::try_from(delay(row.attempts).as_millis()).unwrap_or(i64::MAX);
            row.next_attempt_at = Some(now.saturating_add(wait));
            row.state = PushState::Failing;
            row.code = Some(code);
            row.message = Some(text);
            let mut events = Vec::new();
            save_push(&tx, &row, now, &mut events)?;
            tx.commit()?;
            Ok((Some(row), events))
        })
        .await?;
        self.publish_events(&events);
        Ok(row)
    }

    /// The position of the push trigger task in the event log.
    pub(crate) async fn git_push_cursor(&self) -> Result<Option<u64>> {
        call(&self.inner.db, |conn| {
            let value: Option<String> = conn
                .query_row(
                    "SELECT value FROM station_flags WHERE key = ?1",
                    params![CURSOR_FLAG],
                    |r| r.get(0),
                )
                .optional()?;
            Ok(value.and_then(|v| v.parse().ok()))
        })
        .await
    }

    /// Stores the position of the push trigger task.
    pub(crate) async fn git_set_push_cursor(&self, cursor: u64) -> Result<()> {
        call(&self.inner.db, move |conn| {
            conn.execute(
                "INSERT INTO station_flags (key, value) VALUES (?1, ?2)
                 ON CONFLICT (key) DO UPDATE SET value = excluded.value",
                params![CURSOR_FLAG, cursor.to_string()],
            )?;
            Ok(())
        })
        .await
    }

    fn publish_events(&self, events: &[fleetdeck_proto::EventEnvelope]) {
        if let Some(last) = events.last() {
            self.publish(last.station_seq);
        }
    }
}

/// Replaces the project and home rows (design §3.3). The push rows of a
/// project that left the snapshot go to `stopped`.
fn replace_snapshot(
    conn: &Connection,
    snapshot: &GitSnapshot,
    checks: &[HomeCheck],
    now: i64,
    events: &mut Vec<fleetdeck_proto::EventEnvelope>,
) -> Result<()> {
    let kept: Vec<&str> = snapshot
        .projects
        .iter()
        .map(|p| p.project_id.as_str())
        .collect();
    let gone: Vec<String> = {
        let mut stmt = conn.prepare("SELECT project_id FROM git_projects ORDER BY project_id")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    }
    .into_iter()
    .filter(|id| !kept.contains(&id.as_str()))
    .collect();
    for project in &gone {
        stop_project(conn, project, now, events)?;
    }
    conn.execute("DELETE FROM git_projects", [])?;
    for (view, check) in snapshot.projects.iter().zip(checks) {
        conn.execute(
            "INSERT INTO git_projects
               (project_id, settings_revision, home_generation, home_path, canonical_path,
                check_state, check_code, view, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                view.project_id,
                to_i64(view.settings_revision),
                to_i64(view.home_generation),
                view.home_path,
                check.canonical_path,
                text_of(&check.state),
                check.code,
                serde_json::to_string(view)?,
                now,
            ],
        )?;
    }
    conn.execute("DELETE FROM station_homes", [])?;
    for StationHome {
        project_id,
        generation,
        path,
        sync,
    } in &snapshot.homes
    {
        conn.execute(
            "INSERT OR REPLACE INTO station_homes (project_id, generation, path, sync)
             VALUES (?1, ?2, ?3, ?4)",
            params![project_id, to_i64(*generation), path, i64::from(*sync)],
        )?;
    }
    Ok(())
}

/// Stops the push rows of the sessions of a project that left the snapshot.
fn stop_project(
    conn: &Connection,
    project_id: &str,
    now: i64,
    events: &mut Vec<fleetdeck_proto::EventEnvelope>,
) -> Result<()> {
    let ids: Vec<String> = {
        let mut stmt = conn.prepare(
            "SELECT p.session_id FROM git_pushes p JOIN session_git s USING (session_id)
             WHERE s.project_id = ?1 AND p.state != 'stopped' ORDER BY p.session_id",
        )?;
        let rows = stmt.query_map(params![project_id], |r| r.get::<_, String>(0))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };
    for id in ids {
        let id = SessionId::from(id);
        let Some(mut row) = push_row(conn, &id)? else {
            continue;
        };
        row.state = PushState::Stopped;
        row.stop_code = Some("project_gone".to_owned());
        save_push(conn, &row, now, events)?;
    }
    Ok(())
}

#[cfg(test)]
#[path = "git_state_tests.rs"]
pub(crate) mod tests;
