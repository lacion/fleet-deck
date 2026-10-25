//! Send a review and send it again (DL-179, design 3). Each is one deck
//! transaction: the replay check, the caps, the draft checks, the follow-up
//! text, the stored command and the review row commit together or not at
//! all. The route dispatches the command after the commit.
//!
//! A sent comment stays sent: the delivery of the review follows its command
//! (`review::command_ended`). Only a `failed` delivery is sent again.

use std::collections::BTreeSet;

use fleetdeck_proto::member_deny::MemberDeny;
use fleetdeck_proto::review::{
    AnchorKind, CommentState, Delivery, MAX_ATTEMPTS, MAX_COMMENTS_PER_REVIEW,
    MAX_REVIEWS_PER_SESSION, ReviewComment, ReviewSummary,
};
use fleetdeck_proto::{CommandId, StationId};
use ring::digest::{SHA256, digest};
use rusqlite::{Connection, OptionalExtension, params};

use super::commands::{self, NewCommand};
use super::review::{
    COLUMNS, Hide, Outcome, REVIEW_COLUMNS, Viewer, alloc_short, bump, comment, find, flat,
    random_short, refuse, summary, visible,
};
use super::review_text::{self, Sender, TextComment};
use super::{DeckError, DeckStore, Result, call, new_id};

/// The member deny list at send time: `None` when it cannot be established.
/// The follow-up text then carries no excerpt at all (design 2.3).
pub type Excerpts = Option<MemberDeny>;

/// A review send.
#[derive(Debug, Clone)]
pub struct SendRequest {
    pub station: StationId,
    pub session: String,
    /// The sender: the owner or a member.
    pub viewer: Viewer,
    pub send_key: String,
    /// 1 to 50 distinct comment ids (checked by the route).
    pub comment_ids: Vec<String>,
    pub excerpts: Excerpts,
    /// The follow-up command with an empty text; the transaction sets the
    /// text.
    pub command: NewCommand,
    /// A live link of the station exists.
    pub link_up: bool,
}

/// A "Send again".
#[derive(Debug, Clone)]
pub struct AgainRequest {
    pub station: StationId,
    pub session: String,
    pub viewer: Viewer,
    pub review_id: String,
    pub attempt_key: String,
    pub excerpts: Excerpts,
    pub command: NewCommand,
    pub link_up: bool,
}

/// What a send did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sent {
    pub summary: ReviewSummary,
    /// A new command was stored: dispatch it after the commit. `false`: the
    /// stored result of an earlier call returns and nothing was stored.
    pub created: bool,
}

/// Command ids whose send fails after the command insert (tests of the
/// atomicity of the send transaction).
#[cfg(test)]
pub(super) static FAIL_AFTER_INSERT: std::sync::Mutex<Vec<String>> =
    std::sync::Mutex::new(Vec::new());

#[cfg(test)]
fn fail_after_insert(id: &CommandId) -> Result<()> {
    let list = FAIL_AFTER_INSERT
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if list.iter().any(|c| c == id.as_str()) {
        return Err(DeckError::Invalid("injected failure".to_owned()));
    }
    Ok(())
}

/// SHA-256 of the sorted comment ids, as hex.
fn fingerprint(ids: &[String]) -> String {
    let mut sorted: Vec<&str> = ids.iter().map(String::as_str).collect();
    sorted.sort_unstable();
    let d = digest(&SHA256, sorted.join("\n").as_bytes());
    d.as_ref().iter().map(|b| format!("{b:02x}")).collect()
}

/// Sets the text of the follow-up in the stored envelope.
fn set_text(cmd: &mut NewCommand, text: String) -> Result<()> {
    let slot = cmd
        .envelope
        .pointer_mut("/command/input/text")
        .ok_or_else(|| DeckError::Corrupt("the follow-up has no input text".to_owned()))?;
    *slot = serde_json::Value::String(text);
    Ok(())
}

/// The review row of `id` in the session.
fn review_row(
    conn: &Connection,
    station: &StationId,
    session: &str,
    id: &str,
) -> Result<Option<(ReviewSummary, String)>> {
    Ok(conn
        .query_row(
            &format!(
                "SELECT {REVIEW_COLUMNS}, fingerprint FROM reviews
                 WHERE station_id = ?1 AND session_id = ?2 AND review_id = ?3"
            ),
            params![station.as_str(), session, id],
            |r| Ok((summary(r)?, r.get::<_, String>(9)?)),
        )
        .optional()?)
}

/// The comments of a review, oldest first.
fn review_comments(
    conn: &Connection,
    station: &StationId,
    session: &str,
    review: &str,
) -> Result<Vec<ReviewComment>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLUMNS} FROM review_comments
         WHERE station_id = ?1 AND session_id = ?2 AND review_id = ?3
         ORDER BY created_at, comment_id"
    ))?;
    let rows = stmt
        .query_map(params![station.as_str(), session, review], comment)?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

/// Adds the short ids that `viewer` may see to a review summary.
fn with_ids(mut s: ReviewSummary, comments: &[ReviewComment], viewer: &Viewer) -> ReviewSummary {
    s.comment_ids = comments
        .iter()
        .filter(|c| visible(c, viewer, &Hide::Nothing))
        .map(|c| c.short_id.clone())
        .collect();
    s
}

/// The text lines of `comments`; the short id of the top comment of a reply
/// comes from the session.
fn text_comments(
    conn: &Connection,
    station: &StationId,
    session: &str,
    comments: &[ReviewComment],
) -> Result<Vec<TextComment>> {
    let mut out = Vec::with_capacity(comments.len());
    for c in comments {
        let reply_to = if c.thread_id == c.comment_id {
            None
        } else {
            find(conn, station, session, &c.thread_id)?.map(|top| top.short_id)
        };
        out.push(TextComment {
            short_id: c.short_id.clone(),
            reply_to,
            path: c.path.clone(),
            other_path: c.other_path.clone(),
            side: c.side,
            line_start: c.line_start,
            line_end: c.line_end,
            anchor_text: c.anchor_text.clone(),
            imported: c.anchor_kind == AnchorKind::Imported,
            body: c.body.clone(),
        });
    }
    Ok(out)
}

fn sender(viewer: &Viewer) -> Sender<'_> {
    match viewer {
        Viewer::Member(name) => Sender::Member(name),
        Viewer::Owner | Viewer::Token => Sender::Owner,
    }
}

impl DeckStore {
    /// Sends the drafts `req.comment_ids` as one review (design 3). A repeat
    /// of a send (same `send_key`, same comments) returns the stored result.
    ///
    /// # Errors
    /// Refusals `send_key_reused`, `station_unreachable`, `too_many_reviews`,
    /// `unknown_comment`, `not_draft`, `thread_root_unsent`,
    /// `review_too_large`; a database error.
    pub async fn review_send(&self, req: SendRequest) -> Outcome<Sent> {
        let now = self.now_ms();
        let ids = self.inner.ids.clone();
        let out = flat(
            call(&self.inner.db, move |conn| {
                let tx = conn.transaction()?;
                let SendRequest {
                    station,
                    session,
                    viewer,
                    send_key,
                    comment_ids,
                    excerpts,
                    mut command,
                    link_up,
                } = req;
                let principal = viewer.principal().unwrap_or_default();
                // 1. The replay comes first, inside the transaction.
                let old: Option<(String, String)> = tx
                    .query_row(
                        "SELECT review_id, fingerprint FROM reviews
                     WHERE station_id = ?1 AND session_id = ?2 AND principal = ?3
                       AND send_key = ?4",
                        params![station.as_str(), session, principal, send_key],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )
                    .optional()?;
                let mut picked = Vec::with_capacity(comment_ids.len());
                for id in &comment_ids {
                    let c = find(&tx, &station, &session, id)?
                        .filter(|c| visible(c, &viewer, &Hide::Nothing));
                    match c {
                        Some(c) => picked.push(c),
                        None if old.is_none() => refuse!("unknown_comment", "no such comment"),
                        None => {}
                    }
                }
                let print = fingerprint(
                    &picked
                        .iter()
                        .map(|c| c.comment_id.clone())
                        .collect::<Vec<_>>(),
                );
                if let Some((review_id, stored)) = old {
                    if stored != print {
                        refuse!(
                            "send_key_reused",
                            "this send key was used for other comments"
                        );
                    }
                    let (s, _) = review_row(&tx, &station, &session, &review_id)?
                        .ok_or_else(|| DeckError::Corrupt("a stored review is gone".to_owned()))?;
                    let comments = review_comments(&tx, &station, &session, &review_id)?;
                    return Ok(Ok(Sent {
                        summary: with_ids(s, &comments, &viewer),
                        created: false,
                    }));
                }
                if !link_up {
                    refuse!(
                        "station_unreachable",
                        "the station has no live link to this deck"
                    );
                }
                // 2. The cap of reviews in the life of a session.
                let count: i64 = tx.query_row(
                    "SELECT COUNT(*) FROM reviews WHERE station_id = ?1 AND session_id = ?2",
                    params![station.as_str(), session],
                    |r| r.get(0),
                )?;
                if usize::try_from(count).unwrap_or(usize::MAX) >= MAX_REVIEWS_PER_SESSION {
                    refuse!(
                        "too_many_reviews",
                        format!("a session has at most {MAX_REVIEWS_PER_SESSION} reviews")
                    );
                }
                // 3. The drafts: the caller's, in this session, still drafts, and
                // the top comment of each reply is sent or in this send.
                if picked.is_empty() || picked.len() > MAX_COMMENTS_PER_REVIEW {
                    return Err(DeckError::Invalid("1 to 50 comments".to_owned()));
                }
                if picked.iter().any(|c| c.state != CommentState::Draft) {
                    refuse!("not_draft", "a comment of this review is sent already");
                }
                let in_send: BTreeSet<&str> =
                    picked.iter().map(|c| c.comment_id.as_str()).collect();
                for c in picked.iter().filter(|c| c.thread_id != c.comment_id) {
                    let top = find(&tx, &station, &session, &c.thread_id)?;
                    let ok = top.as_ref().is_some_and(|t| {
                        t.state == CommentState::Sent || in_send.contains(t.comment_id.as_str())
                    });
                    if !ok {
                        refuse!(
                            "thread_root_unsent",
                            "a reply needs a top comment that is sent or in this review"
                        );
                    }
                }
                picked.sort_by(|a, b| {
                    (a.created_at, &a.comment_id).cmp(&(b.created_at, &b.comment_id))
                });
                // 4. The text and the stored command.
                let review_id: String = new_id(ids.as_ref());
                let short_id = alloc_short(&tx, "reviews", &station, &session, |len| {
                    random_short("r-", len)
                })?;
                let lines = text_comments(&tx, &station, &session, &picked)?;
                let Ok(text) =
                    review_text::build(&short_id, &sender(&viewer), &lines, excerpts.as_ref())
                else {
                    refuse!(
                        "review_too_large",
                        "the follow-up of this review is over 64 KiB"
                    );
                };
                set_text(&mut command, text)?;
                commands::insert(&tx, &command, now)?;
                #[cfg(test)]
                fail_after_insert(&command.command_id)?;
                // 5. The review row, the sent drafts, the counter.
                tx.execute(
                    "INSERT INTO reviews
                     (review_id, short_id, station_id, session_id, send_key, fingerprint,
                      principal, command_id, delivery, attempts, comment_count, sent_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'pending', 1, ?9, ?10)",
                    params![
                        review_id,
                        short_id,
                        station.as_str(),
                        session,
                        send_key,
                        print,
                        principal,
                        command.command_id.as_str(),
                        i64::try_from(picked.len()).unwrap_or(i64::MAX),
                        now
                    ],
                )?;
                for c in &picked {
                    tx.execute(
                        "UPDATE review_comments SET state = 'sent', review_id = ?2, updated_at = ?3
                     WHERE comment_id = ?1",
                        params![c.comment_id, review_id, now],
                    )?;
                }
                bump(&tx, &station, &session)?;
                let (s, _) = review_row(&tx, &station, &session, &review_id)?
                    .ok_or_else(|| DeckError::Corrupt("a new review is gone".to_owned()))?;
                let comments = review_comments(&tx, &station, &session, &review_id)?;
                tx.commit()?;
                Ok(Ok(Sent {
                    summary: with_ids(s, &comments, &viewer),
                    created: true,
                }))
            })
            .await,
        )?;
        if out.created {
            self.commands_changed();
        }
        self.board_changed();
        Ok(out)
    }

    /// Sends a `failed` review again (design 3, step 6): one transaction with
    /// the replay of the `attempt_key`, the text rebuilt under the current deny
    /// policy, a new stored command and `attempts + 1`.
    ///
    /// # Errors
    /// Refusals `unknown_review`, `forbidden` (another sender), `not_failed`,
    /// `too_many_attempts`, `station_unreachable`, `review_too_large`.
    pub async fn review_send_again(&self, req: AgainRequest) -> Outcome<Sent> {
        let now = self.now_ms();
        let out = flat(
            call(&self.inner.db, move |conn| {
                let tx = conn.transaction()?;
                let AgainRequest {
                    station,
                    session,
                    viewer,
                    review_id,
                    attempt_key,
                    excerpts,
                    mut command,
                    link_up,
                } = req;
                let Some((mut s, _)) = review_row(&tx, &station, &session, &review_id)? else {
                    refuse!("unknown_review", "no such review");
                };
                let comments = review_comments(&tx, &station, &session, &review_id)?;
                let again: Option<String> = tx
                    .query_row(
                        "SELECT command_id FROM review_attempts
                     WHERE review_id = ?1 AND attempt_key = ?2",
                        params![review_id, attempt_key],
                        |r| r.get(0),
                    )
                    .optional()?;
                if let Some(command_id) = again {
                    s.command_id = command_id;
                    return Ok(Ok(Sent {
                        summary: with_ids(s, &comments, &viewer),
                        created: false,
                    }));
                }
                if viewer.principal().as_deref() != Some(s.principal.as_str()) {
                    refuse!("forbidden", "only the sender of a review sends it again");
                }
                if s.delivery != Delivery::Failed {
                    refuse!("not_failed", "only a failed review is sent again");
                }
                if s.attempts >= MAX_ATTEMPTS {
                    refuse!(
                        "too_many_attempts",
                        format!("a review is sent at most {MAX_ATTEMPTS} times")
                    );
                }
                if !link_up {
                    refuse!(
                        "station_unreachable",
                        "the station has no live link to this deck"
                    );
                }
                let lines = text_comments(&tx, &station, &session, &comments)?;
                let who = match s.principal.strip_prefix("member:") {
                    Some(name) => Sender::Member(name),
                    None => Sender::Owner,
                };
                let Ok(text) = review_text::build(&s.short_id, &who, &lines, excerpts.as_ref())
                else {
                    refuse!(
                        "review_too_large",
                        "the follow-up of this review is over 64 KiB"
                    );
                };
                set_text(&mut command, text)?;
                commands::insert(&tx, &command, now)?;
                let attempt = s.attempts + 1;
                tx.execute(
                    "UPDATE reviews SET delivery = 'pending', attempts = ?2, command_id = ?3,
                     input_id = NULL, reason = NULL
                 WHERE review_id = ?1",
                    params![review_id, attempt, command.command_id.as_str()],
                )?;
                tx.execute(
                    "INSERT INTO review_attempts (review_id, attempt_key, command_id, attempt)
                 VALUES (?1, ?2, ?3, ?4)",
                    params![review_id, attempt_key, command.command_id.as_str(), attempt],
                )?;
                bump(&tx, &station, &session)?;
                let (now_row, _) = review_row(&tx, &station, &session, &review_id)?
                    .ok_or_else(|| DeckError::Corrupt("a review is gone".to_owned()))?;
                tx.commit()?;
                Ok(Ok(Sent {
                    summary: with_ids(now_row, &comments, &viewer),
                    created: true,
                }))
            })
            .await,
        )?;
        if out.created {
            self.commands_changed();
        }
        self.board_changed();
        Ok(out)
    }

    /// The command id of a review and its delivery, for a test or a probe.
    ///
    /// # Errors
    /// A database error.
    pub async fn review_command(&self, review_id: String) -> Result<Option<CommandId>> {
        call(&self.inner.db, move |conn| {
            Ok(conn
                .query_row(
                    "SELECT command_id FROM reviews WHERE review_id = ?1",
                    [review_id],
                    |r| r.get::<_, String>(0),
                )
                .optional()?
                .map(CommandId::from))
        })
        .await
    }
}
