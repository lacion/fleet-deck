//! Change and delete review comments (DL-179, design 2.2): lookup and
//! visibility first, then the author, then the state.

use fleetdeck_proto::StationId;
use fleetdeck_proto::review::{CommentPatch, CommentState, ReviewComment};
use rusqlite::params;

use super::review::{
    COLUMNS, Edited, Hide, Outcome, Viewer, bump, comment, find, flat, refuse, visible,
};
use super::{DeckError, DeckStore, call};

impl DeckStore {
    /// Changes the body and the resolved flag of a comment. Lookup and
    /// visibility first (`unknown_comment`), then the author (`not_author`),
    /// then the state (`not_draft`).
    ///
    /// # Errors
    /// The refusals above, and `invalid_body` for `resolved` on a reply.
    pub async fn review_edit(
        &self,
        station: StationId,
        session: String,
        viewer: Viewer,
        hide: Hide,
        id: String,
        patch: CommentPatch,
    ) -> Outcome<Edited> {
        let now = self.now_ms();
        let CommentPatch { body, resolved } = patch;
        let out = flat(
            call(&self.inner.db, move |conn| {
                let tx = conn.transaction()?;
                let Some(old) =
                    find(&tx, &station, &session, &id)?.filter(|c| visible(c, &viewer, &hide))
                else {
                    refuse!("unknown_comment", "no such comment");
                };
                let body_changed = body.is_some();
                if body_changed {
                    if !viewer.wrote(&old) {
                        refuse!("not_author", "only the author edits a comment");
                    }
                    if old.state != CommentState::Draft {
                        refuse!("not_draft", "a sent comment cannot be edited");
                    }
                }
                if resolved.is_some() && old.comment_id != old.thread_id {
                    refuse!("invalid_body", "only a top comment is resolved");
                }
                tx.execute(
                    "UPDATE review_comments SET body = COALESCE(?2, body),
                     resolved = COALESCE(?3, resolved), updated_at = ?4
                 WHERE comment_id = ?1",
                    params![old.comment_id, body, resolved.map(i64::from), now],
                )?;
                bump(&tx, &station, &session)?;
                let stored = find(&tx, &station, &session, &old.comment_id)?
                    .ok_or_else(|| DeckError::Corrupt("a changed comment is gone".to_owned()))?;
                tx.commit()?;
                Ok(Ok(Edited {
                    comment: stored,
                    body_changed,
                    resolved_changed: resolved.is_some(),
                }))
            })
            .await,
        )?;
        self.board_changed();
        Ok(out)
    }

    /// Deletes a draft of the viewer. A draft top comment takes its draft
    /// replies along when the same author wrote all of them (`thread_has_
    /// replies` otherwise). A sent comment is never deleted.
    ///
    /// # Errors
    /// `unknown_comment`, `not_author`, `not_draft`, `thread_has_replies`.
    pub async fn review_delete(
        &self,
        station: StationId,
        session: String,
        viewer: Viewer,
        hide: Hide,
        id: String,
    ) -> Outcome<ReviewComment> {
        let out = flat(
            call(&self.inner.db, move |conn| {
                let tx = conn.transaction()?;
                let Some(old) =
                    find(&tx, &station, &session, &id)?.filter(|c| visible(c, &viewer, &hide))
                else {
                    refuse!("unknown_comment", "no such comment");
                };
                if !viewer.wrote(&old) {
                    refuse!("not_author", "only the author deletes a comment");
                }
                if old.state != CommentState::Draft {
                    refuse!("not_draft", "a sent comment is never deleted");
                }
                let mut gone = vec![old.comment_id.clone()];
                if old.comment_id == old.thread_id {
                    let mut stmt = tx.prepare(&format!(
                        "SELECT {COLUMNS} FROM review_comments
                     WHERE station_id = ?1 AND session_id = ?2 AND thread_id = ?3
                       AND comment_id <> ?3"
                    ))?;
                    let replies: Vec<ReviewComment> = stmt
                        .query_map(params![station.as_str(), session, old.comment_id], comment)?
                        .collect::<rusqlite::Result<_>>()?;
                    drop(stmt);
                    if replies
                        .iter()
                        .any(|c| c.state != CommentState::Draft || !viewer.wrote(c))
                    {
                        refuse!(
                            "thread_has_replies",
                            "the thread has replies that are sent or by another author"
                        );
                    }
                    gone.extend(replies.into_iter().map(|c| c.comment_id));
                }
                for id in &gone {
                    tx.execute(
                        "DELETE FROM review_imports
                     WHERE station_id = ?1 AND session_id = ?2 AND comment_id = ?3",
                        params![station.as_str(), session, id],
                    )?;
                    tx.execute("DELETE FROM review_comments WHERE comment_id = ?1", [id])?;
                }
                bump(&tx, &station, &session)?;
                tx.commit()?;
                Ok(Ok(old))
            })
            .await,
        )?;
        self.board_changed();
        Ok(out)
    }
}
