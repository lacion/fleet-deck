//! The side chat of a session (R11.6, DL-166): people only, the agent never
//! reads it. The deck keeps the newest [`CHAT_KEEP`] messages of a session.
//! Also the check that a member may be handed the composer of a session in
//! mob mode.

use fleetdeck_proto::share::{
    CHAT_KEEP, CHAT_PAGE, ChatMessage, MAX_CHAT_CHARS, ShareKind, ShareRole,
};
use rusqlite::{OptionalExtension, params};

use super::members::scope_of;
use super::{DeckError, DeckStore, Result, call, to_i64, to_u64};

impl DeckStore {
    /// Adds a message to the side chat of a session. The caller checked who
    /// may write.
    ///
    /// # Errors
    /// [`DeckError::Invalid`] for an empty or too long text; a database error.
    pub async fn post_chat(
        &self,
        station: String,
        session: String,
        author: String,
        text: &str,
    ) -> Result<ChatMessage> {
        let text = text.trim().to_owned();
        if text.is_empty() || text.chars().count() > MAX_CHAT_CHARS {
            return Err(DeckError::Invalid(format!(
                "a chat message has 1 to {MAX_CHAT_CHARS} characters"
            )));
        }
        let now = self.now_ms();
        call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            let last: i64 = tx.query_row(
                "SELECT COALESCE(MAX(seq), 0) FROM session_chat
                 WHERE station_id = ?1 AND session_id = ?2",
                params![station, session],
                |r| r.get(0),
            )?;
            let seq = last + 1;
            tx.execute(
                "INSERT INTO session_chat (station_id, session_id, seq, author, text, at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![station, session, seq, author, text, now],
            )?;
            tx.execute(
                "DELETE FROM session_chat
                 WHERE station_id = ?1 AND session_id = ?2 AND seq <= ?3",
                params![station, session, seq - i64::from(CHAT_KEEP)],
            )?;
            tx.commit()?;
            Ok(ChatMessage {
                seq: to_u64(seq)?,
                author,
                text,
                at: now,
            })
        })
        .await
    }

    /// The messages of the side chat after `after` (0: from the start), oldest
    /// first, at most [`CHAT_PAGE`].
    ///
    /// # Errors
    /// A database error.
    pub async fn chat(
        &self,
        station: String,
        session: String,
        after: u64,
    ) -> Result<Vec<ChatMessage>> {
        let after = to_i64(after)?;
        call(&self.inner.db, move |conn| {
            let mut stmt = conn.prepare(
                "SELECT seq, author, text, at FROM (
                     SELECT seq, author, text, at FROM session_chat
                     WHERE station_id = ?1 AND session_id = ?2 AND seq > ?3
                     ORDER BY seq DESC LIMIT ?4
                 ) ORDER BY seq",
            )?;
            let rows = stmt
                .query_map(params![station, session, after, CHAT_PAGE], |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, i64>(3)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows.into_iter()
                .map(|(seq, author, text, at)| {
                    Ok(ChatMessage {
                        seq: to_u64(seq)?,
                        author,
                        text,
                        at,
                    })
                })
                .collect()
        })
        .await
    }

    /// True when a member with this display name has a live login in an
    /// active share of at least the collaborator role that covers the
    /// session: the person who may be handed the composer.
    ///
    /// # Errors
    /// A database error.
    pub async fn member_can_drive(
        &self,
        station: String,
        session: String,
        display_name: String,
    ) -> Result<bool> {
        let now = self.now_ms();
        let login_max_ms = self.knobs().login_max_ms;
        call(&self.inner.db, move |conn| {
            let mut stmt = conn.prepare(
                "SELECT s.kind, s.target, s.role FROM members m
                 JOIN shares s ON s.share_id = m.share_id
                 WHERE m.display_name = ?1 COLLATE NOCASE
                   AND s.revoked_at IS NULL AND s.expires_at > ?2
                   AND EXISTS (SELECT 1 FROM member_sessions ms
                               WHERE ms.member_id = m.member_id AND ms.expires_at > ?2
                                 AND ms.created_at > ?3)",
            )?;
            let rows = stmt
                .query_map(params![display_name, now, now - login_max_ms], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            for (kind, target, role) in rows {
                let (Some(kind), Some(role)) = (ShareKind::parse(&kind), ShareRole::parse(&role))
                else {
                    continue;
                };
                if role >= ShareRole::Collaborator
                    && scope_of(conn, kind, &target)?.covers(&station, &session)
                {
                    return Ok(true);
                }
            }
            Ok(false)
        })
        .await
    }

    /// True when the read model holds the session of the station.
    ///
    /// # Errors
    /// A database error.
    pub async fn session_known(&self, station: String, session: String) -> Result<bool> {
        call(&self.inner.db, move |conn| {
            Ok(conn
                .query_row(
                    "SELECT 1 FROM sessions WHERE station_id = ?1 AND session_id = ?2",
                    params![station, session],
                    |_| Ok(()),
                )
                .optional()?
                .is_some())
        })
        .await
    }
}

#[cfg(test)]
#[path = "chat_tests.rs"]
mod tests;
