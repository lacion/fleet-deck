//! The display authentication generation (R13 §5E item 2): link redemption,
//! pairing claim and session issue are fenced by `displays.auth_generation`,
//! each in the same transaction as its write.
//!
//! - Every display link, approved pairing and display session stores the
//!   generation at its creation.
//! - A session is issued by `INSERT ... SELECT ... WHERE
//!   displays.auth_generation = ?`: a replacement or a revoke between the
//!   check of a link or a pairing and the issue makes the insert write
//!   nothing, and the caller answers `401 login_required`.
//! - A replacement (a pairing claim) moves the generation up and deletes
//!   every session, unused link and approved-but-unclaimed pairing of the
//!   display in the same transaction.

use fleetdeck_core::Secret;
use rusqlite::{Connection, OptionalExtension, params};

use super::auth::{random_token, secret_hash};
use super::displays::{SESSION_PREFIX, delete_logins, load};
use super::{DeckError, DeckStore, Result, call};
use crate::knobs::DeckKnobs;

/// A display session that was just issued.
#[derive(Debug)]
pub struct IssuedSession {
    pub display_id: String,
    /// The cookie value (`fdisp1_...`); it is shown once.
    pub cookie: Secret<String>,
    /// The absolute expiry, in Unix ms.
    pub expires_at: i64,
}

/// Issues a session for `display_id` when its generation is still
/// `generation`. `None` when the display is gone, revoked or at another
/// generation. The caller holds the transaction and has read `knobs` once.
pub(super) fn issue_fenced(
    conn: &Connection,
    display_id: &str,
    generation: i64,
    now: i64,
    knobs: &DeckKnobs,
) -> Result<Option<IssuedSession>> {
    let cookie = format!("{SESSION_PREFIX}{}", random_token()?);
    let hash = secret_hash(&cookie);
    let expires_at = now + knobs.display_max_ms;
    conn.execute(
        "DELETE FROM display_sessions WHERE expires_at <= ?1 OR last_seen <= ?2",
        params![now, now - knobs.display_idle_ms],
    )?;
    let n = conn.execute(
        "INSERT INTO display_sessions
         (session_hash, display_id, created_at, last_seen, expires_at, auth_generation)
         SELECT ?1, d.display_id, ?3, ?3, ?4, d.auth_generation FROM displays d
         WHERE d.display_id = ?2 AND d.revoked_at IS NULL AND d.auth_generation = ?5",
        params![hash, display_id, now, expires_at, generation],
    )?;
    Ok((n == 1).then(|| IssuedSession {
        display_id: display_id.to_owned(),
        cookie: Secret::new(cookie),
        expires_at,
    }))
}

/// A replacement of the logins of a display: the generation moves up and
/// every session, unused link and pairing is deleted. Returns the new
/// generation. The caller holds the transaction.
pub(super) fn replace_logins(conn: &Connection, display_id: &str) -> Result<i64> {
    conn.execute(
        "UPDATE displays SET auth_generation = auth_generation + 1 WHERE display_id = ?1",
        params![display_id],
    )?;
    delete_logins(conn, display_id)?;
    Ok(conn.query_row(
        "SELECT auth_generation FROM displays WHERE display_id = ?1",
        params![display_id],
        |r| r.get(0),
    )?)
}

impl DeckStore {
    /// Uses a display link: the link is consumed and, when its generation is
    /// still the generation of its display, a session is issued, all in one
    /// transaction. `None` for an unknown, expired or used link, for a
    /// revoked display and for a link of an older generation.
    ///
    /// # Errors
    /// A database error.
    pub async fn redeem_display_link(
        &self,
        token: &Secret<String>,
    ) -> Result<Option<IssuedSession>> {
        let hash = secret_hash(token.expose());
        let now = self.now_ms();
        let knobs = self.knobs();
        call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            let link: Option<(String, i64)> = tx
                .query_row(
                    "DELETE FROM display_links WHERE token_hash = ?1 AND expires_at > ?2
                     RETURNING display_id, auth_generation",
                    params![hash, now],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            let issued = match link {
                Some((id, generation)) => issue_fenced(&tx, &id, generation, now, &knobs)?,
                None => None,
            };
            tx.commit()?;
            Ok(issued)
        })
        .await
    }

    /// Starts a display session for a display at its current generation.
    /// Returns the cookie value (`fdisp1_...`, once) and the absolute expiry.
    ///
    /// # Errors
    /// [`DeckError::Invalid`] for a display that is unknown or revoked; a
    /// database error.
    pub async fn create_display_session(
        &self,
        display_id: String,
    ) -> Result<(Secret<String>, i64)> {
        let now = self.now_ms();
        let knobs = self.knobs();
        call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            let Some(display) = load(&tx, &display_id)? else {
                return Err(DeckError::Invalid("no such display".to_owned()));
            };
            let issued = issue_fenced(&tx, &display_id, display.auth_generation, now, &knobs)?
                .ok_or_else(|| DeckError::Invalid("no such display".to_owned()))?;
            tx.commit()?;
            Ok((issued.cookie, issued.expires_at))
        })
        .await
    }
}
