//! The display principal (R13 §5, DL-201): a read-only browser login for a
//! wall display.
//!
//! - displays: a name, a ceiling (station ids), the owner's switches and an
//!   optional binding to a desktop;
//! - display sessions: a random cookie value of which only the SHA-256 hash
//!   is stored, an idle limit of 30 days and an absolute limit of 90 days (a
//!   use never moves the absolute limit);
//! - one-time display links, single use, 5 minutes.
//!
//! The pairing by code is in [`super::displays_pairing`]. The store never
//! holds a plain cookie, link or secret value.

use fleetdeck_core::Secret;
use fleetdeck_proto::StationId;
use fleetdeck_proto::display::{DisplayBinding, DisplayView};
use rusqlite::{Connection, OptionalExtension, params};

use super::auth::{random_token, secret_hash};
use super::{DeckError, DeckStore, Result, call};

/// A display session ends after 30 idle days.
pub const DISPLAY_IDLE_MS: i64 = 30 * 24 * 60 * 60 * 1000;
/// A display session ends 90 days after it started, however often it is used.
pub const DISPLAY_MAX_MS: i64 = 90 * 24 * 60 * 60 * 1000;
/// A display link is valid for 5 minutes and one use.
pub const DISPLAY_LINK_TTL_MS: i64 = 5 * 60 * 1000;
/// The longest display name, in characters.
pub const MAX_DISPLAY_NAME: usize = 63;
/// The most stations in a ceiling.
pub const MAX_CEILING: usize = 64;

const SESSION_PREFIX: &str = "fdisp1_";
const LINK_PREFIX: &str = "fddlink1_";

/// The V24 migration (R13 §5): the display tables. To reverse it, drop the
/// four tables; no other table changes.
pub const DISPLAYS_SQL: &str = "
CREATE TABLE displays (
    display_id         TEXT    PRIMARY KEY,
    name               TEXT    NOT NULL,
    ceiling            TEXT    NOT NULL,
    show_diffs         INTEGER NOT NULL DEFAULT 0,
    wake_on_needs_you  INTEGER NOT NULL DEFAULT 1,
    quiet_hours        TEXT,
    binding_station    TEXT,
    binding_monitor    TEXT,
    created_at         INTEGER NOT NULL,
    last_seen          INTEGER,
    revoked_at         INTEGER
) STRICT;

CREATE TABLE display_sessions (
    session_hash  TEXT    PRIMARY KEY,
    display_id    TEXT    NOT NULL,
    created_at    INTEGER NOT NULL,
    last_seen     INTEGER NOT NULL,
    expires_at    INTEGER NOT NULL
) STRICT;

CREATE TABLE display_links (
    token_hash  TEXT    PRIMARY KEY,
    display_id  TEXT    NOT NULL,
    created_at  INTEGER NOT NULL,
    expires_at  INTEGER NOT NULL
) STRICT;

CREATE TABLE display_pairings (
    pairing_id     TEXT    PRIMARY KEY,
    code           TEXT    NOT NULL,
    verifier_hash  TEXT,
    state          TEXT    NOT NULL CHECK (state IN ('pending', 'approved')),
    source         TEXT    NOT NULL,
    user_agent     TEXT    NOT NULL,
    display_id     TEXT,
    created_at     INTEGER NOT NULL,
    expires_at     INTEGER NOT NULL
) STRICT;
CREATE UNIQUE INDEX display_pairings_code ON display_pairings(code);
";

/// A valid display login (a display session of a display that is not
/// revoked).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisplayLogin {
    pub session_hash: String,
    pub display_id: String,
    pub name: String,
    pub ceiling: Vec<StationId>,
}

/// A stored display.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisplayRecord {
    pub display_id: String,
    pub name: String,
    pub ceiling: Vec<StationId>,
    pub show_diffs: bool,
    pub wake_on_needs_you: bool,
    pub quiet_hours: Option<String>,
    pub binding: Option<DisplayBinding>,
    pub created_at: i64,
    pub last_seen: Option<i64>,
}

impl DisplayRecord {
    /// The owner's view of the display.
    #[must_use]
    pub fn view(&self) -> DisplayView {
        DisplayView {
            display_id: self.display_id.clone(),
            name: self.name.clone(),
            ceiling: self.ceiling.clone(),
            show_diffs: self.show_diffs,
            wake_on_needs_you: self.wake_on_needs_you,
            quiet_hours: self.quiet_hours.clone(),
            binding: self.binding.clone(),
            created_at: self.created_at,
            last_seen: self.last_seen,
        }
    }
}

/// A new display: what the owner chooses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewDisplay {
    pub name: String,
    pub ceiling: Vec<StationId>,
}

/// A change of the settings of a display. `None`: unchanged. `quiet_hours`
/// is `Some(None)` to clear it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DisplayChange {
    pub name: Option<String>,
    pub ceiling: Option<Vec<StationId>>,
    pub show_diffs: Option<bool>,
    pub wake_on_needs_you: Option<bool>,
    pub quiet_hours: Option<Option<String>>,
}

fn invalid(message: &str) -> DeckError {
    DeckError::Invalid(message.to_owned())
}

/// A trimmed display name of 1 to [`MAX_DISPLAY_NAME`] characters without a
/// control character.
///
/// # Errors
/// [`DeckError::Invalid`].
pub fn clean_name(name: &str) -> Result<String> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > MAX_DISPLAY_NAME {
        return Err(invalid("a display name is 1-63 characters"));
    }
    if name.chars().any(char::is_control) {
        return Err(invalid("a display name has no control character"));
    }
    Ok(name.to_owned())
}

/// `HH:MM-HH:MM` with a valid hour and minute in each part.
///
/// # Errors
/// [`DeckError::Invalid`].
pub fn check_quiet_hours(text: &str) -> Result<()> {
    fn part(p: &str) -> bool {
        let b = p.as_bytes();
        p.is_ascii()
            && b.len() == 5
            && b[2] == b':'
            && [0, 1, 3, 4].iter().all(|i| b[*i].is_ascii_digit())
            && p[..2].parse::<u8>().is_ok_and(|h| h < 24)
            && p[3..].parse::<u8>().is_ok_and(|m| m < 60)
    }
    match text.split_once('-') {
        Some((a, b)) if part(a) && part(b) => Ok(()),
        _ => Err(invalid("the quiet hours are HH:MM-HH:MM")),
    }
}

/// A ceiling of 1 to [`MAX_CEILING`] distinct stations that the deck pairs.
pub(super) fn check_ceiling(conn: &Connection, ceiling: &[StationId]) -> Result<()> {
    if ceiling.is_empty() || ceiling.len() > MAX_CEILING {
        return Err(invalid("a ceiling is 1-64 stations"));
    }
    let mut seen = std::collections::BTreeSet::new();
    for id in ceiling {
        if !seen.insert(id.as_str()) {
            return Err(invalid("a ceiling names a station once"));
        }
        let known = conn
            .query_row(
                "SELECT 1 FROM stations WHERE station_id = ?1",
                [id.as_str()],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        if !known {
            return Err(invalid("a ceiling names a station that is not paired"));
        }
    }
    Ok(())
}

fn row_record(r: &rusqlite::Row<'_>) -> rusqlite::Result<(DisplayRecord, String)> {
    let ceiling: String = r.get(2)?;
    let station: Option<String> = r.get(6)?;
    let monitor: Option<String> = r.get(7)?;
    let binding = station.zip(monitor).map(|(s, m)| DisplayBinding {
        station_id: StationId::new(s),
        monitor: m,
    });
    Ok((
        DisplayRecord {
            display_id: r.get(0)?,
            name: r.get(1)?,
            ceiling: Vec::new(),
            show_diffs: r.get::<_, i64>(3)? != 0,
            wake_on_needs_you: r.get::<_, i64>(4)? != 0,
            quiet_hours: r.get(5)?,
            binding,
            created_at: r.get(8)?,
            last_seen: r.get(9)?,
        },
        ceiling,
    ))
}

const COLUMNS: &str = "display_id, name, ceiling, show_diffs, wake_on_needs_you, quiet_hours,
    binding_station, binding_monitor, created_at, last_seen";

fn parse_ceiling(text: &str) -> Result<Vec<StationId>> {
    let ids: Vec<String> = serde_json::from_str(text)?;
    Ok(ids.into_iter().map(StationId::new).collect())
}

fn finish(pair: (DisplayRecord, String)) -> Result<DisplayRecord> {
    let (mut record, ceiling) = pair;
    record.ceiling = parse_ceiling(&ceiling)?;
    Ok(record)
}

/// A live (not revoked) display.
pub(super) fn load(conn: &Connection, display_id: &str) -> Result<Option<DisplayRecord>> {
    conn.query_row(
        &format!("SELECT {COLUMNS} FROM displays WHERE display_id = ?1 AND revoked_at IS NULL"),
        [display_id],
        row_record,
    )
    .optional()?
    .map(finish)
    .transpose()
}

/// Inserts a checked display. The caller holds the transaction.
pub(super) fn insert(
    conn: &Connection,
    display_id: &str,
    new: &NewDisplay,
    now: i64,
) -> Result<DisplayRecord> {
    check_ceiling(conn, &new.ceiling)?;
    let ids: Vec<&str> = new.ceiling.iter().map(StationId::as_str).collect();
    conn.execute(
        "INSERT INTO displays (display_id, name, ceiling, created_at) VALUES (?1, ?2, ?3, ?4)",
        params![display_id, new.name, serde_json::to_string(&ids)?, now],
    )?;
    load(conn, display_id)?.ok_or_else(|| DeckError::Corrupt("a new display is missing".to_owned()))
}

/// A display session is alive: not expired, not idle, and its display is not
/// revoked. A use refreshes `last_seen` of the session and of the display;
/// `expires_at` never moves.
fn touch_session(conn: &Connection, session_hash: &str, now: i64) -> Result<Option<String>> {
    let id: Option<String> = conn
        .query_row(
            "UPDATE display_sessions SET last_seen = ?2
             WHERE session_hash = ?1 AND expires_at > ?2 AND last_seen > ?3
               AND display_id IN (SELECT display_id FROM displays WHERE revoked_at IS NULL)
             RETURNING display_id",
            params![session_hash, now, now - DISPLAY_IDLE_MS],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(id) = &id {
        conn.execute(
            "UPDATE displays SET last_seen = ?2 WHERE display_id = ?1",
            params![id, now],
        )?;
    }
    Ok(id)
}

fn login_of(conn: &Connection, session_hash: &str, now: i64) -> Result<Option<DisplayLogin>> {
    let Some(id) = touch_session(conn, session_hash, now)? else {
        return Ok(None);
    };
    Ok(load(conn, &id)?.map(|d| DisplayLogin {
        session_hash: session_hash.to_owned(),
        display_id: d.display_id,
        name: d.name,
        ceiling: d.ceiling,
    }))
}

/// True while the display session and its display are valid (a refresh).
pub(super) fn session_alive(conn: &Connection, session_hash: &str, now: i64) -> Result<bool> {
    Ok(touch_session(conn, session_hash, now)?.is_some())
}

impl DeckStore {
    /// Checks a new display without writing: the cleaned name and a ceiling
    /// of paired stations. A route calls it first, so a bad body is a 400
    /// before the elevation check.
    ///
    /// # Errors
    /// [`DeckError::Invalid`]; a database error.
    pub async fn display_validate(&self, new: NewDisplay) -> Result<NewDisplay> {
        let name = clean_name(&new.name)?;
        let ceiling = new.ceiling;
        call(&self.inner.db, move |conn| {
            check_ceiling(conn, &ceiling)?;
            Ok(NewDisplay { name, ceiling })
        })
        .await
    }

    /// Checks a ceiling of paired stations without writing.
    ///
    /// # Errors
    /// [`DeckError::Invalid`]; a database error.
    pub async fn display_validate_ceiling(&self, ceiling: Vec<StationId>) -> Result<()> {
        call(&self.inner.db, move |conn| check_ceiling(conn, &ceiling)).await
    }

    /// Makes a display.
    ///
    /// # Errors
    /// [`DeckError::Invalid`] for a bad name or ceiling; a database error.
    pub async fn create_display(&self, new: NewDisplay) -> Result<DisplayRecord> {
        let name = clean_name(&new.name)?;
        let new = NewDisplay { name, ..new };
        let id: String = fleetdeck_core::ids::new_id(self.inner.ids.as_ref());
        let now = self.now_ms();
        call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            let record = insert(&tx, &id, &new, now)?;
            tx.commit()?;
            Ok(record)
        })
        .await
    }

    /// The displays that are not revoked, oldest first.
    ///
    /// # Errors
    /// A database error.
    pub async fn display_list(&self) -> Result<Vec<DisplayRecord>> {
        call(&self.inner.db, |conn| {
            let mut stmt = conn.prepare(&format!(
                "SELECT {COLUMNS} FROM displays WHERE revoked_at IS NULL
                 ORDER BY created_at, display_id"
            ))?;
            let rows = stmt.query_map([], row_record)?;
            rows.map(|r| finish(r?)).collect()
        })
        .await
    }

    /// A display that is not revoked.
    ///
    /// # Errors
    /// A database error.
    pub async fn display_get(&self, display_id: String) -> Result<Option<DisplayRecord>> {
        call(&self.inner.db, move |conn| load(conn, &display_id)).await
    }

    /// Changes the settings of a display. `None` when it does not exist or is
    /// revoked.
    ///
    /// # Errors
    /// [`DeckError::Invalid`] for a bad value; a database error.
    pub async fn display_update(
        &self,
        display_id: String,
        change: DisplayChange,
    ) -> Result<Option<DisplayRecord>> {
        let name = change.name.as_deref().map(clean_name).transpose()?;
        if let Some(Some(q)) = &change.quiet_hours {
            check_quiet_hours(q)?;
        }
        call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            let Some(old) = load(&tx, &display_id)? else {
                return Ok(None);
            };
            if let Some(ceiling) = &change.ceiling {
                check_ceiling(&tx, ceiling)?;
            }
            let ceiling = change.ceiling.as_ref().unwrap_or(&old.ceiling);
            let ids: Vec<&str> = ceiling.iter().map(StationId::as_str).collect();
            let quiet = change.quiet_hours.unwrap_or(old.quiet_hours);
            tx.execute(
                "UPDATE displays SET name = ?2, ceiling = ?3, show_diffs = ?4,
                     wake_on_needs_you = ?5, quiet_hours = ?6 WHERE display_id = ?1",
                params![
                    display_id,
                    name.as_deref().unwrap_or(&old.name),
                    serde_json::to_string(&ids)?,
                    i64::from(change.show_diffs.unwrap_or(old.show_diffs)),
                    i64::from(change.wake_on_needs_you.unwrap_or(old.wake_on_needs_you)),
                    quiet,
                ],
            )?;
            let record = load(&tx, &display_id)?;
            tx.commit()?;
            Ok(record)
        })
        .await
    }

    /// Revokes a display: its sessions, links and approved pairings go, and
    /// `revoked_at` is set. False when it does not exist or is revoked
    /// already.
    ///
    /// # Errors
    /// A database error.
    pub async fn display_revoke(&self, display_id: String) -> Result<bool> {
        let now = self.now_ms();
        call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            let n = tx.execute(
                "UPDATE displays SET revoked_at = ?2 WHERE display_id = ?1 AND revoked_at IS NULL",
                params![display_id, now],
            )?;
            if n == 1 {
                for table in ["display_sessions", "display_links", "display_pairings"] {
                    tx.execute(
                        &format!("DELETE FROM {table} WHERE display_id = ?1"),
                        [&display_id],
                    )?;
                }
            }
            tx.commit()?;
            Ok(n == 1)
        })
        .await
    }

    /// Makes a display and its one-time link in one transaction. Returns the
    /// display, the token and the link expiry.
    ///
    /// # Errors
    /// [`DeckError::Invalid`] for a bad name or ceiling; a database error.
    pub async fn create_display_with_link(
        &self,
        new: NewDisplay,
    ) -> Result<(DisplayRecord, Secret<String>, i64)> {
        let name = clean_name(&new.name)?;
        let new = NewDisplay { name, ..new };
        let id: String = fleetdeck_core::ids::new_id(self.inner.ids.as_ref());
        let token = format!("{LINK_PREFIX}{}", random_token()?);
        let hash = secret_hash(&token);
        let now = self.now_ms();
        let expires_at = now + DISPLAY_LINK_TTL_MS;
        let record = call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            tx.execute("DELETE FROM display_links WHERE expires_at <= ?1", [now])?;
            let record = insert(&tx, &id, &new, now)?;
            tx.execute(
                "INSERT INTO display_links (token_hash, display_id, created_at, expires_at)
                 VALUES (?1, ?2, ?3, ?4)",
                params![hash, id, now, expires_at],
            )?;
            tx.commit()?;
            Ok(record)
        })
        .await?;
        Ok((record, Secret::new(token), expires_at))
    }

    /// Uses a display link: the display id once for a valid link, `None`
    /// after that (or for a display that is revoked).
    ///
    /// # Errors
    /// A database error.
    pub async fn redeem_display_link(&self, token: &Secret<String>) -> Result<Option<String>> {
        let hash = secret_hash(token.expose());
        let now = self.now_ms();
        call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            let id: Option<String> = tx
                .query_row(
                    "DELETE FROM display_links WHERE token_hash = ?1 AND expires_at > ?2
                     RETURNING display_id",
                    params![hash, now],
                    |r| r.get(0),
                )
                .optional()?;
            let live = match &id {
                Some(id) => load(&tx, id)?.is_some(),
                None => false,
            };
            tx.commit()?;
            Ok(id.filter(|_| live))
        })
        .await
    }

    /// Starts a display session for a display. Returns the cookie value
    /// (`fdisp1_…`, once) and the absolute expiry.
    ///
    /// # Errors
    /// A database error.
    pub async fn create_display_session(
        &self,
        display_id: String,
    ) -> Result<(Secret<String>, i64)> {
        let cookie = format!("{SESSION_PREFIX}{}", random_token()?);
        let hash = secret_hash(&cookie);
        let now = self.now_ms();
        let expires_at = now + DISPLAY_MAX_MS;
        call(&self.inner.db, move |conn| {
            conn.execute(
                "DELETE FROM display_sessions WHERE expires_at <= ?1 OR last_seen <= ?2",
                params![now, now - DISPLAY_IDLE_MS],
            )?;
            conn.execute(
                "INSERT INTO display_sessions
                 (session_hash, display_id, created_at, last_seen, expires_at)
                 VALUES (?1, ?2, ?3, ?3, ?4)",
                params![hash, display_id, now, expires_at],
            )?;
            Ok(())
        })
        .await?;
        Ok((Secret::new(cookie), expires_at))
    }

    /// The display login of a cookie value; refreshes the idle time of the
    /// session and the last use of the display.
    ///
    /// # Errors
    /// A database error.
    pub async fn display_session(&self, cookie: &Secret<String>) -> Result<Option<DisplayLogin>> {
        let hash = secret_hash(cookie.expose());
        let now = self.now_ms();
        call(&self.inner.db, move |conn| login_of(conn, &hash, now)).await
    }

    /// The login again, with the ceiling as it is now. `None` when the
    /// session or its display ended (an open WebSocket asks).
    ///
    /// # Errors
    /// A database error.
    pub async fn display_refresh(&self, login: &DisplayLogin) -> Result<Option<DisplayLogin>> {
        let hash = login.session_hash.clone();
        let now = self.now_ms();
        call(&self.inner.db, move |conn| login_of(conn, &hash, now)).await
    }

    /// The absolute expiry of a display session.
    ///
    /// # Errors
    /// A database error.
    pub async fn display_session_expires(&self, session_hash: String) -> Result<Option<i64>> {
        call(&self.inner.db, move |conn| {
            Ok(conn
                .query_row(
                    "SELECT expires_at FROM display_sessions WHERE session_hash = ?1",
                    [session_hash],
                    |r| r.get(0),
                )
                .optional()?)
        })
        .await
    }

    /// Ends one display session. True when it existed.
    ///
    /// # Errors
    /// A database error.
    pub async fn end_display_session(&self, session_hash: String) -> Result<bool> {
        call(&self.inner.db, move |conn| {
            Ok(conn.execute(
                "DELETE FROM display_sessions WHERE session_hash = ?1",
                [session_hash],
            )? == 1)
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn a_name_is_trimmed_and_checked() {
        assert_eq!(clean_name("  Wall  ").unwrap(), "Wall");
        assert!(clean_name("   ").is_err());
        assert!(clean_name(&"x".repeat(64)).is_err());
        assert!(clean_name(&"x".repeat(63)).is_ok());
        assert!(clean_name("a\nb").is_err());
    }

    #[test]
    fn quiet_hours_are_two_clock_times() {
        for ok in ["22:00-07:30", "00:00-23:59"] {
            assert!(check_quiet_hours(ok).is_ok(), "{ok}");
        }
        for bad in [
            "",
            "22:00",
            "24:00-07:00",
            "22:60-07:00",
            "2:00-07:00",
            "22:00-07:0x",
        ] {
            assert!(check_quiet_hours(bad).is_err(), "{bad}");
        }
    }
}
