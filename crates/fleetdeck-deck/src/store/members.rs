//! Local member accounts and their logins (R11, DL-154, DL-156, DL-166).
//!
//! A member logs in only through an invite: `POST /api/auth/join` redeems
//! the invite token of a share and the person picks a display name. The
//! deck makes (or finds, for the same display name and share) a member and
//! starts a member login: a random cookie value of which only the SHA-256
//! hash is stored, a CSRF token, and an expiry that never passes the expiry
//! of the share. A revoked or expired share ends every login of it.
//!
//! [`member_allows`] is the one place that decides what a member may do.

use std::collections::BTreeSet;

use fleetdeck_core::Secret;
use fleetdeck_proto::link::Scope;
use fleetdeck_proto::share::{MAX_DISPLAY_NAME_CHARS, ShareKind, ShareRole, Steering};
use rusqlite::{Connection, OptionalExtension, params};

use super::auth::{SESSION_MAX_MS, random_token, secret_hash};
use super::{DeckError, DeckStore, Result, call, new_id};

/// The id of a member.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MemberId(pub String);

/// The sessions that a share covers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShareScope {
    /// Every session (a board share).
    Board,
    /// These sessions: `(station_id, session_id)`.
    Sessions(BTreeSet<(String, String)>),
}

impl ShareScope {
    /// True when the share covers the session of the station (ids, not
    /// names).
    #[must_use]
    pub fn covers(&self, station: &str, session: &str) -> bool {
        match self {
            Self::Board => true,
            Self::Sessions(set) => set.contains(&(station.to_owned(), session.to_owned())),
        }
    }
}

/// What a member login may do: the share, the role and the covered
/// sessions. Loaded again for every request, so a revoke or a change of a
/// space takes effect at once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShareGrant {
    pub member_id: MemberId,
    pub share_id: String,
    pub display_name: String,
    pub kind: ShareKind,
    pub target: String,
    pub role: ShareRole,
    pub steering: Steering,
    pub scope: ShareScope,
    /// The SHA-256 of the cookie value.
    pub session_hash: String,
    pub csrf: String,
    /// The end of this login (never after the end of the share).
    pub expires_at: i64,
}

impl ShareGrant {
    /// The audit actor name: `member:<name>`.
    #[must_use]
    pub fn actor(&self) -> String {
        format!("member:{}", self.display_name)
    }

    /// The API scopes that the role maps to.
    #[must_use]
    pub fn scopes(&self) -> BTreeSet<Scope> {
        let mut set = BTreeSet::from([Scope::Read]);
        if self.role >= ShareRole::Collaborator {
            set.insert(Scope::Drive);
        }
        if self.role >= ShareRole::Approver {
            set.insert(Scope::Approve);
        }
        set
    }
}

/// What a member does to a shared session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemberAction {
    /// Read the session: cards, timeline, files, diffs, artifacts.
    Read,
    /// The side chat and the presence of the session.
    Chat,
    Prompt,
    Steer,
    FollowUp,
    Interrupt,
    /// The answer to a question of the agent (not a tool approval).
    AnswerQuestion,
    /// The answer to a tool approval.
    AnswerApproval,
    /// A mail to the session.
    Mail,
    /// Ask to drive a session in mob mode, and hand the composer on.
    Drive,
}

impl MemberAction {
    /// The lowest role that may do the action.
    #[must_use]
    pub const fn min_role(self) -> ShareRole {
        match self {
            Self::Read | Self::Chat => ShareRole::Viewer,
            Self::Prompt
            | Self::Steer
            | Self::FollowUp
            | Self::Interrupt
            | Self::AnswerQuestion
            | Self::Mail
            | Self::Drive => ShareRole::Collaborator,
            Self::AnswerApproval => ShareRole::Approver,
        }
    }

    /// True for the actions that steer the agent: in mob mode only the
    /// driver sends them. A mail reaches the agent as a follow-up or a steer,
    /// so it counts.
    #[must_use]
    pub const fn steers(self) -> bool {
        matches!(
            self,
            Self::Prompt | Self::Steer | Self::FollowUp | Self::Interrupt | Self::Mail
        )
    }

    /// The action of a command method of the board API (`session.prompt`,
    /// ...). `approval.answer` has none: a question and a tool approval need
    /// different roles, so the caller picks.
    #[must_use]
    pub fn of_method(method: &str) -> Option<Self> {
        Some(match method {
            "session.prompt" => Self::Prompt,
            "session.steer" => Self::Steer,
            "session.follow_up" => Self::FollowUp,
            "session.interrupt" => Self::Interrupt,
            _ => return None,
        })
    }
}

/// The member policy (R11.2, DL-166): true when the member of `grant` may do
/// `action` to `session` of `station` (ids, not names). The role must hold the
/// action and the share must cover the session. A member never starts or
/// stops a session, changes the mode or the model, opens a shell, or touches
/// the settings of the deck: those have no action here, so no caller can ask
/// for them.
#[must_use]
pub fn member_allows(
    grant: &ShareGrant,
    action: MemberAction,
    station: &str,
    session: &str,
) -> bool {
    grant.role >= action.min_role() && grant.scope.covers(station, session)
}

/// A new member login: the cookie value goes to the browser once.
#[derive(Debug)]
pub struct NewMemberLogin {
    pub cookie: Secret<String>,
    pub grant: ShareGrant,
}

/// A display name: trimmed, 1 to [`MAX_DISPLAY_NAME_CHARS`] characters, no
/// control character.
///
/// # Errors
/// [`DeckError::Invalid`].
pub fn check_display_name(name: &str) -> Result<String> {
    let name = name.trim();
    if name.is_empty()
        || name.chars().count() > MAX_DISPLAY_NAME_CHARS
        || name.chars().any(char::is_control)
    {
        return Err(DeckError::Invalid(format!(
            "a display name has 1 to {MAX_DISPLAY_NAME_CHARS} printable characters"
        )));
    }
    Ok(name.to_owned())
}

/// The covered sessions of a share.
pub(crate) fn scope_of(conn: &Connection, kind: ShareKind, target: &str) -> Result<ShareScope> {
    Ok(match kind {
        ShareKind::Board => ShareScope::Board,
        ShareKind::Session => {
            let mut set = BTreeSet::new();
            if let Some((station, session)) = target.split_once('/')
                && !station.is_empty()
                && !session.is_empty()
            {
                set.insert((station.to_owned(), session.to_owned()));
            }
            ShareScope::Sessions(set)
        }
        ShareKind::Space => {
            let mut stmt = conn
                .prepare("SELECT station_id, session_id FROM space_sessions WHERE space_id = ?1")?;
            let rows = stmt.query_map([target], |r| Ok((r.get(0)?, r.get(1)?)))?;
            ShareScope::Sessions(rows.collect::<rusqlite::Result<_>>()?)
        }
    })
}

type GrantRow = (
    String,
    String,
    String,
    i64,
    String,
    String,
    String,
    String,
    String,
);

fn parse_grant(row: GrantRow, hash: &str, conn: &Connection) -> Result<ShareGrant> {
    let (member_id, display_name, csrf, expires_at, share_id, kind, target, role, steering) = (
        row.0, row.1, row.2, row.3, row.4, row.5, row.6, row.7, row.8,
    );
    let corrupt = |what: &str, v: &str| DeckError::Corrupt(format!("share {what} `{v}`"));
    let kind = ShareKind::parse(&kind).ok_or_else(|| corrupt("kind", &kind))?;
    let scope = scope_of(conn, kind, &target)?;
    Ok(ShareGrant {
        member_id: MemberId(member_id),
        share_id,
        display_name,
        kind,
        target,
        role: ShareRole::parse(&role).ok_or_else(|| corrupt("role", &role))?,
        steering: Steering::parse(&steering).ok_or_else(|| corrupt("steering", &steering))?,
        scope,
        session_hash: hash.to_owned(),
        csrf,
        expires_at,
    })
}

/// The live grant of a member login, or `None` when the login or its share
/// ended.
fn load_grant(conn: &Connection, hash: &str, now: i64) -> Result<Option<ShareGrant>> {
    let row = conn
        .query_row(
            "SELECT ms.member_id, m.display_name, ms.csrf_token, ms.expires_at,
                    s.share_id, s.kind, s.target, s.role, s.steering
             FROM member_sessions ms
             JOIN members m ON m.member_id = ms.member_id
             JOIN shares s ON s.share_id = ms.share_id
             WHERE ms.session_hash = ?1 AND ms.expires_at > ?2
               AND s.revoked_at IS NULL AND s.expires_at > ?2",
            params![hash, now],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                    r.get(7)?,
                    r.get(8)?,
                ))
            },
        )
        .optional()?;
    row.map(|row| parse_grant(row, hash, conn)).transpose()
}

/// True while the member login and its share are alive.
pub(crate) fn grant_alive(conn: &Connection, hash: &str, now: i64) -> rusqlite::Result<bool> {
    Ok(conn
        .query_row(
            "SELECT 1 FROM member_sessions ms
             JOIN shares s ON s.share_id = ms.share_id
             WHERE ms.session_hash = ?1 AND ms.expires_at > ?2
               AND s.revoked_at IS NULL AND s.expires_at > ?2",
            params![hash, now],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

impl DeckStore {
    /// Redeems an invite and starts a member login. `Ok(None)` for a token
    /// that is unknown, used up, expired or of a revoked share. The member of
    /// the same display name (case-insensitive) in the same share is reused.
    ///
    /// # Errors
    /// [`DeckError::Invalid`] for a bad display name; a database error.
    pub async fn join(
        &self,
        token: &Secret<String>,
        display_name: &str,
    ) -> Result<Option<NewMemberLogin>> {
        let name = check_display_name(display_name)?;
        let invite_hash = secret_hash(token.expose());
        let cookie = format!("fdmem1_{}", random_token()?);
        let csrf = random_token()?;
        let cookie_hash = secret_hash(&cookie);
        let member_id: String = new_id(self.inner.ids.as_ref());
        let now = self.now_ms();
        let found = call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            tx.execute("DELETE FROM member_sessions WHERE expires_at <= ?1", [now])?;
            let share_id: Option<String> = tx
                .query_row(
                    "UPDATE invites SET uses_left = uses_left - 1
                     WHERE token_hash = ?1 AND uses_left > 0 AND expires_at > ?2
                       AND share_id IN (SELECT share_id FROM shares
                                        WHERE revoked_at IS NULL AND expires_at > ?2)
                     RETURNING share_id",
                    params![invite_hash, now],
                    |r| r.get(0),
                )
                .optional()?;
            let Some(share_id) = share_id else {
                return Ok(None);
            };
            let share_expires: i64 = tx.query_row(
                "SELECT expires_at FROM shares WHERE share_id = ?1",
                [&share_id],
                |r| r.get(0),
            )?;
            let existing: Option<String> = tx
                .query_row(
                    "SELECT member_id FROM members
                     WHERE share_id = ?1 AND display_name = ?2 COLLATE NOCASE",
                    params![share_id, name],
                    |r| r.get(0),
                )
                .optional()?;
            let member = match existing {
                Some(id) => id,
                None => {
                    tx.execute(
                        "INSERT INTO members (member_id, share_id, display_name, created_at)
                         VALUES (?1, ?2, ?3, ?4)",
                        params![member_id, share_id, name, now],
                    )?;
                    member_id
                }
            };
            let expires_at = share_expires.min(now.saturating_add(SESSION_MAX_MS));
            tx.execute(
                "INSERT INTO member_sessions
                 (session_hash, member_id, share_id, csrf_token, created_at, expires_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![cookie_hash, member, share_id, csrf, now, expires_at],
            )?;
            let grant = load_grant(&tx, &cookie_hash, now)?
                .ok_or_else(|| DeckError::Corrupt("a new member login is gone".to_owned()))?;
            tx.commit()?;
            Ok(Some(grant))
        })
        .await?;
        Ok(found.map(|grant| NewMemberLogin {
            cookie: Secret::new(cookie),
            grant,
        }))
    }

    /// The live member login of a cookie value.
    ///
    /// # Errors
    /// A database error.
    pub async fn member_login(&self, cookie: &Secret<String>) -> Result<Option<ShareGrant>> {
        let hash = secret_hash(cookie.expose());
        let now = self.now_ms();
        call(&self.inner.db, move |conn| load_grant(conn, &hash, now)).await
    }

    /// The same login with the sessions that its share covers now.
    ///
    /// # Errors
    /// A database error.
    pub async fn member_refresh(&self, grant: &ShareGrant) -> Result<Option<ShareGrant>> {
        let hash = grant.session_hash.clone();
        let now = self.now_ms();
        call(&self.inner.db, move |conn| load_grant(conn, &hash, now)).await
    }

    /// Ends one member login. True when it existed.
    ///
    /// # Errors
    /// A database error.
    pub async fn end_member_login(&self, session_hash: String) -> Result<bool> {
        call(&self.inner.db, move |conn| {
            Ok(conn.execute(
                "DELETE FROM member_sessions WHERE session_hash = ?1",
                [session_hash],
            )? == 1)
        })
        .await
    }
}

#[cfg(test)]
#[path = "members_tests.rs"]
mod tests;
