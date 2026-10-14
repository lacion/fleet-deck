//! Review comments on the Changes diff (DL-179, design `review-comments.md`
//! sections 1, 2 and 4): the comment tables, the rules of who sees and
//! changes what, the review counter of a session and the cleanup.
//!
//! The deck has one writer connection, so every method here is one
//! transaction and two calls never interleave inside it. The Send and "Send
//! again" transactions are in [`super::review_send`].
//!
//! A refusal is a [`Refusal`] with a stable code; the HTTP layer maps the
//! codes to statuses. A refusal drops the transaction, so it changes
//! nothing.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use fleetdeck_proto::command::{DeckCommandState, ReceiptState};
use fleetdeck_proto::member_deny::MemberDeny;
use fleetdeck_proto::review::{
    AnchorKind, AuthorKind, CommentState, Delivery, MAX_COMMENTS_PER_SESSION, REVIEWS_PAGE,
    ReviewComment, ReviewSide, ReviewSummary, ReviewThread, ReviewThreads,
};
use fleetdeck_proto::{CommandId, Receipt, SessionId, StationId};
use ring::rand::{SecureRandom, SystemRandom};
use rusqlite::{Connection, OptionalExtension, params};

use super::{DeckError, DeckStore, Result, call, new_id};

/// The alphabet of a short id: base32 without `l`, `o`, `0` and `1`.
const ALPHABET: &[u8; 32] = b"abcdefghijkmnpqrstuvwxyz23456789";
/// The lengths of the random part of a short id, in the order of the retries.
const SHORT_LENGTHS: [usize; 4] = [6, 8, 10, 12];

/// Who reads or writes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Viewer {
    /// The owner's board login.
    Owner,
    /// A member of a shared session, by display name.
    Member(String),
    /// An API token: sent comments only, never drafts.
    Token,
}

impl Viewer {
    /// `owner` or `member:<name>`; a token has none.
    #[must_use]
    pub fn principal(&self) -> Option<String> {
        match self {
            Self::Owner => Some("owner".to_owned()),
            Self::Member(name) => Some(format!("member:{name}")),
            Self::Token => None,
        }
    }

    /// True when the comment was written by this viewer.
    #[must_use]
    pub fn wrote(&self, c: &ReviewComment) -> bool {
        match self {
            Self::Owner => c.author_kind == AuthorKind::Owner,
            Self::Member(name) => {
                c.author_kind == AuthorKind::Member && c.author_name.as_deref() == Some(name)
            }
            Self::Token => false,
        }
    }
}

/// Which comments the member deny list hides from the viewer (design 2.3).
#[derive(Debug, Clone)]
pub enum Hide {
    /// The owner and a token see everything.
    Nothing,
    /// The deny list of the station cannot be established: a member sees no
    /// comment of the session.
    Everything,
    /// A member does not see a comment whose path or other path matches.
    Denied(MemberDeny),
}

impl Hide {
    fn hides(&self, c: &ReviewComment) -> bool {
        match self {
            Self::Nothing => false,
            Self::Everything => true,
            Self::Denied(d) => {
                d.denies(&c.path) || c.other_path.as_deref().is_some_and(|p| d.denies(p))
            }
        }
    }
}

/// A comment as the viewer may see it: a sent comment that the deny list does
/// not hide, or a draft that the viewer wrote.
pub(super) fn visible(c: &ReviewComment, viewer: &Viewer, hide: &Hide) -> bool {
    if hide.hides(c) {
        return false;
    }
    match c.state {
        CommentState::Sent => true,
        CommentState::Draft => viewer.wrote(c),
    }
}

/// A refusal with a stable code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    pub code: &'static str,
    pub message: String,
}

impl Refusal {
    #[must_use]
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

/// Why a review call failed.
#[derive(Debug)]
pub enum ReviewError {
    Refused(Refusal),
    Deck(DeckError),
}

impl From<DeckError> for ReviewError {
    fn from(e: DeckError) -> Self {
        Self::Deck(e)
    }
}

impl From<Refusal> for ReviewError {
    fn from(r: Refusal) -> Self {
        Self::Refused(r)
    }
}

/// The result of a review call.
pub type Outcome<T> = std::result::Result<T, ReviewError>;

/// Joins the two layers of a transaction closure: a refusal comes out as the
/// call's error (and the closure drops the transaction, so nothing changes).
pub(super) fn flat<T>(r: Result<std::result::Result<T, Refusal>>) -> Outcome<T> {
    match r {
        Ok(Ok(v)) => Ok(v),
        Ok(Err(refusal)) => Err(ReviewError::Refused(refusal)),
        Err(e) => Err(ReviewError::Deck(e)),
    }
}

/// Returns a refusal from a transaction closure.
macro_rules! refuse {
    ($code:expr, $msg:expr) => {
        return Ok(Err($crate::store::review::Refusal::new($code, $msg)))
    };
}
pub(super) use refuse;

pub(super) const COLUMNS: &str = "comment_id, short_id, thread_id, snapshot, path, other_path, \
     side, line_start, line_end, anchor_text, anchor_kind, body, author_kind, author_name, state, \
     resolved, review_id, created_at, updated_at";

fn bad(col: usize, what: &str) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(col, rusqlite::types::Type::Text, what.into())
}

pub(super) fn comment(r: &rusqlite::Row<'_>) -> rusqlite::Result<ReviewComment> {
    let text = |i: usize| r.get::<_, String>(i);
    let side = match text(6)?.as_str() {
        "new" => ReviewSide::New,
        "old" => ReviewSide::Old,
        _ => return Err(bad(6, "side")),
    };
    let anchor_kind = match text(10)?.as_str() {
        "diff" => AnchorKind::Diff,
        "imported" => AnchorKind::Imported,
        _ => return Err(bad(10, "anchor_kind")),
    };
    let author_kind = match text(12)?.as_str() {
        "owner" => AuthorKind::Owner,
        "member" => AuthorKind::Member,
        _ => return Err(bad(12, "author_kind")),
    };
    let state = match text(14)?.as_str() {
        "draft" => CommentState::Draft,
        "sent" => CommentState::Sent,
        _ => return Err(bad(14, "state")),
    };
    Ok(ReviewComment {
        comment_id: text(0)?,
        short_id: text(1)?,
        thread_id: text(2)?,
        snapshot: text(3)?,
        path: text(4)?,
        other_path: r.get(5)?,
        side,
        line_start: r.get(7)?,
        line_end: r.get(8)?,
        anchor_text: text(9)?,
        anchor_kind,
        body: text(11)?,
        author_kind,
        author_name: r.get(13)?,
        state,
        resolved: r.get::<_, i64>(15)? != 0,
        review_id: r.get(16)?,
        created_at: r.get(17)?,
        updated_at: r.get(18)?,
    })
}

/// A random short id: `prefix` and `len` characters of [`ALPHABET`] from the
/// system random source.
///
/// # Errors
/// [`DeckError::Hash`] when the random source fails.
pub(super) fn random_short(prefix: &str, len: usize) -> Result<String> {
    let mut bytes = vec![0u8; len];
    SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| DeckError::Hash("the system random source failed".to_owned()))?;
    let mut out = String::with_capacity(prefix.len() + len);
    out.push_str(prefix);
    out.extend(
        bytes
            .iter()
            .map(|b| char::from(ALPHABET[usize::from(b & 31)])),
    );
    Ok(out)
}

/// A short id of `table` (`review_comments` or `reviews`) that no row of the
/// session has: the first free one of the lengths 6, 8, 10 and 12. `make`
/// draws a candidate of the given length.
///
/// # Errors
/// [`DeckError::Corrupt`] when all four lengths clash.
pub(super) fn alloc_short(
    conn: &Connection,
    table: &str,
    station: &StationId,
    session: &str,
    mut make: impl FnMut(usize) -> Result<String>,
) -> Result<String> {
    for len in SHORT_LENGTHS {
        let id = make(len)?;
        let taken: bool = conn.query_row(
            &format!(
                "SELECT EXISTS (SELECT 1 FROM {table}
                 WHERE station_id = ?1 AND session_id = ?2 AND short_id = ?3)"
            ),
            params![station.as_str(), session, id],
            |r| r.get(0),
        )?;
        if !taken {
            return Ok(id);
        }
    }
    Err(DeckError::Corrupt(
        "no free short id after four lengths".to_owned(),
    ))
}

/// Adds one to the review counter of a session.
pub(super) fn bump(conn: &Connection, station: &StationId, session: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO review_revs (station_id, session_id, rev) VALUES (?1, ?2, 1)
         ON CONFLICT (station_id, session_id) DO UPDATE SET rev = rev + 1",
        params![station.as_str(), session],
    )?;
    Ok(())
}

/// The comment named `id` in the session: a comment id (a ULID) or a short id
/// (`c-` and base32); the two forms cannot clash.
pub(super) fn find(
    conn: &Connection,
    station: &StationId,
    session: &str,
    id: &str,
) -> Result<Option<ReviewComment>> {
    Ok(conn
        .query_row(
            &format!(
                "SELECT {COLUMNS} FROM review_comments
                 WHERE station_id = ?1 AND session_id = ?2
                   AND (comment_id = ?3 OR short_id = ?3)"
            ),
            params![station.as_str(), session, id],
            comment,
        )
        .optional()?)
}

/// Deletes every review row of one session.
pub(super) fn delete_session(conn: &Connection, station: &StationId, session: &str) -> Result<()> {
    let key = params![station.as_str(), session];
    conn.execute(
        "DELETE FROM review_attempts WHERE review_id IN
             (SELECT review_id FROM reviews WHERE station_id = ?1 AND session_id = ?2)",
        key,
    )?;
    for table in [
        "review_imports",
        "review_comments",
        "reviews",
        "review_revs",
    ] {
        conn.execute(
            &format!("DELETE FROM {table} WHERE station_id = ?1 AND session_id = ?2"),
            key,
        )?;
    }
    Ok(())
}

/// Deletes every review row of a removed station.
pub(super) fn delete_station(conn: &Connection, station: &StationId) -> Result<()> {
    conn.execute(
        "DELETE FROM review_attempts WHERE review_id IN
             (SELECT review_id FROM reviews WHERE station_id = ?1)",
        [station.as_str()],
    )?;
    for table in [
        "review_imports",
        "review_comments",
        "reviews",
        "review_revs",
    ] {
        conn.execute(
            &format!("DELETE FROM {table} WHERE station_id = ?1"),
            [station.as_str()],
        )?;
    }
    Ok(())
}

/// Deletes the review rows of the sessions of `station` that a completed
/// snapshot does not name; the rows of the named sessions stay.
pub(super) fn delete_unlisted(
    conn: &Connection,
    station: &StationId,
    named: &BTreeSet<String>,
) -> Result<()> {
    let mut stmt = conn.prepare(
        "SELECT session_id FROM review_comments WHERE station_id = ?1
         UNION SELECT session_id FROM reviews WHERE station_id = ?1
         UNION SELECT session_id FROM review_revs WHERE station_id = ?1
         UNION SELECT session_id FROM review_imports WHERE station_id = ?1",
    )?;
    let all: Vec<String> = stmt
        .query_map([station.as_str()], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);
    for session in all.iter().filter(|s| !named.contains(*s)) {
        delete_session(conn, station, session)?;
    }
    Ok(())
}

/// The delivery that a command end gives its review: `(delivery, input id,
/// reason)`; `None` while the command is pending.
fn delivery_of(
    state: DeckCommandState,
    receipt: Option<&Receipt>,
    reason: Option<&str>,
) -> Option<(Delivery, Option<String>, Option<String>)> {
    let reason = reason.map(str::to_owned);
    match state {
        DeckCommandState::Pending => None,
        DeckCommandState::Done => {
            let r = receipt?;
            if r.state == ReceiptState::Accepted {
                let input = r
                    .result
                    .as_ref()
                    .and_then(|x| x.input_id.as_ref())
                    .map(|i| i.as_str().to_owned());
                Some((Delivery::Delivered, input, None))
            } else {
                let code = r
                    .reject
                    .as_ref()
                    .and_then(|x| serde_json::to_value(x.code).ok())
                    .and_then(|v| v.as_str().map(str::to_owned));
                Some((Delivery::Failed, None, code))
            }
        }
        DeckCommandState::Failed => Some((Delivery::Failed, None, reason)),
        DeckCommandState::Ambiguous => Some((Delivery::Unknown, None, reason)),
    }
}

/// The delivery of the review of a command that just ended (design 3, step
/// 5), in the transaction that records the end. Returns true when a review
/// changed.
pub(super) fn command_ended(
    conn: &Connection,
    id: &CommandId,
    state: DeckCommandState,
    receipt: Option<&Receipt>,
    reason: Option<&str>,
) -> Result<bool> {
    let Some((delivery, input_id, why)) = delivery_of(state, receipt, reason) else {
        return Ok(false);
    };
    let hit: Option<(String, String)> = conn
        .query_row(
            "UPDATE reviews SET delivery = ?2, input_id = ?3, reason = ?4
             WHERE command_id = ?1 AND delivery = 'pending'
             RETURNING station_id, session_id",
            params![id.as_str(), delivery.as_str(), input_id, why],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    match hit {
        Some((station, session)) => {
            bump(conn, &StationId::from(station), &session)?;
            Ok(true)
        }
        None => Ok(false),
    }
}

/// The fields of the anchor of a new top comment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnchorFields {
    pub snapshot: String,
    pub path: String,
    pub other_path: Option<String>,
    pub side: ReviewSide,
    pub line_start: u32,
    pub line_end: u32,
    pub anchor_text: String,
    pub anchor_kind: AnchorKind,
}

/// What a change of a comment did, for the audit log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edited {
    pub comment: ReviewComment,
    pub body_changed: bool,
    pub resolved_changed: bool,
}

/// A new top comment, or a reply to the thread of a top comment (which
/// copies the anchor of the top comment).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Anchor(AnchorFields),
    Reply(String),
}

/// A new comment; the route has checked the shape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewReviewComment {
    pub target: Target,
    pub body: String,
    /// An imported note: a repeat returns the comment that it made.
    pub import_key: Option<String>,
}

impl DeckStore {
    /// The visible threads and the newest reviews of a session, the older
    /// reviews before `before` (a `review_id`).
    ///
    /// # Errors
    /// A database error.
    pub async fn review_list(
        &self,
        station: StationId,
        session: String,
        viewer: Viewer,
        hide: Hide,
        before: Option<String>,
    ) -> Result<ReviewThreads> {
        call(&self.inner.db, move |conn| {
            let mut stmt = conn.prepare(&format!(
                "SELECT {COLUMNS} FROM review_comments WHERE station_id = ?1 AND session_id = ?2
                 ORDER BY created_at, comment_id"
            ))?;
            let all: Vec<ReviewComment> = stmt
                .query_map(params![station.as_str(), session], comment)?
                .collect::<rusqlite::Result<_>>()?;
            drop(stmt);
            let seen: Vec<ReviewComment> = all
                .into_iter()
                .filter(|c| visible(c, &viewer, &hide))
                .collect();
            let mut order: Vec<String> = Vec::new();
            let mut threads: HashMap<String, Vec<ReviewComment>> = HashMap::new();
            for c in &seen {
                if !threads.contains_key(&c.thread_id) {
                    order.push(c.thread_id.clone());
                }
                threads
                    .entry(c.thread_id.clone())
                    .or_default()
                    .push(c.clone());
            }
            let threads = order
                .into_iter()
                .map(|id| ReviewThread {
                    comments: threads.remove(&id).unwrap_or_default(),
                    thread_id: id,
                })
                .collect();
            let mut stmt = conn.prepare(
                "SELECT review_id, short_id, principal, delivery, attempts, input_id, reason,
                        sent_at, command_id
                 FROM reviews WHERE station_id = ?1 AND session_id = ?2
                   AND (?3 IS NULL OR review_id < ?3)
                 ORDER BY review_id DESC LIMIT ?4",
            )?;
            let limit = i64::try_from(REVIEWS_PAGE + 1).unwrap_or(i64::MAX);
            let mut reviews: Vec<ReviewSummary> = stmt
                .query_map(params![station.as_str(), session, before, limit], summary)?
                .collect::<rusqlite::Result<_>>()?;
            drop(stmt);
            let more_reviews = reviews.len() > REVIEWS_PAGE;
            reviews.truncate(REVIEWS_PAGE);
            // A viewer without a deny list gets nothing of the session.
            if matches!(hide, Hide::Everything) {
                reviews.clear();
            }
            for r in &mut reviews {
                r.comment_ids = seen
                    .iter()
                    .filter(|c| c.review_id.as_deref() == Some(r.review_id.as_str()))
                    .map(|c| c.short_id.clone())
                    .collect();
            }
            Ok(ReviewThreads {
                threads,
                more_reviews: more_reviews && !reviews.is_empty(),
                reviews,
                rev: rev_of(conn, &station, &session)?,
            })
        })
        .await
    }

    /// The review counter of every session that has one.
    ///
    /// # Errors
    /// A database error.
    pub async fn review_revs(&self) -> Result<BTreeMap<(StationId, SessionId), u32>> {
        call(&self.inner.db, |conn| {
            let mut stmt = conn.prepare("SELECT station_id, session_id, rev FROM review_revs")?;
            let rows = stmt.query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                ))
            })?;
            let mut out = BTreeMap::new();
            for row in rows {
                let (station, session, rev) = row?;
                out.insert(
                    (StationId::from(station), SessionId::from(session)),
                    u32::try_from(rev).unwrap_or(u32::MAX),
                );
            }
            Ok(out)
        })
        .await
    }

    /// Adds a comment. Returns the comment and whether it is new (`false`:
    /// the `import_key` was imported before and the stored comment returns).
    ///
    /// # Errors
    /// Refusals `too_many_comments`, `unknown_comment` (a reply whose top
    /// comment the viewer cannot see); a database error.
    pub async fn review_add(
        &self,
        station: StationId,
        session: String,
        viewer: Viewer,
        hide: Hide,
        new: NewReviewComment,
    ) -> Outcome<(ReviewComment, bool)> {
        let now = self.now_ms();
        let ids = self.inner.ids.clone();
        let out = flat(
            call(&self.inner.db, move |conn| {
                let tx = conn.transaction()?;
                let principal = viewer.principal().unwrap_or_default();
                if let Some(key) = &new.import_key {
                    let old: Option<String> = tx
                        .query_row(
                            "SELECT comment_id FROM review_imports
                         WHERE station_id = ?1 AND session_id = ?2 AND principal = ?3
                           AND import_key = ?4",
                            params![station.as_str(), session, principal, key],
                            |r| r.get(0),
                        )
                        .optional()?;
                    if let Some(old) = old
                        && let Some(c) = find(&tx, &station, &session, &old)?
                    {
                        return Ok(Ok((c, false)));
                    }
                }
                let count: i64 = tx.query_row(
                "SELECT COUNT(*) FROM review_comments WHERE station_id = ?1 AND session_id = ?2",
                params![station.as_str(), session],
                |r| r.get(0),
            )?;
                if usize::try_from(count).unwrap_or(usize::MAX) >= MAX_COMMENTS_PER_SESSION {
                    refuse!(
                        "too_many_comments",
                        format!("a session has at most {MAX_COMMENTS_PER_SESSION} comments")
                    );
                }
                let (thread, a) = match &new.target {
                    Target::Anchor(a) => (None, a.clone()),
                    Target::Reply(top) => {
                        let top = find(&tx, &station, &session, top)?
                            .filter(|c| c.comment_id == c.thread_id && visible(c, &viewer, &hide));
                        let Some(top) = top else {
                            refuse!("unknown_comment", "no such comment to reply to");
                        };
                        (
                            Some(top.comment_id.clone()),
                            AnchorFields {
                                snapshot: top.snapshot,
                                path: top.path,
                                other_path: top.other_path,
                                side: top.side,
                                line_start: top.line_start,
                                line_end: top.line_end,
                                anchor_text: top.anchor_text,
                                anchor_kind: top.anchor_kind,
                            },
                        )
                    }
                };
                let comment_id: String = new_id(ids.as_ref());
                let short_id = alloc_short(&tx, "review_comments", &station, &session, |len| {
                    random_short("c-", len)
                })?;
                let (kind, name) = match &viewer {
                    Viewer::Owner => ("owner", None),
                    Viewer::Member(n) => ("member", Some(n.clone())),
                    Viewer::Token => {
                        return Err(DeckError::Invalid("a token writes no comment".to_owned()));
                    }
                };
                tx.execute(
                    "INSERT INTO review_comments
                     (comment_id, short_id, station_id, session_id, thread_id, snapshot, path,
                      other_path, side, line_start, line_end, anchor_text, anchor_kind, body,
                      author_kind, author_name, state, resolved, review_id, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16,
                         'draft', 0, NULL, ?17, ?17)",
                    params![
                        comment_id,
                        short_id,
                        station.as_str(),
                        session,
                        thread.as_deref().unwrap_or(&comment_id),
                        a.snapshot,
                        a.path,
                        a.other_path,
                        a.side.as_str(),
                        a.line_start,
                        a.line_end,
                        a.anchor_text,
                        match a.anchor_kind {
                            AnchorKind::Diff => "diff",
                            AnchorKind::Imported => "imported",
                        },
                        new.body,
                        kind,
                        name,
                        now
                    ],
                )?;
                if let Some(key) = &new.import_key {
                    tx.execute(
                        "INSERT INTO review_imports
                         (station_id, session_id, principal, import_key, comment_id)
                     VALUES (?1, ?2, ?3, ?4, ?5)
                     ON CONFLICT (station_id, session_id, principal, import_key)
                     DO UPDATE SET comment_id = excluded.comment_id",
                        params![station.as_str(), session, principal, key, comment_id],
                    )?;
                }
                bump(&tx, &station, &session)?;
                let stored = find(&tx, &station, &session, &comment_id)?
                    .ok_or_else(|| DeckError::Corrupt("a new comment is gone".to_owned()))?;
                tx.commit()?;
                Ok(Ok((stored, true)))
            })
            .await,
        )?;
        self.board_changed();
        Ok(out)
    }
}

pub(super) fn rev_of(conn: &Connection, station: &StationId, session: &str) -> Result<u32> {
    let rev: Option<i64> = conn
        .query_row(
            "SELECT rev FROM review_revs WHERE station_id = ?1 AND session_id = ?2",
            params![station.as_str(), session],
            |r| r.get(0),
        )
        .optional()?;
    Ok(u32::try_from(rev.unwrap_or(0)).unwrap_or(u32::MAX))
}

/// The columns of [`summary`].
pub(super) const REVIEW_COLUMNS: &str = "review_id, short_id, principal, delivery, attempts, \
     input_id, reason, sent_at, command_id";

/// A review row without its comment ids.
pub(super) fn summary(r: &rusqlite::Row<'_>) -> rusqlite::Result<ReviewSummary> {
    let delivery = match r.get::<_, String>(3)?.as_str() {
        "pending" => Delivery::Pending,
        "delivered" => Delivery::Delivered,
        "failed" => Delivery::Failed,
        "unknown" => Delivery::Unknown,
        _ => return Err(bad(3, "delivery")),
    };
    Ok(ReviewSummary {
        review_id: r.get(0)?,
        short_id: r.get(1)?,
        principal: r.get(2)?,
        delivery,
        attempts: r.get(4)?,
        input_id: r.get(5)?,
        reason: r.get(6)?,
        sent_at: r.get(7)?,
        comment_ids: Vec::new(),
        command_id: r.get(8)?,
    })
}
