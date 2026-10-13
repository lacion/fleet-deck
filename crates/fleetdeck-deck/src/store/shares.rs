//! Shares, invites, spaces and the mob-mode driver (R11, DL-154, DL-166).
//!
//! The owner makes a share of a session, of a space (a named set of
//! sessions) or of the board, with a role, a steering mode and an expiry.
//! A share has one invite token of N uses; only its SHA-256 is stored. A
//! revoke ends the share, its invite and every member login of it at once.
//!
//! In mob steering one driver at a time sends the steering commands of a
//! session; the others ask to drive, and the driver or the owner hands the
//! composer on.

use std::collections::BTreeMap;

use fleetdeck_core::Secret;
use fleetdeck_proto::board::SessionKey;
use fleetdeck_proto::event::{EventBody, RequestKind};
use fleetdeck_proto::share::{
    DEFAULT_INVITE_USES, DEFAULT_SHARE_DAYS, DriverState, MAX_INVITE_USES, MAX_SHARE_DAYS,
    MAX_SPACE_NAME_CHARS, ShareInfo, ShareKind, ShareRole, SpaceView, Steering,
};
use fleetdeck_proto::{SessionId, StationId};
use rusqlite::{Connection, OptionalExtension, params};

use super::auth::{random_token, secret_hash};
use super::{DeckError, DeckStore, Result, call, new_id};

const DAY_MS: i64 = 24 * 60 * 60 * 1000;
const INVITE_PREFIX: &str = "fdjoin1_";

/// A new share (the owner's `POST /api/shares`, checked).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewShareRow {
    pub kind: ShareKind,
    /// `<station_id>/<session_id>`, the space id, or empty.
    pub target: String,
    pub role: ShareRole,
    pub steering: Steering,
    pub expires_in_days: Option<u32>,
    pub uses: Option<u32>,
}

/// A created share and its invite token (shown once).
#[derive(Debug)]
pub struct CreatedShare {
    pub share_id: String,
    pub token: Secret<String>,
    pub expires_at: i64,
}

fn share_view(conn: &Connection, share_id: &str, now: i64) -> Result<Option<ShareInfo>> {
    let row = conn
        .query_row(
            "SELECT kind, target, role, steering, expires_at, revoked_at, created_at
             FROM shares WHERE share_id = ?1",
            [share_id],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, i64>(4)?,
                    r.get::<_, Option<i64>>(5)?,
                    r.get::<_, i64>(6)?,
                ))
            },
        )
        .optional()?;
    let Some((kind, target, role, steering, expires_at, revoked_at, created_at)) = row else {
        return Ok(None);
    };
    let corrupt = |what: &str, v: &str| DeckError::Corrupt(format!("share {what} `{v}`"));
    let uses_left: i64 = conn.query_row(
        "SELECT COALESCE(SUM(uses_left), 0) FROM invites
         WHERE share_id = ?1 AND expires_at > ?2",
        params![share_id, now],
        |r| r.get(0),
    )?;
    let mut stmt =
        conn.prepare("SELECT display_name FROM members WHERE share_id = ?1 ORDER BY created_at")?;
    let members = stmt
        .query_map([share_id], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(Some(ShareInfo {
        share_id: share_id.to_owned(),
        kind: ShareKind::parse(&kind).ok_or_else(|| corrupt("kind", &kind))?,
        target,
        role: ShareRole::parse(&role).ok_or_else(|| corrupt("role", &role))?,
        steering: Steering::parse(&steering).ok_or_else(|| corrupt("steering", &steering))?,
        expires_at,
        revoked_at,
        created_at,
        uses_left: if revoked_at.is_some() {
            0
        } else {
            u32::try_from(uses_left).unwrap_or(u32::MAX)
        },
        members,
    }))
}

fn driver_state(conn: &Connection, station: &str, session: &str) -> Result<DriverState> {
    let driver: Option<(String, i64)> = conn
        .query_row(
            "SELECT member_or_owner, since FROM drivers
             WHERE station_id = ?1 AND session_id = ?2",
            params![station, session],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let mut stmt = conn.prepare(
        "SELECT requester FROM drive_requests
         WHERE station_id = ?1 AND session_id = ?2 ORDER BY at, requester",
    )?;
    let requests = stmt
        .query_map(params![station, session], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(DriverState {
        since: driver.as_ref().map(|d| d.1),
        driver: driver.map(|d| d.0),
        requests,
    })
}

impl DeckStore {
    /// Makes a share with its invite. The caller checked `target`.
    ///
    /// # Errors
    /// [`DeckError::Invalid`] for a lifetime or a number of uses out of range;
    /// a database error.
    pub async fn create_share(&self, new: NewShareRow) -> Result<CreatedShare> {
        let days = new.expires_in_days.unwrap_or(DEFAULT_SHARE_DAYS);
        if !(1..=MAX_SHARE_DAYS).contains(&days) {
            return Err(DeckError::Invalid(format!(
                "a share lives for 1 to {MAX_SHARE_DAYS} days"
            )));
        }
        let uses = new.uses.unwrap_or(DEFAULT_INVITE_USES);
        if !(1..=MAX_INVITE_USES).contains(&uses) {
            return Err(DeckError::Invalid(format!(
                "an invite has 1 to {MAX_INVITE_USES} uses"
            )));
        }
        let token = format!("{INVITE_PREFIX}{}", random_token()?);
        let hash = secret_hash(&token);
        let share_id: String = new_id(self.inner.ids.as_ref());
        let now = self.now_ms();
        let expires_at = now.saturating_add(i64::from(days) * DAY_MS);
        let id = share_id.clone();
        call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            tx.execute(
                "INSERT INTO shares
                 (share_id, kind, target, role, steering, expires_at, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    id,
                    new.kind.as_str(),
                    new.target,
                    new.role.as_str(),
                    new.steering.as_str(),
                    expires_at,
                    now
                ],
            )?;
            tx.execute(
                "INSERT INTO invites (token_hash, share_id, uses_left, expires_at)
                 VALUES (?1, ?2, ?3, ?4)",
                params![hash, id, uses, expires_at],
            )?;
            tx.commit()?;
            Ok(())
        })
        .await?;
        Ok(CreatedShare {
            share_id,
            token: Secret::new(token),
            expires_at,
        })
    }

    /// Every share, newest first.
    ///
    /// # Errors
    /// A database error.
    pub async fn shares(&self) -> Result<Vec<ShareInfo>> {
        let now = self.now_ms();
        call(&self.inner.db, move |conn| {
            let mut stmt = conn
                .prepare("SELECT share_id FROM shares ORDER BY created_at DESC, share_id DESC")?;
            let ids = stmt
                .query_map([], |r| r.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            let mut out = Vec::with_capacity(ids.len());
            for id in ids {
                out.extend(share_view(conn, &id, now)?);
            }
            Ok(out)
        })
        .await
    }

    /// Revokes a share: its invite and every member login of it end at
    /// once. `None` for an unknown share; a second revoke changes nothing.
    ///
    /// # Errors
    /// A database error.
    pub async fn revoke_share(&self, share_id: String) -> Result<Option<ShareInfo>> {
        let now = self.now_ms();
        call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            tx.execute(
                "UPDATE shares SET revoked_at = ?2 WHERE share_id = ?1 AND revoked_at IS NULL",
                params![share_id, now],
            )?;
            tx.execute("DELETE FROM invites WHERE share_id = ?1", [&share_id])?;
            tx.execute(
                "DELETE FROM member_sessions WHERE share_id = ?1",
                [&share_id],
            )?;
            let view = share_view(&tx, &share_id, now)?;
            tx.commit()?;
            Ok(view)
        })
        .await
    }

    /// Makes a space.
    ///
    /// # Errors
    /// [`DeckError::Invalid`] for a bad name; a database error.
    pub async fn create_space(&self, name: &str) -> Result<SpaceView> {
        let name = name.trim().to_owned();
        if name.is_empty()
            || name.chars().count() > MAX_SPACE_NAME_CHARS
            || name.chars().any(char::is_control)
        {
            return Err(DeckError::Invalid(format!(
                "a space name has 1 to {MAX_SPACE_NAME_CHARS} printable characters"
            )));
        }
        let space_id: String = new_id(self.inner.ids.as_ref());
        let now = self.now_ms();
        let (id, n) = (space_id.clone(), name.clone());
        call(&self.inner.db, move |conn| {
            conn.execute(
                "INSERT INTO spaces (space_id, name, created_at) VALUES (?1, ?2, ?3)",
                params![id, n, now],
            )?;
            Ok(())
        })
        .await?;
        Ok(SpaceView {
            space_id,
            name,
            sessions: Vec::new(),
        })
    }

    /// Every space with its sessions, by name.
    ///
    /// # Errors
    /// A database error.
    pub async fn spaces(&self) -> Result<Vec<SpaceView>> {
        call(&self.inner.db, |conn| {
            let mut spaces: BTreeMap<String, SpaceView> = BTreeMap::new();
            let mut stmt =
                conn.prepare("SELECT space_id, name FROM spaces ORDER BY name, space_id")?;
            let rows = stmt
                .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            let order: Vec<String> = rows.iter().map(|r| r.0.clone()).collect();
            for (space_id, name) in rows {
                spaces.insert(
                    space_id.clone(),
                    SpaceView {
                        space_id,
                        name,
                        sessions: Vec::new(),
                    },
                );
            }
            let mut stmt = conn.prepare(
                "SELECT space_id, station_id, session_id FROM space_sessions
                 ORDER BY station_id, session_id",
            )?;
            let rows = stmt
                .query_map([], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            for (space, station, session) in rows {
                if let Some(s) = spaces.get_mut(&space) {
                    s.sessions.push(SessionKey {
                        station_id: StationId::from(station),
                        session_id: SessionId::from(session),
                    });
                }
            }
            Ok(order
                .into_iter()
                .filter_map(|id| spaces.remove(&id))
                .collect())
        })
        .await
    }

    /// True when the space exists.
    ///
    /// # Errors
    /// A database error.
    pub async fn space_exists(&self, space_id: String) -> Result<bool> {
        call(&self.inner.db, move |conn| {
            Ok(conn
                .query_row(
                    "SELECT 1 FROM spaces WHERE space_id = ?1",
                    [space_id],
                    |_| Ok(()),
                )
                .optional()?
                .is_some())
        })
        .await
    }

    /// The name of a space.
    ///
    /// # Errors
    /// A database error.
    pub async fn space_name(&self, space_id: String) -> Result<Option<String>> {
        call(&self.inner.db, move |conn| {
            Ok(conn
                .query_row(
                    "SELECT name FROM spaces WHERE space_id = ?1",
                    [space_id],
                    |r| r.get(0),
                )
                .optional()?)
        })
        .await
    }

    /// Adds a session to a space (a repeat changes nothing). The caller
    /// checked that the session exists. `None` for an unknown space.
    ///
    /// # Errors
    /// A database error.
    pub async fn add_space_session(
        &self,
        space_id: String,
        station: StationId,
        session: SessionId,
    ) -> Result<Option<SpaceView>> {
        let wanted = space_id.clone();
        let all = call(&self.inner.db, move |conn| {
            let known = conn
                .query_row(
                    "SELECT 1 FROM spaces WHERE space_id = ?1",
                    [&space_id],
                    |_| Ok(()),
                )
                .optional()?
                .is_some();
            if !known {
                return Ok(false);
            }
            conn.execute(
                "INSERT OR IGNORE INTO space_sessions (space_id, station_id, session_id)
                 VALUES (?1, ?2, ?3)",
                params![space_id, station.as_str(), session.as_str()],
            )?;
            Ok(true)
        })
        .await?;
        if !all {
            return Ok(None);
        }
        Ok(self
            .spaces()
            .await?
            .into_iter()
            .find(|s| s.space_id == wanted))
    }

    /// True when an active share that covers the session steers in mob mode.
    ///
    /// # Errors
    /// A database error.
    pub async fn session_is_mob(&self, station: String, session: String) -> Result<bool> {
        let now = self.now_ms();
        call(&self.inner.db, move |conn| {
            let key = format!("{station}/{session}");
            Ok(conn
                .query_row(
                    "SELECT 1 FROM shares
                     WHERE steering = 'mob' AND revoked_at IS NULL AND expires_at > ?1
                       AND (kind = 'board'
                            OR (kind = 'session' AND target = ?2)
                            OR (kind = 'space' AND target IN
                                (SELECT space_id FROM space_sessions
                                 WHERE station_id = ?3 AND session_id = ?4)))
                     LIMIT 1",
                    params![now, key, station, session],
                    |_| Ok(()),
                )
                .optional()?
                .is_some())
        })
        .await
    }

    /// Who drives the session now and who asks to drive.
    ///
    /// # Errors
    /// A database error.
    pub async fn driver_state(&self, station: String, session: String) -> Result<DriverState> {
        call(&self.inner.db, move |conn| {
            driver_state(conn, &station, &session)
        })
        .await
    }

    /// `actor` asks to drive. Nobody drives: `actor` drives now. Else the
    /// request is kept (a repeat changes nothing).
    ///
    /// # Errors
    /// A database error.
    pub async fn drive_request(
        &self,
        station: String,
        session: String,
        actor: String,
    ) -> Result<DriverState> {
        let now = self.now_ms();
        let state = call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            let current = driver_state(&tx, &station, &session)?;
            match current.driver.as_deref() {
                None => {
                    tx.execute(
                        "INSERT INTO drivers (station_id, session_id, member_or_owner, since)
                         VALUES (?1, ?2, ?3, ?4)",
                        params![station, session, actor, now],
                    )?;
                    tx.execute(
                        "DELETE FROM drive_requests
                         WHERE station_id = ?1 AND session_id = ?2 AND requester = ?3",
                        params![station, session, actor],
                    )?;
                }
                Some(driver) if driver == actor => {}
                Some(_) => {
                    tx.execute(
                        "INSERT OR IGNORE INTO drive_requests (station_id, session_id, requester, at)
                         VALUES (?1, ?2, ?3, ?4)",
                        params![station, session, actor, now],
                    )?;
                }
            }
            let state = driver_state(&tx, &station, &session)?;
            tx.commit()?;
            Ok(state)
        })
        .await?;
        Ok(state)
    }

    /// Hands the composer to `to`. The request of `to` ends. The caller
    /// checked who may hand it on and that `to` may drive.
    ///
    /// # Errors
    /// A database error.
    pub async fn drive_grant(
        &self,
        station: String,
        session: String,
        to: String,
    ) -> Result<DriverState> {
        let now = self.now_ms();
        call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            tx.execute(
                "DELETE FROM drive_requests
                 WHERE station_id = ?1 AND session_id = ?2 AND requester = ?3",
                params![station, session, to],
            )?;
            tx.execute(
                "INSERT INTO drivers (station_id, session_id, member_or_owner, since)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT (station_id, session_id)
                 DO UPDATE SET member_or_owner = excluded.member_or_owner, since = excluded.since",
                params![station, session, to, now],
            )?;
            let state = driver_state(&tx, &station, &session)?;
            tx.commit()?;
            Ok(state)
        })
        .await
    }

    /// The driver lets go: nobody drives. The first request in the queue is
    /// not promoted; the driver or the owner grants it.
    ///
    /// # Errors
    /// A database error.
    pub async fn drive_release(&self, station: String, session: String) -> Result<DriverState> {
        call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            tx.execute(
                "DELETE FROM drivers WHERE station_id = ?1 AND session_id = ?2",
                params![station, session],
            )?;
            let state = driver_state(&tx, &station, &session)?;
            tx.commit()?;
            Ok(state)
        })
        .await
    }

    /// The kind of the request `request_id` of a session, from its stored
    /// timeline; `None` when the timeline does not hold it (the caller then
    /// treats it as a tool approval).
    ///
    /// # Errors
    /// A database error.
    pub async fn request_kind(
        &self,
        station: String,
        session: String,
        request_id: String,
    ) -> Result<Option<RequestKind>> {
        call(&self.inner.db, move |conn| {
            let mut stmt = conn.prepare(
                "SELECT envelope FROM timeline
                 WHERE station_id = ?1 AND session_id = ?2 AND kind = 'approval.requested'",
            )?;
            let rows = stmt
                .query_map(params![station, session], |r| r.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            for text in rows {
                let env: fleetdeck_proto::EventEnvelope = serde_json::from_str(&text)?;
                if let EventBody::Known(fleetdeck_proto::Event::ApprovalRequested {
                    request_id: id,
                    request_kind,
                    ..
                }) = env.event
                    && id.as_str() == request_id
                {
                    return Ok(Some(request_kind));
                }
            }
            Ok(None)
        })
        .await
    }
}

#[cfg(test)]
#[path = "shares_tests.rs"]
mod tests;
