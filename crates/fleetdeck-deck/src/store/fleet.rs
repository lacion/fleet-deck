//! Fleet mail (M3 6-2) and tasks with waitable completions (6-4): deck-owned
//! intent (A1, A5). A mail or a task row and the outbox item that delivers
//! its text are inserted in **one transaction**; the outbox then delivers
//! the text once, as a native follow-up or steer (DL-205, DL-206). The
//! delivery state of a row is the state of its outbox item.
//!
//! A task stays `open` until a completion (`done` or `failed`, with an
//! optional result) arrives from the board or from the coordinator API
//! (6-5). A completion of an ended task with the same state and result is
//! a duplicate and changes nothing; another one is refused.

use std::collections::BTreeMap;

use fleetdeck_core::ids::new_id;
use fleetdeck_proto::orchestration::{DeliveryState, MailDelivery, MailView, TaskState, TaskView};
use fleetdeck_proto::{SessionId, StationId};
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
    }
}

fn parse_task_state(s: &str) -> rusqlite::Result<TaskState> {
    match s {
        "open" => Ok(TaskState::Open),
        "done" => Ok(TaskState::Done),
        "failed" => Ok(TaskState::Failed),
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
     t.state, t.result, o.state, o.reason, t.created_at, t.updated_at, t.completed_at
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
        delivery: parse_outbox_state(&row.get::<_, String>(7)?)?,
        reason: row.get(8)?,
        created_at: row.get(9)?,
        updated_at: row.get(10)?,
        completed_at: row.get(11)?,
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
            if let Some(stored) = get_mail(&tx, &mail.mail_id)? {
                return Ok((stored, false));
            }
            insert_item(&tx, &outbox_id, &mail.item, now)?;
            tx.execute(
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
            let stored = get_mail(&tx, &mail.mail_id)?
                .ok_or_else(|| DeckError::Corrupt("an inserted mail is gone".to_owned()))?;
            tx.commit()?;
            Ok((stored, true))
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

    /// Ends an open task with `state` (`done` or `failed`) and `result`;
    /// `None` for an unknown task.
    ///
    /// # Errors
    /// [`DeckError::Corrupt`] for `state` `open`, or a database error.
    pub async fn complete_task(
        &self,
        task_id: String,
        state: TaskState,
        result: Option<String>,
    ) -> Result<Option<Completed>> {
        if state == TaskState::Open {
            return Err(DeckError::Corrupt(
                "a completion is done or failed".to_owned(),
            ));
        }
        let now = self.now_ms();
        call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            let Some(task) = get_task(&tx, &task_id)? else {
                return Ok(None);
            };
            if task.state != TaskState::Open {
                let same = task.state == state && task.result == result;
                return Ok(Some(if same {
                    Completed::Ended(task)
                } else {
                    Completed::Conflict(task)
                }));
            }
            tx.execute(
                "UPDATE tasks SET state = ?2, result = ?3, updated_at = ?4, completed_at = ?4
                 WHERE task_id = ?1 AND state = 'open'",
                params![task_id, task_state_name(state), result, now],
            )?;
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
