//! Fleet mail (M3 6-2) and tasks with waitable completions (6-4): deck-owned
//! intent (A1, A5). A mail or a task row and the outbox item that delivers
//! its text are inserted in **one transaction**; the outbox then delivers
//! the text once, as a native follow-up or steer (DL-205, DL-206). The
//! delivery state of a row is the state of its outbox item.
//!
//! A task stays `open` until a completion arrives from the board or from
//! the coordinator API (6-5): `done` and `failed` (with an optional
//! result), `blocked` (with a reason) or `question` (with a reason and a
//! question, R2.1). `question` does not end the task: the worker can
//! complete it again. A completion of an ended task with the same content
//! is a duplicate and changes nothing; another one is refused.
//!
//! A task whose session ends (`exited`, `lost`, or removed from the read
//! model) without a final completion ends as `blocked` with the reason
//! (R2.2): [`block_open_tasks`].

use std::collections::{BTreeMap, BTreeSet};

use fleetdeck_core::ids::new_id;
use fleetdeck_proto::board::DeckSession;
use fleetdeck_proto::event::ProviderKind;
use fleetdeck_proto::orchestration::{
    DeliveryState, MailDelivery, MailGroup, MailView, TaskCompletion, TaskState, TaskView,
};
use fleetdeck_proto::{CardStatus, SessionId, StationId};
use rusqlite::{Connection, OptionalExtension, Row, params};

use super::outbox::{NewOutboxItem, insert_item};
use super::{DeckError, DeckStore, Result, call};

/// A mail to store with its outbox item.
#[derive(Debug, Clone)]
pub struct NewMail {
    pub mail_id: String,
    pub sender: String,
    pub session_id: SessionId,
    pub delivery: MailDelivery,
    pub text: String,
    /// The delivery intent (the station is `item.station_id`).
    pub item: NewOutboxItem,
}

/// A task to store with its outbox item.
#[derive(Debug, Clone)]
pub struct NewTask {
    pub task_id: String,
    pub title: String,
    pub creator: String,
    pub session_id: SessionId,
    pub item: NewOutboxItem,
}

/// The answer of a completion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Completed {
    /// The task ended now, or the same completion arrived again.
    Ended(TaskView),
    /// The task ended before with another state or result.
    Conflict(TaskView),
}

fn delivery_name(d: MailDelivery) -> &'static str {
    match d {
        MailDelivery::FollowUp => "follow_up",
        MailDelivery::Steer => "steer",
    }
}

fn parse_delivery(s: &str) -> rusqlite::Result<MailDelivery> {
    match s {
        "follow_up" => Ok(MailDelivery::FollowUp),
        "steer" => Ok(MailDelivery::Steer),
        other => Err(corrupt(format!("mail delivery {other}"))),
    }
}

fn task_state_name(s: TaskState) -> &'static str {
    match s {
        TaskState::Open => "open",
        TaskState::Done => "done",
        TaskState::Failed => "failed",
        TaskState::Blocked => "blocked",
        TaskState::Question => "question",
    }
}

fn parse_task_state(s: &str) -> rusqlite::Result<TaskState> {
    match s {
        "open" => Ok(TaskState::Open),
        "done" => Ok(TaskState::Done),
        "failed" => Ok(TaskState::Failed),
        "blocked" => Ok(TaskState::Blocked),
        "question" => Ok(TaskState::Question),
        other => Err(corrupt(format!("task state {other}"))),
    }
}

fn parse_outbox_state(s: &str) -> rusqlite::Result<DeliveryState> {
    match s {
        "queued" => Ok(DeliveryState::Queued),
        "sending" => Ok(DeliveryState::Sending),
        "delivered" => Ok(DeliveryState::Delivered),
        "dead" => Ok(DeliveryState::Dead),
        other => Err(corrupt(format!("outbox state {other}"))),
    }
}

fn corrupt(message: String) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        0,
        rusqlite::types::Type::Text,
        Box::new(DeckError::Corrupt(message)),
    )
}

const MAIL_SELECT: &str = "SELECT m.mail_id, m.sender, m.station_id, m.session_id, m.delivery,
     m.text, o.state, o.reason, m.created_at
     FROM mail m JOIN outbox o ON o.outbox_id = m.outbox_id";

fn mail_row(row: &Row<'_>) -> rusqlite::Result<MailView> {
    Ok(MailView {
        mail_id: row.get(0)?,
        sender: row.get(1)?,
        station_id: StationId(row.get(2)?),
        session_id: SessionId(row.get(3)?),
        delivery: parse_delivery(&row.get::<_, String>(4)?)?,
        text: row.get(5)?,
        state: parse_outbox_state(&row.get::<_, String>(6)?)?,
        reason: row.get(7)?,
        created_at: row.get(8)?,
    })
}

const TASK_SELECT: &str = "SELECT t.task_id, t.title, t.creator, t.station_id, t.session_id,
     t.state, t.result, t.completion_reason, t.question, o.state, o.reason, t.created_at,
     t.updated_at, t.completed_at
     FROM tasks t JOIN outbox o ON o.outbox_id = t.outbox_id";

fn task_row(row: &Row<'_>) -> rusqlite::Result<TaskView> {
    Ok(TaskView {
        task_id: row.get(0)?,
        title: row.get(1)?,
        creator: row.get(2)?,
        station_id: StationId(row.get(3)?),
        session_id: SessionId(row.get(4)?),
        state: parse_task_state(&row.get::<_, String>(5)?)?,
        result: row.get(6)?,
        completion_reason: row.get(7)?,
        question: row.get(8)?,
        delivery: parse_outbox_state(&row.get::<_, String>(9)?)?,
        reason: row.get(10)?,
        created_at: row.get(11)?,
        updated_at: row.get(12)?,
        completed_at: row.get(13)?,
    })
}

fn get_mail(conn: &Connection, id: &str) -> Result<Option<MailView>> {
    Ok(conn
        .query_row(
            &format!("{MAIL_SELECT} WHERE m.mail_id = ?1"),
            [id],
            mail_row,
        )
        .optional()?)
}

fn get_task(conn: &Connection, id: &str) -> Result<Option<TaskView>> {
    Ok(conn
        .query_row(
            &format!("{TASK_SELECT} WHERE t.task_id = ?1"),
            [id],
            task_row,
        )
        .optional()?)
}

/// Stores a mail and its outbox item in the caller's transaction. A known
/// `mail_id` returns the stored mail and inserts nothing (`false`).
pub(super) fn insert_mail(
    conn: &Connection,
    mail: &NewMail,
    outbox_id: &str,
    now: i64,
) -> Result<(MailView, bool)> {
    if let Some(stored) = get_mail(conn, &mail.mail_id)? {
        return Ok((stored, false));
    }
    insert_item(conn, outbox_id, &mail.item, now)?;
    conn.execute(
        "INSERT INTO mail (mail_id, sender, station_id, session_id, delivery, text,
             outbox_id, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            mail.mail_id,
            mail.sender,
            mail.item.station_id.as_str(),
            mail.session_id.0,
            delivery_name(mail.delivery),
            mail.text,
            outbox_id,
            now
        ],
    )?;
    let stored = get_mail(conn, &mail.mail_id)?
        .ok_or_else(|| DeckError::Corrupt("an inserted mail is gone".to_owned()))?;
    Ok((stored, true))
}

/// The live sessions that a group address names (R2.5): `@idle` the idle
/// ones, `@claude` and `@codex` the sessions of that provider. A session
/// that ended, was lost or sits behind a down link is not live.
#[must_use]
pub fn group_members(sessions: &[DeckSession], group: MailGroup) -> Vec<&DeckSession> {
    sessions
        .iter()
        .filter(|s| {
            !matches!(
                s.status,
                CardStatus::Ended | CardStatus::Lost | CardStatus::Unreachable
            )
        })
        .filter(|s| match group {
            MailGroup::Idle => s.status == CardStatus::Idle,
            MailGroup::Claude => s.summary.provider == Some(ProviderKind::Claude),
            MailGroup::Codex => s.summary.provider == Some(ProviderKind::Codex),
        })
        .collect()
}

/// The id of the `n`-th mail of a group send (`n` from 1).
#[must_use]
pub fn group_mail_id(mail_id: &str, n: usize) -> String {
    format!("{mail_id}-{n}")
}

/// Writes a completion on a task that is `open` or in `question`, in the
/// caller's transaction. The caller has checked the completion
/// ([`TaskCompletion::check`]) and the task.
pub(super) fn write_completion(
    tx: &Connection,
    task_id: &str,
    completion: &TaskCompletion,
    now: i64,
) -> Result<()> {
    let ended = (completion.state != TaskState::Question).then_some(now);
    tx.execute(
        "UPDATE tasks SET state = ?2, result = ?3, completion_reason = ?4, question = ?5,
             updated_at = ?6, completed_at = ?7
         WHERE task_id = ?1 AND state IN ('open', 'question')",
        params![
            task_id,
            task_state_name(completion.state),
            completion.result,
            completion.reason,
            completion.question,
            now,
            ended
        ],
    )?;
    Ok(())
}

/// Ends the open tasks of one session as `blocked` (R2.2): `cause` is the
/// session state (`exited`, `lost`) or `removed`. Only a task without a
/// final completion changes, so a repeat changes nothing. Returns the
/// number of tasks that ended.
pub(super) fn block_open_tasks(
    tx: &Connection,
    station: &StationId,
    session: &str,
    cause: &str,
    now: i64,
) -> Result<usize> {
    Ok(tx.execute(
        "UPDATE tasks SET state = 'blocked', completion_reason = ?4, updated_at = ?5,
             completed_at = ?5
         WHERE station_id = ?1 AND session_id = ?2 AND state IN ('open', 'question')",
        params![
            station.as_str(),
            session,
            format!("the worker session ended without a completion ({cause})"),
            now
        ],
    )?)
}

/// [`block_open_tasks`] for every session of `station` with an open task
/// that is not in `named` (the sessions that the read model still holds).
pub(super) fn block_unlisted_tasks(
    tx: &Connection,
    station: &StationId,
    named: &BTreeSet<String>,
    now: i64,
) -> Result<()> {
    let mut stmt = tx.prepare(
        "SELECT DISTINCT session_id FROM tasks
         WHERE station_id = ?1 AND state IN ('open', 'question')",
    )?;
    let open: Vec<String> = stmt
        .query_map([station.as_str()], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);
    for session in open.iter().filter(|s| !named.contains(*s)) {
        block_open_tasks(tx, station, session, "removed", now)?;
    }
    Ok(())
}

impl DeckStore {
    /// Stores a mail and queues its delivery in one transaction. A known
    /// `mail_id` returns the stored mail and queues nothing (`false`).
    ///
    /// # Errors
    /// [`DeckError::UnknownStation`], or a database error.
    pub async fn send_mail(&self, mail: NewMail) -> Result<(MailView, bool)> {
        let now = self.now_ms();
        let outbox_id: String = new_id(self.inner.ids.as_ref());
        call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            let stored = insert_mail(&tx, &mail, &outbox_id, now)?;
            tx.commit()?;
            Ok(stored)
        })
        .await
    }

    /// Stores the mails of one group send (R2.5), each with its outbox item,
    /// in one transaction. `mails` hold the ids `<prefix>-<n>`. When mails
    /// of `prefix` exist already, they are returned and nothing is stored
    /// (`false`), so a resend does not reach a changed group.
    ///
    /// # Errors
    /// [`DeckError::UnknownStation`], or a database error.
    pub async fn send_group_mail(
        &self,
        prefix: String,
        mails: Vec<NewMail>,
    ) -> Result<(Vec<MailView>, bool)> {
        let now = self.now_ms();
        let outbox_ids: Vec<String> = mails
            .iter()
            .map(|_| new_id(self.inner.ids.as_ref()))
            .collect();
        call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            let mut stored = {
                let mut stmt = tx.prepare(&format!("{MAIL_SELECT} WHERE m.mail_id GLOB ?1"))?;
                let rows = stmt.query_map([format!("{prefix}-[0-9]*")], mail_row)?;
                let all = rows.collect::<rusqlite::Result<Vec<_>>>()?;
                all.into_iter()
                    .filter_map(|m| {
                        let n = m
                            .mail_id
                            .strip_prefix(&format!("{prefix}-"))?
                            .parse::<usize>();
                        Some((n.ok()?, m))
                    })
                    .collect::<Vec<_>>()
            };
            if !stored.is_empty() {
                stored.sort_by_key(|(n, _)| *n);
                return Ok((stored.into_iter().map(|(_, m)| m).collect(), false));
            }
            let mut out = Vec::with_capacity(mails.len());
            for (mail, outbox_id) in mails.iter().zip(&outbox_ids) {
                out.push(insert_mail(&tx, mail, outbox_id, now)?.0);
            }
            tx.commit()?;
            Ok((out, true))
        })
        .await
    }

    /// Every mail, newest first (at most `limit`).
    ///
    /// # Errors
    /// A database error.
    pub async fn mails(&self, limit: u32) -> Result<Vec<MailView>> {
        call(&self.inner.db, move |conn| {
            let mut stmt = conn.prepare(&format!(
                "{MAIL_SELECT} ORDER BY m.created_at DESC, m.mail_id DESC LIMIT ?1"
            ))?;
            let rows = stmt.query_map([limit], mail_row)?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })
        .await
    }

    /// The number of mails that are not delivered yet, per session (the
    /// "least buried" term of `assign auto`, DL-204).
    ///
    /// # Errors
    /// A database error.
    pub async fn undelivered_mail(&self) -> Result<BTreeMap<SessionId, u32>> {
        call(&self.inner.db, move |conn| {
            let mut stmt = conn.prepare(
                "SELECT m.session_id, COUNT(*) FROM mail m
                 JOIN outbox o ON o.outbox_id = m.outbox_id
                 WHERE o.state IN ('queued', 'sending') GROUP BY m.session_id",
            )?;
            let rows = stmt.query_map([], |row| {
                let n: i64 = row.get(1)?;
                Ok((SessionId(row.get(0)?), u32::try_from(n).unwrap_or(u32::MAX)))
            })?;
            Ok(rows.collect::<rusqlite::Result<BTreeMap<_, _>>>()?)
        })
        .await
    }

    /// Stores a task and queues its delivery in one transaction. A known
    /// `task_id` returns the stored task and queues nothing (`false`).
    ///
    /// # Errors
    /// [`DeckError::UnknownStation`], or a database error.
    pub async fn create_task(&self, task: NewTask) -> Result<(TaskView, bool)> {
        let now = self.now_ms();
        let outbox_id: String = new_id(self.inner.ids.as_ref());
        call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            if let Some(stored) = get_task(&tx, &task.task_id)? {
                return Ok((stored, false));
            }
            insert_item(&tx, &outbox_id, &task.item, now)?;
            tx.execute(
                "INSERT INTO tasks (task_id, title, creator, station_id, session_id, state,
                     outbox_id, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, 'open', ?6, ?7, ?7)",
                params![
                    task.task_id,
                    task.title,
                    task.creator,
                    task.item.station_id.as_str(),
                    task.session_id.0,
                    outbox_id,
                    now
                ],
            )?;
            let stored = get_task(&tx, &task.task_id)?
                .ok_or_else(|| DeckError::Corrupt("an inserted task is gone".to_owned()))?;
            tx.commit()?;
            Ok((stored, true))
        })
        .await
    }

    /// One task.
    ///
    /// # Errors
    /// A database error.
    pub async fn task(&self, task_id: String) -> Result<Option<TaskView>> {
        call(&self.inner.db, move |conn| get_task(conn, &task_id)).await
    }

    /// Every task, newest first (at most `limit`).
    ///
    /// # Errors
    /// A database error.
    pub async fn tasks(&self, limit: u32) -> Result<Vec<TaskView>> {
        call(&self.inner.db, move |conn| {
            let mut stmt = conn.prepare(&format!(
                "{TASK_SELECT} ORDER BY t.created_at DESC, t.task_id DESC LIMIT ?1"
            ))?;
            let rows = stmt.query_map([limit], task_row)?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })
        .await
    }

    /// The tasks of `ids`, in the order of `ids`. `Ok(Err(id))` names an id
    /// that is no task (R2.4).
    ///
    /// # Errors
    /// A database error.
    pub async fn tasks_by_ids(
        &self,
        ids: Vec<String>,
    ) -> Result<std::result::Result<Vec<TaskView>, String>> {
        call(&self.inner.db, move |conn| {
            let mut out = Vec::with_capacity(ids.len());
            for id in ids {
                match get_task(conn, &id)? {
                    Some(task) => out.push(task),
                    None => return Ok(Err(id)),
                }
            }
            Ok(Ok(out))
        })
        .await
    }

    /// Completes an open task (or answers a `question` with a new
    /// completion); `None` for an unknown task. The same completion again
    /// is a duplicate; any completion of an ended task is a conflict.
    ///
    /// # Errors
    /// [`DeckError::Invalid`] for a completion that breaks
    /// [`TaskCompletion::check`], or a database error.
    pub async fn complete_task(
        &self,
        task_id: String,
        completion: TaskCompletion,
    ) -> Result<Option<Completed>> {
        completion.check().map_err(DeckError::Invalid)?;
        let now = self.now_ms();
        call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            let Some(task) = get_task(&tx, &task_id)? else {
                return Ok(None);
            };
            let same = task.state == completion.state
                && task.result == completion.result
                && task.completion_reason == completion.reason
                && task.question == completion.question;
            if same {
                return Ok(Some(Completed::Ended(task)));
            }
            if !task.state.is_open() {
                return Ok(Some(Completed::Conflict(task)));
            }
            write_completion(&tx, &task_id, &completion, now)?;
            let ended = get_task(&tx, &task_id)?
                .ok_or_else(|| DeckError::Corrupt("a completed task is gone".to_owned()))?;
            tx.commit()?;
            Ok(Some(Completed::Ended(ended)))
        })
        .await
    }
}

#[cfg(test)]
#[path = "fleet_tests.rs"]
mod tests;
