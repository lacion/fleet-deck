//! The Fleet Deck counters of the analytics page (B29, design `analytics.md`
//! section 3.6 and 3.6a item 1).
//!
//! One ordered cursor, `analytics_meta.next_seq`, counts every event once, in
//! `station_seq` order, through [`observe_one`]. [`observe`] runs in the
//! append transaction after `workflow_state::observe` and counts the event
//! only when the cursor is caught up. [`catch_up_step`] counts the events
//! that the cursor is behind, in order. The store's one database thread
//! serializes the two callers. Only a database error stops the count: an
//! event that does not decode counts nothing.

use fleetdeck_proto::analytics::{HOUR_MS, metric};
use fleetdeck_proto::event::{Event, SessionState, ToolStatus, TurnStatus};
use fleetdeck_proto::{EventBody, EventEnvelope, PROTO_MAX, SessionId};
use rusqlite::{Connection, OptionalExtension, params};

use super::{Result, Store, call, to_i64, to_u64};

/// The kinds that [`observe_one`] counts. Every other kind costs no JSON
/// work (`message.delta` first of all).
fn counts_kind(kind: &str) -> bool {
    matches!(
        kind,
        "session.started"
            | "run.started"
            | "turn.started"
            | "turn.completed"
            | "tool.started"
            | "tool.completed"
            | "approval.requested"
            | "approval.resolved"
            | "approval.expired"
            | "error"
            | "turn.finalized"
            | "artifact.published"
            | "workflow.updated"
            | "run.completed"
            | "session.ended"
            | "session.state"
    )
}

/// A unit enum's serde name; `other` for a value that is not a string.
fn name<T: serde::Serialize>(value: &T) -> String {
    match serde_json::to_value(value) {
        Ok(serde_json::Value::String(s)) => s,
        _ => "other".to_owned(),
    }
}

/// An error code that is a metric key part: `^[a-z][a-z0-9_]{0,31}$`.
fn code_ok(code: &str) -> bool {
    let mut chars = code.chars();
    code.len() <= 32
        && chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

fn int(v: u64) -> i64 {
    i64::try_from(v).unwrap_or(i64::MAX)
}

/// The facts that every counter of one event shares.
struct Counter<'a> {
    conn: &'a Connection,
    hour: i64,
    session: &'a str,
    at: i64,
    /// The `station_seq` of the event.
    seq: i64,
}

impl Counter<'_> {
    /// `analytics_activity(hour, session, metric) += n`.
    fn add(&self, key: &str, n: u64) -> Result<()> {
        if n == 0 {
            return Ok(());
        }
        self.conn
            .prepare_cached(
                "INSERT INTO analytics_activity (hour, session_id, metric, value)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT (hour, session_id, metric) DO UPDATE SET value = value + excluded.value",
            )?
            .execute(params![self.hour, self.session, key, int(n)])?;
        Ok(())
    }

    fn open(&self, kind: &str, id: &str, detail: Option<&str>) -> Result<()> {
        self.conn
            .prepare_cached(
                "INSERT OR REPLACE INTO analytics_open (session_id, kind, id, started_at, detail)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
            )?
            .execute(params![self.session, kind, id, self.at, detail])?;
        Ok(())
    }

    /// Deletes the open row and returns its start and detail.
    fn take(&self, kind: &str, id: &str) -> Result<Option<(i64, Option<String>)>> {
        let row = self
            .conn
            .prepare_cached(
                "SELECT started_at, detail FROM analytics_open
                 WHERE session_id = ?1 AND kind = ?2 AND id = ?3",
            )?
            .query_row(params![self.session, kind, id], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .optional()?;
        if row.is_some() {
            self.conn
                .prepare_cached(
                    "DELETE FROM analytics_open WHERE session_id = ?1 AND kind = ?2 AND id = ?3",
                )?
                .execute(params![self.session, kind, id])?;
        }
        Ok(row)
    }

    /// The milliseconds from an open row to this event (never negative).
    fn since(&self, started_at: i64) -> u64 {
        u64::try_from(self.at.saturating_sub(started_at)).unwrap_or(0)
    }

    /// The session's process ended: its open rows go, its open interval
    /// closes (design section 3.6 and 3.6a).
    ///
    /// `session.ended` and `exited`: the station saw the end, so the interval
    /// closes at the event. `lost`: the station did not see the end (the
    /// station restart recovery writes `lost` at the restart, after an outage
    /// of unknown length). The interval closes at the last evidence of the
    /// process: the latest earlier event of the session with a time before
    /// this one (the other recovery events have the same time), never before
    /// the start of the interval. The outage is not a proven run: a record
    /// made in it (a terminal continuation of the conversation) has no
    /// session (build answer A11).
    fn end_session(&self, lost: bool) -> Result<()> {
        self.conn.execute(
            "DELETE FROM analytics_open WHERE session_id = ?1",
            [self.session],
        )?;
        if lost {
            self.conn.execute(
                "UPDATE analytics_exec
                 SET ended_at = MAX(started_at, COALESCE(
                   (SELECT max(at) FROM events
                    WHERE session_id = ?1 AND station_seq < ?3 AND at < ?2),
                   started_at))
                 WHERE session_id = ?1 AND ended_at IS NULL",
                params![self.session, self.at, self.seq],
            )?;
        } else {
            self.conn.execute(
                "UPDATE analytics_exec SET ended_at = ?2 WHERE session_id = ?1 AND ended_at IS NULL",
                params![self.session, self.at],
            )?;
        }
        Ok(())
    }

    /// A `session.started`: closes the open interval of the session and of
    /// every other holder of its conversation, then opens a new one.
    fn start_run(&self) -> Result<()> {
        self.conn.execute(
            "UPDATE analytics_exec SET ended_at = ?2
             WHERE ended_at IS NULL AND session_id IN (
               SELECT ?1
               UNION
               SELECT o.session_id FROM sessions me
                 JOIN sessions o ON o.provider = me.provider
                  AND o.native_session_id = me.native_session_id
               WHERE me.session_id = ?1 AND me.native_session_id IS NOT NULL)",
            params![self.session, self.at],
        )?;
        self.conn.execute(
            "INSERT INTO analytics_exec (session_id, started_at, ended_at) VALUES (?1, ?2, NULL)
             ON CONFLICT (session_id, started_at) DO UPDATE SET ended_at = NULL",
            params![self.session, self.at],
        )?;
        Ok(())
    }
}

/// Sets `events_since` at the first event that the cursor counts.
fn note_since(conn: &Connection, at: i64) -> Result<()> {
    conn.prepare_cached(
        "UPDATE analytics_meta SET events_since = ?1 WHERE id = 1 AND events_since IS NULL",
    )?
    .execute([at])?;
    Ok(())
}

/// Counts one event (design section 3.6). Called in `station_seq` order only.
fn observe_one(conn: &Connection, envelope: &EventEnvelope) -> Result<()> {
    note_since(conn, envelope.at)?;
    if !counts_kind(envelope.event.kind_name()) {
        return Ok(());
    }
    let EventBody::Known(event) = &envelope.event else {
        return Ok(());
    };
    let c = Counter {
        conn,
        hour: envelope.at - envelope.at.rem_euclid(HOUR_MS),
        session: envelope.session_id.as_str(),
        at: envelope.at,
        seq: to_i64(envelope.station_seq)?,
    };
    match event {
        Event::SessionStarted { resumed, .. } => {
            let key = if *resumed == Some(true) {
                metric::RESUMES
            } else {
                metric::STARTS
            };
            c.add(key, 1)?;
            c.start_run()
        }
        Event::RunStarted { .. } => c.add(metric::RUNS, 1),
        Event::TurnStarted { turn_id, .. } => c.open("turn", turn_id.as_str(), None),
        Event::TurnCompleted {
            turn_id, status, ..
        } => {
            c.add(
                match status {
                    TurnStatus::Done => metric::TURNS_DONE,
                    TurnStatus::Interrupted => metric::TURNS_INTERRUPTED,
                    TurnStatus::Failed => metric::TURNS_FAILED,
                },
                1,
            )?;
            if let Some((started, _)) = c.take("turn", turn_id.as_str())? {
                c.add(metric::TURN_MS, c.since(started))?;
                c.add(metric::TURNS_TIMED, 1)?;
            }
            Ok(())
        }
        Event::ToolStarted {
            node_id, tool_kind, ..
        } => {
            let kind = name(tool_kind);
            c.add(&format!("{}.{kind}", metric::TOOLS), 1)?;
            c.open("tool", node_id.as_str(), Some(&kind))
        }
        Event::ToolCompleted {
            node_id, status, ..
        } => {
            let open = c.take("tool", node_id.as_str())?;
            let prefix = match status {
                ToolStatus::Ok => return Ok(()),
                ToolStatus::Failed => "failed",
                ToolStatus::Denied => "denied",
            };
            let kind = open
                .and_then(|(_, d)| d)
                .unwrap_or_else(|| "other".to_owned());
            c.add(&format!("{}.{prefix}.{kind}", metric::TOOLS), 1)
        }
        Event::ApprovalRequested { request_id, .. } => {
            c.add(metric::APPROVALS_ASKED, 1)?;
            c.open("approval", request_id.as_str(), None)
        }
        Event::ApprovalResolved { request_id, by, .. } => {
            c.add(&format!("{}.{}", metric::APPROVALS_ANSWERED, name(by)), 1)?;
            if let Some((started, _)) = c.take("approval", request_id.as_str())? {
                c.add(metric::APPROVAL_WAIT_MS, c.since(started))?;
                c.add(metric::APPROVALS_WAITED, 1)?;
            }
            Ok(())
        }
        Event::ApprovalExpired { request_id, .. } => {
            c.add(metric::APPROVALS_EXPIRED, 1)?;
            c.take("approval", request_id.as_str())?;
            Ok(())
        }
        Event::Error { code, .. } => {
            c.add(metric::ERRORS, 1)?;
            let part = if code_ok(code) {
                code.as_str()
            } else {
                "other"
            };
            c.add(&format!("{}.{part}", metric::ERRORS), 1)
        }
        Event::TurnFinalized {
            diff_summary: Some(diff),
            ..
        } => {
            c.add(metric::FILES_CHANGED, u64::from(diff.files_changed))?;
            c.add(metric::LINES_ADDED, diff.insertions)?;
            c.add(metric::LINES_REMOVED, diff.deletions)
        }
        Event::ArtifactPublished { .. } => c.add(metric::ARTIFACTS, 1),
        Event::WorkflowUpdated(update) => {
            let inserted = conn.execute(
                "INSERT OR IGNORE INTO analytics_workflows (session_id, task_id) VALUES (?1, ?2)",
                params![c.session, update.task_id],
            )?;
            if inserted == 1 {
                c.add(metric::WORKFLOWS, 1)?;
            }
            Ok(())
        }
        Event::RunCompleted { .. } => {
            conn.execute(
                "DELETE FROM analytics_open WHERE session_id = ?1 AND kind = 'tool'",
                [c.session],
            )?;
            Ok(())
        }
        Event::SessionEnded { .. }
        | Event::SessionStateChanged {
            state: SessionState::Exited,
            ..
        } => c.end_session(false),
        Event::SessionStateChanged {
            state: SessionState::Lost,
            ..
        } => c.end_session(true),
        _ => Ok(()),
    }
}

/// Counts the event of an append when the cursor is caught up (its
/// `station_seq` is `next_seq`), in the event transaction. When the cursor
/// is behind, it does nothing: [`catch_up_step`] reaches the event later.
pub(super) fn observe(conn: &Connection, envelope: &EventEnvelope) -> Result<()> {
    let advanced = conn
        .prepare_cached(
            "UPDATE analytics_meta SET next_seq = next_seq + 1 WHERE id = 1 AND next_seq = ?1",
        )?
        .execute([to_i64(envelope.station_seq)?])?;
    if advanced == 1 {
        observe_one(conn, envelope)?;
    }
    Ok(())
}

/// Counts the events with `station_seq >= next_seq`, in order, at most
/// `limit`, in one transaction, and moves `next_seq` past the last one.
/// Returns `true` while events are left.
///
/// # Errors
/// A database error.
pub fn catch_up_step(conn: &mut Connection, limit: u32) -> Result<bool> {
    let tx = conn.transaction()?;
    let next = to_i64(super::analytics::next_seq(&tx)?)?;
    let rows = {
        let mut stmt = tx.prepare(
            "SELECT station_seq, session_id, session_seq, kind, payload, at
             FROM events WHERE station_seq >= ?1 ORDER BY station_seq LIMIT ?2",
        )?;
        stmt.query_map(params![next, i64::from(limit)], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, i64>(5)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?
    };
    let Some(last) = rows.last().map(|r| r.0) else {
        return Ok(false);
    };
    for (seq, session, session_seq, kind, payload, at) in rows {
        if !counts_kind(&kind) {
            note_since(&tx, at)?;
            continue;
        }
        let Ok(event) = serde_json::from_str::<EventBody>(&payload) else {
            note_since(&tx, at)?;
            continue;
        };
        let envelope = EventEnvelope {
            station_seq: to_u64(seq)?,
            session_id: SessionId::from(session),
            session_seq: to_u64(session_seq)?,
            at,
            proto_version: PROTO_MAX,
            raw_ref: None,
            event,
        };
        observe_one(&tx, &envelope)?;
    }
    tx.execute(
        "UPDATE analytics_meta SET next_seq = ?1 WHERE id = 1",
        [last + 1],
    )?;
    let left: bool = tx.query_row(
        "SELECT EXISTS (SELECT 1 FROM events WHERE station_seq > ?1)",
        [last],
        |r| r.get(0),
    )?;
    tx.commit()?;
    Ok(left)
}

/// The start of the station (design section 3.6): closes the open interval
/// of every session whose state is `exited` or `lost`, at the time of that
/// session's last stored event (else at the start of the interval). Returns
/// the number of closed intervals.
///
/// # Errors
/// A database error.
pub fn close_ended_runs(conn: &Connection) -> Result<u64> {
    let n = conn.execute(
        "WITH last AS (SELECT session_id, max(at) AS at FROM events GROUP BY session_id)
         UPDATE analytics_exec
         SET ended_at = MAX(started_at, COALESCE((SELECT at FROM last
                                                  WHERE last.session_id = analytics_exec.session_id),
                                                 started_at))
         WHERE ended_at IS NULL
           AND session_id IN (SELECT session_id FROM sessions WHERE state IN ('exited', 'lost'))",
        [],
    )?;
    Ok(n as u64)
}

impl Store {
    /// One step of the event cursor (see [`catch_up_step`]).
    ///
    /// # Errors
    /// A database error.
    pub async fn analytics_catch_up_step(&self, limit: u32) -> Result<bool> {
        call(&self.inner.db, move |conn| catch_up_step(conn, limit)).await
    }

    /// Closes the intervals of the sessions that ended while the station was
    /// down (see [`close_ended_runs`]).
    ///
    /// # Errors
    /// A database error.
    pub async fn analytics_close_ended_runs(&self) -> Result<u64> {
        call(&self.inner.db, |conn| close_ended_runs(conn)).await
    }
}
