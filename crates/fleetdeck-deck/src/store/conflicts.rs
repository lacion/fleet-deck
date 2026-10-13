//! The conflict radar across stations (M3 6-7, DL-306).
//!
//! [`record_touches`] runs in the read-model transaction of `apply_events`
//! and keeps the latest touch of each file by each session (`file.changed`
//! and the `files` of `tool.completed`). [`DeckStore::conflicts`] reports
//! the files that two or more sessions touched inside the conflict window.
//!
//! The same transaction mails both sessions of a new conflict once (DL-145):
//! the mail id is stable for the pair, the file and the recipient, so the
//! stored mail is the dedup record and survives a restart.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};

use fleetdeck_core::ids::{IdSource, new_id};
use fleetdeck_proto::assist::fence_untrusted;
use fleetdeck_proto::command::{CommandOrigin, SessionCommand, UserInput};
use fleetdeck_proto::conflicts::{
    CONFLICT_WINDOW_MS, FileConflict, FileTouch, MAX_CONFLICTS, touch_key,
};
use fleetdeck_proto::event::{Event, EventBody, SessionState};
use fleetdeck_proto::orchestration::{MailDelivery, mail_frame};
use fleetdeck_proto::{EventEnvelope, SessionId, SessionSummary, StationId};
use ring::digest::{SHA256, digest};
use rusqlite::{OptionalExtension, Transaction, params};

use super::fleet::{NewMail, insert_mail};
use super::outbox::NewOutboxItem;
use super::{DeckStore, Result, call};
use crate::link::router::{CommandKind, origin_name};

/// The most files of one event that the radar records.
const MAX_FILES_PER_EVENT: usize = 64;
/// The longest path that the radar records.
const MAX_PATH_BYTES: usize = 1024;
/// How long the outbox tries to deliver a conflict mail; an old warning is
/// noise.
pub const CONFLICT_MAIL_TTL_MS: i64 = 2 * 60 * 60 * 1000;
/// The sender of a conflict mail.
const MAIL_SENDER: &str = "fleetdeck";
/// How long the delivery of a conflict mail waits for its generated
/// sentence (DL-167). The wait ends when the sentence is stored or when the
/// call failed; the hold only bounds a call that never answers.
pub const SENTENCE_HOLD_MS: i64 = 30_000;

/// A conflict mail that waits for a generated sentence (R10.6, DL-167).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SentenceJob {
    pub mail_id: String,
    /// The station of the recipient: it runs the model call.
    pub station: StationId,
    /// The model input: the file and what each session did, fenced.
    pub input: String,
}

/// What the conflict mail needs from the caller of [`record_touches`].
pub(super) struct MailContext<'a> {
    pub ids: &'a dyn IdSource,
    /// The deck time in Unix ms.
    pub now: i64,
    /// The stations whose model help is on: a mail to one of their sessions
    /// waits for a generated sentence (DL-167).
    pub assist: &'a BTreeSet<StationId>,
    /// The mails that wait for a sentence.
    pub jobs: &'a RefCell<Vec<SentenceJob>>,
}

/// One session of a conflict.
struct Party {
    station: StationId,
    session: SessionId,
}

impl Party {
    /// The identity of a session in a mail id.
    fn identity(&self) -> String {
        format!("{}/{}", self.station.as_str(), self.session.as_str())
    }
}

fn sha256_hex(text: &str) -> String {
    digest(&SHA256, text.as_bytes())
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The stable id of the mail to `recipient` about `file` (`a` and `b` are
/// the two identities in string order).
fn conflict_mail_id(group: &str, file: &str, a: &str, b: &str, recipient: &str) -> String {
    let hash = sha256_hex(&format!("{group}|{file}|{a}|{b}|{recipient}"));
    format!("conflict-{}", &hash[..24])
}

fn stored_summary(tx: &Transaction<'_>, party: &Party) -> Result<Option<SessionSummary>> {
    let text: Option<String> = tx
        .query_row(
            "SELECT summary FROM sessions WHERE station_id = ?1 AND session_id = ?2",
            params![party.station.as_str(), party.session.as_str()],
            |r| r.get(0),
        )
        .optional()?;
    Ok(text.map(|t| serde_json::from_str(&t)).transpose()?)
}

fn station_name(tx: &Transaction<'_>, station: &StationId) -> Result<String> {
    let name: Option<String> = tx
        .query_row(
            "SELECT name FROM stations WHERE station_id = ?1",
            [station.as_str()],
            |r| r.get(0),
        )
        .optional()?;
    Ok(name.unwrap_or_else(|| station.as_str().to_owned()))
}

/// Queues the mail from `other` to `recipient` unless it exists already.
fn mail_party(
    tx: &Transaction<'_>,
    mail: &MailContext<'_>,
    group: &str,
    file: &str,
    (a, b): (&str, &str),
    recipient: &Party,
    other: &Party,
) -> Result<()> {
    let mail_id = conflict_mail_id(group, file, a, b, &recipient.identity());
    let known = tx
        .query_row("SELECT 1 FROM mail WHERE mail_id = ?1", [&mail_id], |_| {
            Ok(())
        })
        .optional()?
        .is_some();
    if known {
        return Ok(());
    }
    let Some(summary) = stored_summary(tx, recipient)? else {
        return Ok(());
    };
    if matches!(summary.state, SessionState::Exited | SessionState::Lost) {
        return Ok(());
    }
    let other_summary = stored_summary(tx, other)?;
    let other_name = other_summary
        .as_ref()
        .and_then(|s| s.title.clone())
        .unwrap_or_else(|| other.session.as_str().to_owned());
    let other_station = station_name(tx, &other.station)?;
    let text = format!(
        "Fleet Deck: the session \"{other_name}\" on {other_station} also changed {file} in the \
         last 30 minutes. Coordinate before you change it further."
    );
    let command = SessionCommand::FollowUp {
        session_id: recipient.session.clone(),
        input: UserInput::new(mail_frame(MAIL_SENDER, &text)),
    };
    let item = NewOutboxItem {
        station_id: recipient.station.clone(),
        origin: origin_name(CommandOrigin::Mail).to_owned(),
        payload: serde_json::to_value(CommandKind::Session { command })?,
        expires_at: mail.now.saturating_add(CONFLICT_MAIL_TTL_MS),
    };
    let outbox_id: String = new_id(mail.ids);
    insert_mail(
        tx,
        &NewMail {
            mail_id: mail_id.clone(),
            sender: MAIL_SENDER.to_owned(),
            session_id: recipient.session.clone(),
            delivery: MailDelivery::FollowUp,
            text,
            item,
        },
        &outbox_id,
        mail.now,
    )?;
    if mail.assist.contains(&recipient.station) {
        // The delivery waits for the generated sentence (R10.6, DL-167);
        // the plain text goes out when the hold ends.
        tx.execute(
            "UPDATE outbox SET next_at = ?2 WHERE outbox_id = ?1",
            params![outbox_id, mail.now.saturating_add(SENTENCE_HOLD_MS)],
        )?;
        let me = (
            summary
                .title
                .as_deref()
                .unwrap_or(recipient.session.as_str()),
            station_name(tx, &recipient.station)?,
            summary.digest.as_deref(),
        );
        let them = (
            other_name.as_str(),
            other_station.clone(),
            other_summary.as_ref().and_then(|s| s.digest.as_deref()),
        );
        mail.jobs.borrow_mut().push(SentenceJob {
            mail_id,
            station: recipient.station.clone(),
            input: conflict_input(file, [me, them]),
        });
    }
    Ok(())
}

/// The model input of a conflict sentence: the file and, for each session,
/// its title and digest. Session text comes from agents, so it is fenced.
fn conflict_input(file: &str, sessions: [(&str, String, Option<&str>); 2]) -> String {
    let mut text = format!("File: {file}\n");
    for (n, (title, station, digest)) in sessions.into_iter().enumerate() {
        let what = digest.unwrap_or("no summary yet");
        text.push_str(&format!(
            "Session {}:\n{}\n",
            n + 1,
            fence_untrusted(&format!("\"{title}\" on {station}: {what}"))
        ));
    }
    text
}

/// Mails both sessions of each pair that touched `(group, file)` in the
/// window before `e` and have no conflict mail yet.
fn mail_conflicts(
    tx: &Transaction<'_>,
    mail: &MailContext<'_>,
    station: &StationId,
    e: &EventEnvelope,
    group: &str,
    file: &str,
) -> Result<()> {
    let mut stmt = tx.prepare(
        "SELECT station_id, session_id FROM file_touches
         WHERE group_key = ?1 AND file = ?2 AND touched_at >= ?3 AND touched_at <= ?6
             AND NOT (station_id = ?4 AND session_id = ?5)
         ORDER BY station_id, session_id",
    )?;
    let others: Vec<Party> = stmt
        .query_map(
            params![
                group,
                file,
                e.at.saturating_sub(CONFLICT_WINDOW_MS),
                station.as_str(),
                e.session_id.as_str(),
                // Both sides of the window: a replayed old touch is not a
                // conflict with a recent one (Codex review).
                e.at.saturating_add(CONFLICT_WINDOW_MS)
            ],
            |r| {
                Ok(Party {
                    station: StationId::from(r.get::<_, String>(0)?),
                    session: SessionId::from(r.get::<_, String>(1)?),
                })
            },
        )?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);
    let this = Party {
        station: station.clone(),
        session: e.session_id.clone(),
    };
    for other in &others {
        let (x, y) = (this.identity(), other.identity());
        let pair = if x <= y { (x, y) } else { (y, x) };
        let pair = (pair.0.as_str(), pair.1.as_str());
        mail_party(tx, mail, group, file, pair, &this, other)?;
        mail_party(tx, mail, group, file, pair, other, &this)?;
    }
    Ok(())
}

/// Records the files that an event changed; other events do nothing.
pub(super) fn record_touches(
    tx: &Transaction<'_>,
    station: &StationId,
    e: &EventEnvelope,
    summary: &SessionSummary,
    mail: Option<&MailContext<'_>>,
) -> Result<()> {
    let paths: Vec<&str> = match &e.event {
        EventBody::Known(Event::FileChanged { path, .. }) => vec![path.as_str()],
        EventBody::Known(Event::ToolCompleted { files, .. }) => files
            .iter()
            .take(MAX_FILES_PER_EVENT)
            .map(String::as_str)
            .collect(),
        _ => return Ok(()),
    };
    for path in paths {
        if path.is_empty() || path.len() > MAX_PATH_BYTES {
            continue;
        }
        let (group, repo, file) =
            touch_key(station, summary.cwd.as_deref(), summary.repo.as_ref(), path);
        tx.execute(
            "INSERT INTO file_touches (group_key, file, repo, station_id, session_id,
                 session_title, path, touched_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT (group_key, file, station_id, session_id) DO UPDATE SET
                 session_title = excluded.session_title, path = excluded.path,
                 touched_at = max(touched_at, excluded.touched_at)",
            params![
                group,
                file,
                repo,
                station.as_str(),
                e.session_id.as_str(),
                summary.title,
                path,
                e.at,
            ],
        )?;
        if let Some(mail) = mail {
            mail_conflicts(tx, mail, station, e, &group, &file)?;
        }
    }
    // Touches far outside the window are never read again.
    tx.execute(
        "DELETE FROM file_touches WHERE touched_at < ?1",
        [e.at.saturating_sub(2 * CONFLICT_WINDOW_MS)],
    )?;
    Ok(())
}

impl DeckStore {
    /// The conflict mails that wait for a generated sentence, once each
    /// (R10.6, DL-167). The link takes them after a fold and asks the
    /// station; [`Self::finish_conflict_sentence`] ends each wait.
    pub fn take_conflict_jobs(&self) -> Vec<SentenceJob> {
        std::mem::take(
            &mut *self
                .inner
                .sentence_jobs
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        )
    }

    /// Ends the wait of one conflict mail: with `Some(sentence)` the
    /// sentence goes after the plain text, in the mail and in its queued
    /// delivery; either way the delivery is due now. A mail that was sent
    /// already (the hold ended first) is not changed.
    ///
    /// # Errors
    /// A database or JSON error.
    pub async fn finish_conflict_sentence(
        &self,
        mail_id: String,
        sentence: Option<String>,
    ) -> Result<()> {
        let now = self.now_ms();
        call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            let row: Option<(String, String, String, String)> = tx
                .query_row(
                    "SELECT m.text, m.outbox_id, o.payload, o.state FROM mail m
                     JOIN outbox o ON o.outbox_id = m.outbox_id WHERE m.mail_id = ?1",
                    [&mail_id],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                )
                .optional()?;
            let Some((text, outbox_id, payload, state)) = row else {
                return Ok(());
            };
            if state != "queued" {
                return Ok(());
            }
            let mut payload: serde_json::Value = serde_json::from_str(&payload)?;
            if let Some(sentence) = sentence {
                let text = format!("{text} {sentence}");
                let mut kind: CommandKind = serde_json::from_value(payload)?;
                if let CommandKind::Session {
                    command: SessionCommand::FollowUp { input, .. },
                } = &mut kind
                {
                    *input = UserInput::new(mail_frame(MAIL_SENDER, &text));
                }
                payload = serde_json::to_value(&kind)?;
                tx.execute(
                    "UPDATE mail SET text = ?2 WHERE mail_id = ?1",
                    params![mail_id, text],
                )?;
            }
            tx.execute(
                "UPDATE outbox SET payload = ?2, next_at = min(next_at, ?3), updated_at = ?3
                 WHERE outbox_id = ?1 AND state = 'queued'",
                params![outbox_id, serde_json::to_string(&payload)?, now],
            )?;
            tx.commit()?;
            Ok(())
        })
        .await
    }

    /// The files that two or more sessions touched in the last
    /// [`CONFLICT_WINDOW_MS`], the newest conflict first.
    ///
    /// # Errors
    /// A database error.
    pub async fn conflicts(&self) -> Result<Vec<FileConflict>> {
        let since = self.now_ms().saturating_sub(CONFLICT_WINDOW_MS);
        call(&self.inner.db, move |conn| {
            let mut stmt = conn.prepare(
                "SELECT group_key, file, repo, station_id, session_id, session_title, path,
                     touched_at
                 FROM file_touches WHERE touched_at >= ?1
                 ORDER BY touched_at DESC, station_id, session_id",
            )?;
            let mut groups: BTreeMap<(String, String), FileConflict> = BTreeMap::new();
            let rows = stmt.query_map([since], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Option<String>>(2)?,
                    FileTouch {
                        station_id: StationId::from(r.get::<_, String>(3)?),
                        session_id: r.get::<_, String>(4)?.into(),
                        session_title: r.get(5)?,
                        path: r.get(6)?,
                        touched_at: r.get(7)?,
                    },
                ))
            })?;
            for row in rows {
                let (group, file, repo, touch) = row?;
                groups
                    .entry((group, file.clone()))
                    .or_insert_with(|| FileConflict {
                        repo,
                        file,
                        touches: Vec::new(),
                    })
                    .touches
                    .push(touch);
            }
            let mut out: Vec<FileConflict> = groups
                .into_values()
                .filter(|c| c.touches.len() >= 2)
                .collect();
            out.sort_by_key(|c| std::cmp::Reverse(c.touches.first().map_or(0, |t| t.touched_at)));
            out.truncate(MAX_CONFLICTS);
            Ok(out)
        })
        .await
    }
}
