//! Review comments on the Changes diff (DL-179, design `review-comments.md`
//! section 2 and 3): the routes under
//! `/api/sessions/{station}/{session}/review` and the turn diff by checkpoint
//! pair.
//!
//! - `GET …/review?reviews_before=` the visible threads and reviews;
//! - `POST …/review/comments` a comment or a reply, or an imported note;
//! - `PATCH` / `DELETE …/review/comments/{id}`;
//! - `POST …/review/send` and `…/review/reviews/{review_id}/send-again`;
//! - `GET /api/stations/{station}/sessions/{session}/turn-diff?from=&to=`.
//!
//! A reader is the owner, a member with access to the session, or a token
//! with `read`. A writer is the owner, or a member whose role may follow up
//! (a viewer-role member, a "share guest", reads sent comments and writes
//! nothing). A token never writes and never has drafts. Every mutation is
//! audited with ids only.

use std::sync::Arc;

use axum::Json;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Extension, Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use fleetdeck_proto::command::{CommandOrigin, SessionCommand, UserInput};
use fleetdeck_proto::files::{CheckpointRef, TurnDiff};
use fleetdeck_proto::review::{
    AnchorKind, CommentPatch, MAX_BODY_CHARS, MAX_COMMENTS_PER_REVIEW, MAX_PATH_BYTES,
    MAX_RANGE_LINES, NewComment, ReviewComment, ReviewSummary, ReviewThreads, SendAgain,
    SendReview,
};
use fleetdeck_proto::{CommandId, StationId};
use serde::Deserialize;

use super::guard::ApiError;
use super::review_anchor_cache::{self as cache, Snapshot};
use super::{Board, access};
use crate::link::CommandKind;
use crate::store::review::{AnchorFields, Hide, NewReviewComment, ReviewError, Target, Viewer};
use crate::store::review_anchor::AnchorError;
use crate::store::review_send::{AgainRequest, SendRequest};
use crate::store::{AuditResult, DeckError, MemberAction, NewAudit, Principal, member_allows};

fn invalid(message: impl Into<String>) -> ApiError {
    ApiError::new(StatusCode::BAD_REQUEST, "invalid_body", message)
}

/// The HTTP status of a stable refusal code (design answers Q4, Q7).
fn status_of(code: &str) -> StatusCode {
    match code {
        "not_author" | "forbidden" => StatusCode::FORBIDDEN,
        "unknown_comment" | "unknown_review" | "checkpoint_unavailable" => StatusCode::NOT_FOUND,
        "invalid_body" | "not_in_diff" | "anchor_too_large" | "review_too_large" => {
            StatusCode::BAD_REQUEST
        }
        _ => StatusCode::CONFLICT,
    }
}

fn api(e: ReviewError) -> ApiError {
    match e {
        ReviewError::Refused(r) => ApiError::new(status_of(r.code), r.code, r.message),
        ReviewError::Deck(e) => deck(&e),
    }
}

fn deck(e: &DeckError) -> ApiError {
    match e {
        DeckError::NotSynced(_) => ApiError::new(StatusCode::CONFLICT, "not_synced", e.to_string()),
        DeckError::UnknownStation(_) => {
            ApiError::new(StatusCode::NOT_FOUND, "unknown_station", e.to_string())
        }
        _ => ApiError::internal(e),
    }
}

fn body_of<T>(body: Result<Json<T>, JsonRejection>) -> Result<T, ApiError> {
    body.map(|Json(v)| v).map_err(|e| invalid(e.body_text()))
}

fn viewer_of(principal: &Principal) -> Viewer {
    match principal {
        Principal::Owner(_) => Viewer::Owner,
        Principal::Member(_, grant) => Viewer::Member(grant.display_name.clone()),
        Principal::Token(..) => Viewer::Token,
    }
}

/// The station and the session of a reader.
async fn reader(
    board: &Board,
    principal: &Principal,
    station: &str,
    session: &str,
) -> Result<StationId, ApiError> {
    access::session_of(
        board,
        principal,
        station,
        session,
        MemberAction::Read,
        "read comments",
    )
    .await
}

/// The station of a writer: not a token, and a member needs the role that
/// follows up.
async fn writer(
    board: &Board,
    principal: &Principal,
    station: &str,
    session: &str,
) -> Result<StationId, ApiError> {
    if matches!(principal, Principal::Token(..)) {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "owner_or_member_only",
            "a token reads comments and never writes them",
        ));
    }
    access::session_of(
        board,
        principal,
        station,
        session,
        MemberAction::FollowUp,
        "comment on this session",
    )
    .await
}

/// Which comments the member deny list hides from `principal`.
async fn hide_for(board: &Board, principal: &Principal, station: &StationId) -> Hide {
    if !matches!(principal, Principal::Member(..)) {
        return Hide::Nothing;
    }
    match cache::deny_list(board, station).await {
        Some(deny) => Hide::Denied(deny),
        None => Hide::Everything,
    }
}

async fn audit(board: &Board, principal: &Principal, action: &str, target: String, result: &str) {
    let mut row = NewAudit::new(principal.actor(), action, AuditResult::Ok).target(target);
    if !result.is_empty() {
        row = row.detail(result);
    }
    board.store.audit_or_log(row).await;
}

#[derive(Debug, Default, Deserialize)]
pub struct ListQuery {
    pub reviews_before: Option<String>,
}

/// `GET /api/sessions/{station}/{session}/review`.
///
/// # Errors
/// 404 `unknown_station` / `unknown_session`; 403 `forbidden` (a member's
/// role).
pub async fn list(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    Path((station, session)): Path<(String, String)>,
    Query(q): Query<ListQuery>,
) -> Result<Json<ReviewThreads>, ApiError> {
    let id = reader(&board, &principal, &station, &session).await?;
    let hide = hide_for(&board, &principal, &id).await;
    // A share guest (a member whose role cannot follow up) has no drafts.
    let viewer = match &principal {
        Principal::Member(_, grant)
            if !member_allows(grant, MemberAction::FollowUp, id.as_str(), &session) =>
        {
            Viewer::Token
        }
        other => viewer_of(other),
    };
    board
        .store
        .review_list(id, session, viewer, hide, q.reviews_before)
        .await
        .map(Json)
        .map_err(|e| deck(&e))
}

/// The path rule of every stored path (design 2.2): relative, no `..`
/// component, no NUL, no control character, at most 1024 bytes.
fn check_path(path: &str) -> Result<(), ApiError> {
    let bad = path.is_empty()
        || path.len() > MAX_PATH_BYTES
        || path.starts_with('/')
        || path.chars().any(char::is_control)
        || path.split('/').any(|c| c == "..");
    if bad {
        return Err(invalid(
            "a path is relative, without `..`, NUL or control characters, at most 1024 bytes",
        ));
    }
    Ok(())
}

fn check_body(body: &str) -> Result<(), ApiError> {
    if body.is_empty() || body.chars().count() > MAX_BODY_CHARS {
        return Err(invalid(format!(
            "a comment has 1 to {MAX_BODY_CHARS} characters"
        )));
    }
    Ok(())
}

/// An `import_key` is 64 lower-case hex characters.
fn check_import_key(key: &str) -> Result<(), ApiError> {
    if key.len() == 64 && key.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
        Ok(())
    } else {
        Err(invalid("an import key has 64 lower-case hex characters"))
    }
}

/// A client key (`send_key`, `attempt_key`): 1 to 64 characters from
/// `[0-9A-Za-z_-]`, as a command id.
fn check_key(key: &str, what: &str) -> Result<(), ApiError> {
    let ok = !key.is_empty()
        && key.len() <= 64
        && key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
    if ok {
        Ok(())
    } else {
        Err(invalid(format!(
            "{what} has 1 to 64 characters from [0-9A-Za-z_-]"
        )))
    }
}

/// The anchor of a new top comment, read from the diff of the caller.
async fn anchor_of(
    board: &Board,
    station: &StationId,
    session: &str,
    principal: &Principal,
    hide: &Hide,
    new: &NewComment,
) -> Result<AnchorFields, ApiError> {
    check_path(&new.path)?;
    if new.line_start == 0 || new.line_end < new.line_start {
        return Err(invalid(
            "lines start at 1 and the range does not end before it starts",
        ));
    }
    if new.line_end - new.line_start >= MAX_RANGE_LINES {
        return Err(invalid(format!(
            "a comment covers at most {MAX_RANGE_LINES} lines"
        )));
    }
    let snapshot = Snapshot::parse(&new.snapshot)
        .ok_or_else(|| invalid("a snapshot is `spawn:<oid>` or `turn:<oid>..<oid>`"))?;
    let member = matches!(principal, Principal::Member(..));
    let fields = |text: String, other: Option<String>, kind| AnchorFields {
        snapshot: snapshot.id(),
        path: new.path.clone(),
        other_path: other,
        side: new.side,
        line_start: new.line_start,
        line_end: new.line_end,
        anchor_text: text,
        anchor_kind: kind,
    };
    let found = cache::diff(board, station, session, &snapshot, member)
        .await
        .and_then(|d| {
            d.anchor(new.side, &new.path, new.line_start, new.line_end)
                .map_err(|e| match e {
                    AnchorError::NotInDiff => {
                        cache::not_in_diff("the file or the lines are not in this diff")
                    }
                    AnchorError::TooLarge => ApiError::new(
                        StatusCode::BAD_REQUEST,
                        "anchor_too_large",
                        "the commented lines have more than 4 KiB",
                    ),
                })
        });
    match found {
        // A cached diff can be older than the deny list: a hidden path is
        // "not in this diff", as a missing line is (NV-33).
        Ok(a) => {
            let hidden = |path: &str| match hide {
                Hide::Nothing => false,
                Hide::Everything => true,
                Hide::Denied(d) => d.denies(path),
            };
            if hidden(&new.path) || a.other_path.as_deref().is_some_and(hidden) {
                return Err(cache::not_in_diff(
                    "the file or the lines are not in this diff",
                ));
            }
            Ok(fields(a.text, a.other_path, AnchorKind::Diff))
        }
        // An imported note whose line left the diff (design 7). A member
        // cannot import a path that the deny list hides or may hide.
        Err(e) if e.code == "not_in_diff" && new.import_key.is_some() => {
            let allowed = match hide {
                Hide::Nothing => true,
                Hide::Everything => false,
                Hide::Denied(d) => !d.denies(&new.path),
            };
            if allowed {
                Ok(fields(String::new(), None, AnchorKind::Imported))
            } else {
                Err(e)
            }
        }
        Err(e) => Err(e),
    }
}

/// `POST …/review/comments`: 201 and the comment; 200 and the stored
/// comment for an `import_key` that was imported before.
///
/// # Errors
/// 400 `invalid_body`, `not_in_diff`, `anchor_too_large`; 403 `forbidden`,
/// `owner_or_member_only`; 404 `unknown_comment`, `checkpoint_unavailable`;
/// 409 `too_many_comments`, `station_unreachable`.
pub async fn add(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    Path((station, session)): Path<(String, String)>,
    body: Result<Json<NewComment>, JsonRejection>,
) -> Result<Response, ApiError> {
    let id = writer(&board, &principal, &station, &session).await?;
    let new = body_of(body)?;
    check_body(&new.body)?;
    if let Some(key) = &new.import_key {
        check_import_key(key)?;
    }
    let hide = hide_for(&board, &principal, &id).await;
    let target = match &new.thread_id {
        // A reply copies the anchor of its top comment; the fields of the
        // body that name an anchor are not used.
        Some(top) => Target::Reply(top.clone()),
        None => Target::Anchor(anchor_of(&board, &id, &session, &principal, &hide, &new).await?),
    };
    let action = if new.import_key.is_some() {
        "review.import"
    } else {
        "review.comment"
    };
    let (stored, created) = board
        .store
        .review_add(
            id,
            session,
            viewer_of(&principal),
            hide,
            NewReviewComment {
                target,
                body: new.body,
                import_key: new.import_key,
            },
        )
        .await
        .map_err(api)?;
    audit(
        &board,
        &principal,
        action,
        stored.comment_id.clone(),
        if created { "" } else { "duplicate" },
    )
    .await;
    let status = if created {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    Ok((status, Json::<ReviewComment>(stored)).into_response())
}

/// `PATCH …/review/comments/{comment}`.
///
/// # Errors
/// 400 `invalid_body`; 403 `forbidden`, `not_author`; 404
/// `unknown_comment`; 409 `not_draft`.
pub async fn edit(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    Path((station, session, comment)): Path<(String, String, String)>,
    body: Result<Json<CommentPatch>, JsonRejection>,
) -> Result<Json<ReviewComment>, ApiError> {
    let id = writer(&board, &principal, &station, &session).await?;
    let patch = body_of(body)?;
    if patch.body.is_none() && patch.resolved.is_none() {
        return Err(invalid("a change names `body` or `resolved`"));
    }
    if let Some(text) = &patch.body {
        check_body(text)?;
    }
    let hide = hide_for(&board, &principal, &id).await;
    let done = board
        .store
        .review_edit(id, session, viewer_of(&principal), hide, comment, patch)
        .await
        .map_err(api)?;
    let target = done.comment.comment_id.clone();
    if done.body_changed {
        audit(&board, &principal, "review.edit", target.clone(), "").await;
    }
    if done.resolved_changed {
        audit(&board, &principal, "review.resolve", target, "").await;
    }
    Ok(Json(done.comment))
}

/// `DELETE …/review/comments/{comment}`: 204.
///
/// # Errors
/// 403 `forbidden`, `not_author`; 404 `unknown_comment`; 409 `not_draft`,
/// `thread_has_replies`.
pub async fn delete(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    Path((station, session, comment)): Path<(String, String, String)>,
) -> Result<StatusCode, ApiError> {
    let id = writer(&board, &principal, &station, &session).await?;
    let hide = hide_for(&board, &principal, &id).await;
    let gone = board
        .store
        .review_delete(id, session, viewer_of(&principal), hide, comment)
        .await
        .map_err(api)?;
    audit(&board, &principal, "review.delete", gone.comment_id, "").await;
    Ok(StatusCode::NO_CONTENT)
}

/// The follow-up command of a review with an empty text, stored by the
/// send transaction after it sets the text.
async fn prepare(
    board: &Board,
    principal: &Principal,
    station: &StationId,
    session: &str,
) -> Result<crate::store::NewCommand, ApiError> {
    let author = match principal {
        Principal::Member(_, grant) => Some(grant.actor()),
        _ => None,
    };
    let command = SessionCommand::FollowUp {
        session_id: session.into(),
        input: UserInput {
            text: String::new(),
            images: Vec::new(),
            author,
        },
    };
    board
        .link
        .build_until(
            station.clone(),
            CommandOrigin::Board,
            CommandKind::Session { command },
            i64::MAX,
            (None, Some(principal.actor())),
            None,
        )
        .await
        .map_err(|e| deck(&e))
}

/// A member sends as a driver only (mob mode).
async fn driver(
    board: &Board,
    principal: &Principal,
    station: &StationId,
    session: &str,
) -> Result<(), ApiError> {
    access::ensure_driver(board, principal, station, session, MemberAction::FollowUp).await
}

fn sent_answer(board: &Arc<Board>, sent: &crate::store::review_send::Sent) {
    if sent.created {
        board
            .link
            .dispatch_after_commit(&CommandId::from(sent.summary.command_id.clone()));
    }
}

/// `POST …/review/send`: 200 and the review (the stored one for a repeat of
/// the same `send_key`).
///
/// # Errors
/// 400 `invalid_body`, `review_too_large`; 403 `forbidden`,
/// `owner_or_member_only`, `not_driver`; 404 `unknown_comment`; 409
/// `send_key_reused`, `station_unreachable`, `too_many_reviews`,
/// `not_draft`, `thread_root_unsent`, `not_synced`.
pub async fn send(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    Path((station, session)): Path<(String, String)>,
    body: Result<Json<SendReview>, JsonRejection>,
) -> Result<Json<ReviewSummary>, ApiError> {
    let id = writer(&board, &principal, &station, &session).await?;
    driver(&board, &principal, &id, &session).await?;
    let req = body_of(body)?;
    check_key(&req.send_key, "a send key")?;
    let n = req.comment_ids.len();
    let mut unique = req.comment_ids.clone();
    unique.sort_unstable();
    unique.dedup();
    if n == 0 || n > MAX_COMMENTS_PER_REVIEW || unique.len() != n {
        return Err(invalid(format!(
            "a review has 1 to {MAX_COMMENTS_PER_REVIEW} different comments"
        )));
    }
    let command = prepare(&board, &principal, &id, &session).await?;
    let excerpts = cache::deny_list(&board, &id).await;
    let link_up = board.link.control_for(&id).is_some();
    let sent = board
        .store
        .review_send(SendRequest {
            station: id,
            session,
            viewer: viewer_of(&principal),
            send_key: req.send_key,
            comment_ids: req.comment_ids,
            excerpts,
            command,
            link_up,
        })
        .await
        .map_err(api)?;
    sent_answer(&board, &sent);
    audit_send(&board, &principal, "review.send", &sent).await;
    Ok(Json(sent.summary))
}

async fn audit_send(
    board: &Board,
    principal: &Principal,
    action: &str,
    sent: &crate::store::review_send::Sent,
) {
    let mut row = NewAudit::new(principal.actor(), action, AuditResult::Ok)
        .target(sent.summary.review_id.clone())
        .command(CommandId::from(sent.summary.command_id.clone()));
    if !sent.created {
        row = row.detail("duplicate");
    }
    board.store.audit_or_log(row).await;
}

/// `POST …/review/reviews/{review}/send-again`: 200 and the review.
///
/// # Errors
/// 400 `invalid_body`, `review_too_large`; 403 `forbidden`,
/// `owner_or_member_only`, `not_driver`; 404 `unknown_review`; 409
/// `not_failed`, `too_many_attempts`, `station_unreachable`, `not_synced`.
pub async fn send_again(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    Path((station, session, review)): Path<(String, String, String)>,
    body: Result<Json<SendAgain>, JsonRejection>,
) -> Result<Json<ReviewSummary>, ApiError> {
    let id = writer(&board, &principal, &station, &session).await?;
    driver(&board, &principal, &id, &session).await?;
    let req = body_of(body)?;
    check_key(&req.attempt_key, "an attempt key")?;
    let command = prepare(&board, &principal, &id, &session).await?;
    let excerpts = cache::deny_list(&board, &id).await;
    let link_up = board.link.control_for(&id).is_some();
    let sent = board
        .store
        .review_send_again(AgainRequest {
            station: id,
            session,
            viewer: viewer_of(&principal),
            review_id: review,
            attempt_key: req.attempt_key,
            excerpts,
            command,
            link_up,
        })
        .await
        .map_err(api)?;
    sent_answer(&board, &sent);
    audit_send(&board, &principal, "review.retry", &sent).await;
    Ok(Json(sent.summary))
}

#[derive(Debug, Default, Deserialize)]
pub struct TurnQuery {
    pub from: Option<String>,
    pub to: Option<String>,
}

/// `GET /api/stations/{station}/sessions/{session}/turn-diff?from=&to=`: the
/// diff of the finalized turn with that checkpoint commit pair (the stable
/// selector of design 1). The same scope and member rules as the turn diff
/// by provider turn id.
///
/// # Errors
/// 400 `invalid_body` (a bad commit id); 404 `checkpoint_unavailable`,
/// `unknown_session`; 409 `station_unreachable`; 502 `station_error`; 504
/// `timeout`.
pub async fn turn_diff(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    Path((station, session)): Path<(String, String)>,
    Query(q): Query<TurnQuery>,
) -> Result<Json<TurnDiff>, ApiError> {
    let id = if matches!(principal, Principal::Member(..)) {
        reader(&board, &principal, &station, &session).await?
    } else {
        board
            .store
            .find_station(station.clone())
            .await
            .map_err(ApiError::internal)?
            .map(|r| r.station_id)
            .ok_or_else(|| {
                ApiError::new(
                    StatusCode::NOT_FOUND,
                    "unknown_station",
                    format!("no station is named `{station}`"),
                )
            })?
    };
    let snapshot = Snapshot::parse(&format!(
        "turn:{}..{}",
        q.from.unwrap_or_default(),
        q.to.unwrap_or_default()
    ))
    .ok_or_else(|| {
        invalid("`from` and `to` are checkpoint commit ids of 40 or 64 hex characters")
    })?;
    let Snapshot::Turn { from, to } = snapshot else {
        return Err(invalid("`from` and `to` name a turn"));
    };
    let member = matches!(principal, Principal::Member(..));
    let pair = CheckpointRef {
        from_commit: from,
        to_commit: to,
    };
    cache::turn_diff(&board, &id, &session, pair, member)
        .await
        .map(Json)
}
