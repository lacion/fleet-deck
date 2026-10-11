//! Owner login state (M2 P3-8; PLAN §9, A13, L15):
//!
//! - the owner's password as an argon2id PHC string (optional: the owner
//!   can log in with one-time links from `fleetdeck open` only);
//! - login sessions: a random cookie value of which only the SHA-256 hash
//!   is stored, a CSRF token, an idle and an absolute expiry;
//! - one-time login links (`fleetdeck open`), single use, 5 minutes;
//! - API tokens for bearer clients, each with a scope set and an expiry.
//!
//! The store never holds a plain cookie, link or token value.

use std::collections::BTreeSet;

use argon2::Argon2;
use argon2::password_hash::rand_core::OsRng;
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use fleetdeck_core::Secret;
use fleetdeck_proto::link::Scope;
use ring::digest::{SHA256, digest};
use ring::rand::{SecureRandom, SystemRandom};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};

use super::{DeckError, DeckStore, Result, call};

/// A login session ends after 7 idle days.
pub const SESSION_IDLE_MS: i64 = 7 * 24 * 60 * 60 * 1000;
/// A login session ends 30 days after the login.
pub const SESSION_MAX_MS: i64 = 30 * 24 * 60 * 60 * 1000;
/// A `fleetdeck open` link is valid for 5 minutes and one use.
pub const LOGIN_LINK_TTL_MS: i64 = 5 * 60 * 1000;
/// The shortest password.
pub const MIN_PASSWORD_CHARS: usize = 12;
/// The longest API token lifetime: 90 days.
pub const MAX_TOKEN_TTL_MS: i64 = 90 * 24 * 60 * 60 * 1000;
/// The scopes of the owner's login session. `coordinate` belongs to
/// coordinator tokens (Phase 6).
pub const OWNER_SCOPES: [Scope; 4] = [Scope::Read, Scope::Drive, Scope::Approve, Scope::Admin];

const LINK_PREFIX: &str = "fdlogin1_";
const TOKEN_PREFIX: &str = "fdtok1_";

/// How a login session started.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LoginMethod {
    Password,
    /// A one-time link of `fleetdeck open`.
    Link,
}

impl LoginMethod {
    fn as_str(self) -> &'static str {
        match self {
            Self::Password => "password",
            Self::Link => "link",
        }
    }
}

/// A new login session: the cookie value goes to the browser once.
#[derive(Debug)]
pub struct NewLogin {
    pub cookie: Secret<String>,
    pub csrf: String,
    pub expires_at: i64,
}

/// A valid login session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoginSession {
    pub session_hash: String,
    pub csrf: String,
    pub created_at: i64,
    pub expires_at: i64,
}

/// A stored API token (without its value).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiToken {
    pub name: String,
    pub scopes: BTreeSet<Scope>,
    pub created_at: i64,
    pub expires_at: i64,
    pub last_used: Option<i64>,
}

/// Who calls the board API.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Principal {
    /// The owner's login session (cookie; CSRF applies).
    Owner(LoginSession),
    /// A bearer client.
    Token(ApiToken),
}

impl Principal {
    /// The audit actor name.
    #[must_use]
    pub fn actor(&self) -> String {
        match self {
            Self::Owner(_) => "owner".to_owned(),
            Self::Token(t) => format!("token:{}", t.name),
        }
    }

    #[must_use]
    pub fn has(&self, scope: Scope) -> bool {
        match self {
            Self::Owner(_) => OWNER_SCOPES.contains(&scope),
            Self::Token(t) => t.scopes.contains(&scope),
        }
    }

    #[must_use]
    pub fn scopes(&self) -> BTreeSet<Scope> {
        match self {
            Self::Owner(_) => OWNER_SCOPES.into_iter().collect(),
            Self::Token(t) => t.scopes.clone(),
        }
    }
}

/// 32 random bytes as hex.
pub(crate) fn random_token() -> Result<String> {
    random_hex()
}

fn random_hex() -> Result<String> {
    let mut bytes = [0u8; 32];
    SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| DeckError::Hash("the system random source failed".to_owned()))?;
    Ok(hex(&bytes))
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
}

/// The stored form of a cookie, link or token value.
#[must_use]
pub fn secret_hash(value: &str) -> String {
    hex(digest(&SHA256, value.as_bytes()).as_ref())
}

fn hash_password(password: &str) -> Result<String> {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e| DeckError::Hash(e.to_string()))
}

fn verify_password(password: &str, phc: &str) -> Result<bool> {
    let parsed = PasswordHash::new(phc).map_err(|e| DeckError::Corrupt(e.to_string()))?;
    Ok(Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .is_ok())
}

async fn blocking<R: Send + 'static>(f: impl FnOnce() -> Result<R> + Send + 'static) -> Result<R> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| DeckError::Hash(e.to_string()))?
}

fn scopes_json(scopes: &BTreeSet<Scope>) -> Result<String> {
    Ok(serde_json::to_string(scopes)?)
}

impl DeckStore {
    /// The owner id (D6).
    ///
    /// # Errors
    /// A database error.
    pub async fn owner_id(&self) -> Result<String> {
        call(&self.inner.db, |conn| {
            Ok(conn.query_row("SELECT owner_id FROM owner WHERE id = 1", [], |r| r.get(0))?)
        })
        .await
    }

    /// Sets the owner password and ends every login session.
    ///
    /// # Errors
    /// [`DeckError::Invalid`] for a password shorter than
    /// [`MIN_PASSWORD_CHARS`]; a database or hashing error.
    pub async fn set_password(&self, password: Secret<String>) -> Result<()> {
        if password.expose().chars().count() < MIN_PASSWORD_CHARS {
            return Err(DeckError::Invalid(format!(
                "the password needs at least {MIN_PASSWORD_CHARS} characters"
            )));
        }
        let phc = blocking(move || hash_password(password.expose())).await?;
        let now = self.now_ms();
        call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            tx.execute(
                "UPDATE owner SET password_hash = ?1, password_set_at = ?2 WHERE id = 1",
                params![phc, now],
            )?;
            tx.execute("DELETE FROM login_sessions", [])?;
            tx.commit()?;
            Ok(())
        })
        .await
    }

    /// True when the owner has a password.
    ///
    /// # Errors
    /// A database error.
    pub async fn password_set(&self) -> Result<bool> {
        Ok(self.password_hash().await?.is_some())
    }

    async fn password_hash(&self) -> Result<Option<String>> {
        call(&self.inner.db, |conn| {
            Ok(
                conn.query_row("SELECT password_hash FROM owner WHERE id = 1", [], |r| {
                    r.get(0)
                })?,
            )
        })
        .await
    }

    /// Checks the owner password. False when no password is set.
    ///
    /// # Errors
    /// A database or hashing error.
    pub async fn check_password(&self, password: Secret<String>) -> Result<bool> {
        let Some(phc) = self.password_hash().await? else {
            return Ok(false);
        };
        blocking(move || verify_password(password.expose(), &phc)).await
    }

    /// A one-time login link token for `fleetdeck open`.
    ///
    /// # Errors
    /// A database error.
    pub async fn create_login_link(&self) -> Result<Secret<String>> {
        let token = format!("{LINK_PREFIX}{}", random_hex()?);
        let hash = secret_hash(&token);
        let now = self.now_ms();
        call(&self.inner.db, move |conn| {
            conn.execute("DELETE FROM login_links WHERE expires_at <= ?1", [now])?;
            conn.execute(
                "INSERT INTO login_links (token_hash, created_at, expires_at) VALUES (?1, ?2, ?3)",
                params![hash, now, now + LOGIN_LINK_TTL_MS],
            )?;
            Ok(())
        })
        .await?;
        Ok(Secret::new(token))
    }

    /// Uses a login link: true once for a valid link, false after that.
    ///
    /// # Errors
    /// A database error.
    pub async fn redeem_login_link(&self, token: &Secret<String>) -> Result<bool> {
        let hash = secret_hash(token.expose());
        let now = self.now_ms();
        call(&self.inner.db, move |conn| {
            let n = conn.execute(
                "DELETE FROM login_links WHERE token_hash = ?1 AND expires_at > ?2",
                params![hash, now],
            )?;
            Ok(n == 1)
        })
        .await
    }

    /// Starts a login session.
    ///
    /// # Errors
    /// A database error.
    pub async fn create_login(&self, method: LoginMethod) -> Result<NewLogin> {
        let cookie = random_hex()?;
        let csrf = random_hex()?;
        let hash = secret_hash(&cookie);
        let now = self.now_ms();
        let expires_at = now + SESSION_MAX_MS;
        let stored = csrf.clone();
        call(&self.inner.db, move |conn| {
            conn.execute(
                "DELETE FROM login_sessions WHERE expires_at <= ?1 OR last_seen <= ?2",
                params![now, now - SESSION_IDLE_MS],
            )?;
            conn.execute(
                "INSERT INTO login_sessions
                 (session_hash, csrf_token, method, created_at, last_seen, expires_at)
                 VALUES (?1, ?2, ?3, ?4, ?4, ?5)",
                params![hash, stored, method.as_str(), now, expires_at],
            )?;
            Ok(())
        })
        .await?;
        Ok(NewLogin {
            cookie: Secret::new(cookie),
            csrf,
            expires_at,
        })
    }

    /// The valid login session of a cookie value; refreshes its idle time.
    ///
    /// # Errors
    /// A database error.
    pub async fn login_session(&self, cookie: &Secret<String>) -> Result<Option<LoginSession>> {
        let hash = secret_hash(cookie.expose());
        let now = self.now_ms();
        call(&self.inner.db, move |conn| {
            let found = conn
                .query_row(
                    "UPDATE login_sessions SET last_seen = ?2
                     WHERE session_hash = ?1 AND expires_at > ?2 AND last_seen > ?3
                     RETURNING csrf_token, created_at, expires_at",
                    params![hash, now, now - SESSION_IDLE_MS],
                    |r| {
                        Ok(LoginSession {
                            session_hash: hash.clone(),
                            csrf: r.get(0)?,
                            created_at: r.get(1)?,
                            expires_at: r.get(2)?,
                        })
                    },
                )
                .optional()?;
            Ok(found)
        })
        .await
    }

    /// True while the login session or the API token of `principal` is
    /// still valid (an open board WebSocket checks this; an open board
    /// also keeps its login session from idling out).
    ///
    /// # Errors
    /// A database error.
    pub async fn principal_alive(&self, principal: &Principal) -> Result<bool> {
        let now = self.now_ms();
        let principal = principal.clone();
        call(&self.inner.db, move |conn| {
            let n = match &principal {
                Principal::Owner(s) => conn.execute(
                    "UPDATE login_sessions SET last_seen = ?2
                     WHERE session_hash = ?1 AND expires_at > ?2 AND last_seen > ?3",
                    params![s.session_hash, now, now - SESSION_IDLE_MS],
                )?,
                Principal::Token(t) => conn.execute(
                    "UPDATE api_tokens SET last_used = ?2
                     WHERE name = ?1 AND created_at = ?3 AND expires_at > ?2",
                    params![t.name, now, t.created_at],
                )?,
            };
            Ok(n == 1)
        })
        .await
    }

    /// Ends one login session. True when it existed.
    ///
    /// # Errors
    /// A database error.
    pub async fn end_login(&self, session_hash: String) -> Result<bool> {
        call(&self.inner.db, move |conn| {
            Ok(conn.execute(
                "DELETE FROM login_sessions WHERE session_hash = ?1",
                [session_hash],
            )? == 1)
        })
        .await
    }

    /// Creates an API token. The value is returned once.
    ///
    /// # Errors
    /// [`DeckError::Invalid`] for an empty or taken name, an empty scope
    /// set, `coordinate`, or a lifetime above [`MAX_TOKEN_TTL_MS`].
    pub async fn create_token(
        &self,
        name: String,
        scopes: BTreeSet<Scope>,
        ttl_ms: i64,
    ) -> Result<(ApiToken, Secret<String>)> {
        let valid_name = !name.is_empty()
            && name.len() <= 63
            && name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
        if !valid_name {
            return Err(DeckError::Invalid(
                "a token name is 1-63 letters, digits, `-` or `_`".to_owned(),
            ));
        }
        if scopes.is_empty() || scopes.iter().any(|s| !OWNER_SCOPES.contains(s)) {
            return Err(DeckError::Invalid(
                "a token needs one or more of the scopes read, drive, approve, admin".to_owned(),
            ));
        }
        if !(1..=MAX_TOKEN_TTL_MS).contains(&ttl_ms) {
            return Err(DeckError::Invalid(
                "a token lives for 1 ms to 90 days".to_owned(),
            ));
        }
        let value = format!("{TOKEN_PREFIX}{}", random_hex()?);
        let now = self.now_ms();
        let token = ApiToken {
            name,
            scopes,
            created_at: now,
            expires_at: now + ttl_ms,
            last_used: None,
        };
        let row = token.clone();
        let hash = secret_hash(&value);
        call(&self.inner.db, move |conn| {
            let n = conn.execute(
                "INSERT OR IGNORE INTO api_tokens (name, token_hash, scopes, created_at, expires_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![row.name, hash, scopes_json(&row.scopes)?, row.created_at, row.expires_at],
            )?;
            if n == 0 {
                return Err(DeckError::Invalid(format!(
                    "a token named `{}` exists already",
                    row.name
                )));
            }
            Ok(())
        })
        .await?;
        Ok((token, Secret::new(value)))
    }

    /// The valid token of a bearer value; records its use.
    ///
    /// # Errors
    /// A database error.
    pub async fn token(&self, value: &Secret<String>) -> Result<Option<ApiToken>> {
        if !value.expose().starts_with(TOKEN_PREFIX) {
            return Ok(None);
        }
        let hash = secret_hash(value.expose());
        let now = self.now_ms();
        call(&self.inner.db, move |conn| {
            let row = conn
                .query_row(
                    "UPDATE api_tokens SET last_used = ?2
                     WHERE token_hash = ?1 AND expires_at > ?2
                     RETURNING name, scopes, created_at, expires_at, last_used",
                    params![hash, now],
                    token_row,
                )
                .optional()?;
            row.map(parse_token).transpose()
        })
        .await
    }

    /// Every API token (without values).
    ///
    /// # Errors
    /// A database error.
    pub async fn tokens(&self) -> Result<Vec<ApiToken>> {
        call(&self.inner.db, |conn| {
            let mut stmt = conn.prepare(
                "SELECT name, scopes, created_at, expires_at, last_used
                 FROM api_tokens ORDER BY name",
            )?;
            let rows = stmt.query_map([], token_row)?;
            rows.map(|r| parse_token(r?)).collect()
        })
        .await
    }

    /// Deletes an API token. True when it existed.
    ///
    /// # Errors
    /// A database error.
    pub async fn revoke_token(&self, name: String) -> Result<bool> {
        call(&self.inner.db, move |conn| {
            Ok(conn.execute("DELETE FROM api_tokens WHERE name = ?1", [name])? == 1)
        })
        .await
    }
}

type TokenRow = (String, String, i64, i64, Option<i64>);

fn token_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<TokenRow> {
    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
}

fn parse_token((name, scopes, created_at, expires_at, last_used): TokenRow) -> Result<ApiToken> {
    Ok(ApiToken {
        name,
        scopes: serde_json::from_str(&scopes)?,
        created_at,
        expires_at,
        last_used,
    })
}

#[cfg(test)]
#[path = "auth_tests.rs"]
mod tests;
