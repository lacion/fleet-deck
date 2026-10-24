//! Review comments on the Changes diff (DL-179, design `review-comments.md`):
//! the request and answer shapes of the routes under
//! `/api/sessions/{station}/{session}/review`. The board's TypeScript types
//! come from here.
//!
//! A comment sits on a line or a range of lines of one file of one diff
//! snapshot of a session. Comments form threads. A review is a set of
//! drafts of one author that the author sends to the agent in one follow-up.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// The most comments of one session (drafts and sent, all authors).
pub const MAX_COMMENTS_PER_SESSION: usize = 500;
/// The most comments in one review.
pub const MAX_COMMENTS_PER_REVIEW: usize = 50;
/// The most reviews that one session ever holds.
pub const MAX_REVIEWS_PER_SESSION: usize = 200;
/// The most send attempts of one review.
pub const MAX_ATTEMPTS: u32 = 5;
/// The longest comment body in characters.
pub const MAX_BODY_CHARS: usize = 8000;
/// The most lines of one commented range.
pub const MAX_RANGE_LINES: u32 = 200;
/// The longest anchor text in bytes.
pub const MAX_ANCHOR_BYTES: usize = 4096;
/// The longest follow-up text of one review in bytes.
pub const MAX_REVIEW_TEXT_BYTES: usize = 64 * 1024;
/// The longest stored path in bytes.
pub const MAX_PATH_BYTES: usize = 1024;
/// The reviews that one `GET` answer holds at most.
pub const REVIEWS_PAGE: usize = 50;

/// The side of a diff that a comment sits on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "review/")]
pub enum ReviewSide {
    /// The new file (context and added lines).
    New,
    /// The old file (context and removed lines).
    Old,
}

impl ReviewSide {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::New => "new",
            Self::Old => "old",
        }
    }
}

/// Where the anchor text of a comment comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "review/")]
pub enum AnchorKind {
    /// Lines of a diff, read by the deck.
    Diff,
    /// An imported local note without an excerpt.
    Imported,
}

/// Who wrote a comment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "review/")]
pub enum AuthorKind {
    Owner,
    Member,
}

/// A comment is a draft of its author, or sent in a review.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "review/")]
pub enum CommentState {
    Draft,
    Sent,
}

/// How far the follow-up of a review got.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "review/")]
pub enum Delivery {
    /// The command is stored and not yet decided by the station.
    Pending,
    /// The station accepted the follow-up.
    Delivered,
    /// The station did not accept it (proven): "Send again" is possible.
    Failed,
    /// The station may have accepted it: never sent again.
    Unknown,
}

impl Delivery {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Delivered => "delivered",
            Self::Failed => "failed",
            Self::Unknown => "unknown",
        }
    }
}

/// One comment, as the caller may see it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "review/")]
pub struct ReviewComment {
    pub comment_id: String,
    /// `c-` and base32; immutable; the id that people write in the chat.
    pub short_id: String,
    pub thread_id: String,
    /// `spawn:<base oid>` or `turn:<from>..<to>`.
    pub snapshot: String,
    /// The path of the commented side.
    pub path: String,
    /// The path of the other side of a rename or a copy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub other_path: Option<String>,
    pub side: ReviewSide,
    pub line_start: u32,
    pub line_end: u32,
    /// The commented lines, read by the deck from the diff.
    pub anchor_text: String,
    pub anchor_kind: AnchorKind,
    pub body: String,
    pub author_kind: AuthorKind,
    /// The member name; absent for the owner.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub author_name: Option<String>,
    pub state: CommentState,
    pub resolved: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub review_id: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

/// A thread: a top comment and its replies, oldest first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "review/")]
pub struct ReviewThread {
    pub thread_id: String,
    pub comments: Vec<ReviewComment>,
}

/// One sent review.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "review/")]
pub struct ReviewSummary {
    pub review_id: String,
    /// `r-` and base32.
    pub short_id: String,
    /// `owner` or `member:<name>`.
    pub principal: String,
    pub delivery: Delivery,
    pub attempts: u32,
    /// The input of the station that the accepted follow-up created.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub input_id: Option<String>,
    /// Why the delivery failed or is unknown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub reason: Option<String>,
    pub sent_at: i64,
    /// The short ids of the comments of the review that the caller may see.
    pub comment_ids: Vec<String>,
    pub command_id: String,
}

/// `GET /api/sessions/{station}/{session}/review`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "review/")]
pub struct ReviewThreads {
    pub threads: Vec<ReviewThread>,
    /// The newest [`REVIEWS_PAGE`] reviews, newest first.
    pub reviews: Vec<ReviewSummary>,
    /// The `review_rev` of the session now.
    pub rev: u32,
    /// Older reviews exist (`reviews_before` = the last `review_id`).
    pub more_reviews: bool,
}

/// `POST …/review/comments`. The client never sends the anchor text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "review/")]
pub struct NewComment {
    pub snapshot: String,
    pub path: String,
    pub side: ReviewSide,
    pub line_start: u32,
    pub line_end: u32,
    pub body: String,
    /// A reply: the id of the top comment. The reply copies its snapshot,
    /// path, side, range and anchor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub thread_id: Option<String>,
    /// An imported note (64 lower-case hex characters).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub import_key: Option<String>,
}

/// `PATCH …/review/comments/{id}`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "review/")]
pub struct CommentPatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub body: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub resolved: Option<bool>,
}

/// `POST …/review/send`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "review/")]
pub struct SendReview {
    /// The client key of this send; a repeat returns the stored result.
    pub send_key: String,
    /// 1 to [`MAX_COMMENTS_PER_REVIEW`] distinct comments of the caller.
    pub comment_ids: Vec<String>,
}

/// `POST …/review/reviews/{review_id}/send-again`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "review/")]
pub struct SendAgain {
    /// The client key of this attempt; a repeat returns the stored result.
    pub attempt_key: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_comment_refuses_unknown_fields_such_as_an_anchor() {
        let json = serde_json::json!({
            "snapshot": "spawn:abc", "path": "a.rs", "side": "new",
            "line_start": 1, "line_end": 1, "body": "x", "anchor_text": "forged"
        });
        assert!(serde_json::from_value::<NewComment>(json).is_err());
        let ok = serde_json::json!({
            "snapshot": "spawn:abc", "path": "a.rs", "side": "new",
            "line_start": 1, "line_end": 1, "body": "x"
        });
        let c: NewComment = serde_json::from_value(ok).unwrap();
        assert_eq!((c.thread_id, c.import_key), (None, None));
    }

    #[test]
    fn the_enums_use_snake_case_names() {
        assert_eq!(serde_json::to_value(ReviewSide::Old).unwrap(), "old");
        assert_eq!(serde_json::to_value(Delivery::Unknown).unwrap(), "unknown");
        assert_eq!(
            serde_json::to_value(AnchorKind::Imported).unwrap(),
            "imported"
        );
        assert!(serde_json::from_str::<ReviewSide>("\"left\"").is_err());
    }

    #[test]
    fn a_patch_and_the_bodies_refuse_unknown_fields() {
        assert!(serde_json::from_str::<CommentPatch>(r#"{"body":"x","author":"y"}"#).is_err());
        assert!(
            serde_json::from_str::<SendReview>(r#"{"send_key":"k","comment_ids":[],"x":1}"#)
                .is_err()
        );
        assert!(serde_json::from_str::<SendAgain>(r#"{"attempt_key":"k","x":1}"#).is_err());
        let p: CommentPatch = serde_json::from_str("{}").unwrap();
        assert_eq!(p, CommentPatch::default());
    }

    #[test]
    fn a_card_without_a_review_rev_and_a_run_without_an_input_id_stay_as_they_were() {
        let comment = ReviewComment {
            comment_id: "01".into(),
            short_id: "c-abc234".into(),
            thread_id: "01".into(),
            snapshot: "spawn:x".into(),
            path: "a".into(),
            other_path: None,
            side: ReviewSide::New,
            line_start: 1,
            line_end: 1,
            anchor_text: String::new(),
            anchor_kind: AnchorKind::Imported,
            body: "b".into(),
            author_kind: AuthorKind::Owner,
            author_name: None,
            state: CommentState::Draft,
            resolved: false,
            review_id: None,
            created_at: 1,
            updated_at: 1,
        };
        let v = serde_json::to_value(&comment).unwrap();
        assert!(v.get("other_path").is_none() && v.get("author_name").is_none());
        assert!(v.get("review_id").is_none());
    }
}
