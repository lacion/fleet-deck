//! The OAuth transactions of the board (DL-173, design §2.3), in memory
//! only.
//!
//! One transaction goes through three steps:
//! 1. `POST /api/accounts/oauth/start` opens it and binds it to the owner's
//!    login session, the station, the kind and the redirect URI.
//! 2. `GET /oauth/callback` claims it by `state` (one use) and keeps the
//!    authorization code for [`CODE_TTL_MS`]. This route has no cookie and no
//!    CSRF header, because the provider redirect is cross-site.
//! 3. `POST /api/accounts/oauth/finish` takes it, for the same login
//!    session, and forwards `{ state, code }` to the station.
//!
//! Nothing here is stored on disk. The callback handler never logs: this
//! file has no `tracing` call (a test checks that).

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use axum::extract::{Extension, RawQuery, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use fleetdeck_core::Secret;
use fleetdeck_core::secret::ct_eq;
use fleetdeck_proto::accounts::AccountKind;
use ring::rand::{SecureRandom, SystemRandom};

use crate::http::Board;

/// How long a transaction lives from its start.
pub const TTL_MS: i64 = 10 * 60 * 1000;
/// How long the callback keeps the authorization code.
pub const CODE_TTL_MS: i64 = 120 * 1000;
/// The most open transactions of one login session.
pub const MAX_OPEN_PER_SESSION: usize = 4;
/// The longest `state` or `code` that the callback accepts.
pub const MAX_PARAM_CHARS: usize = 512;

/// A transaction that the callback has claimed.
struct Claimed {
    code: Secret<String>,
    at: i64,
}

struct Transaction {
    /// The `state` that the provider returns.
    state: String,
    /// The SHA-256 of the owner's login session (the stored cookie hash).
    session_hash: String,
    station: String,
    kind: AccountKind,
    redirect_uri: String,
    created_at: i64,
    /// `None`: the callback has not claimed it yet.
    claimed: Option<Claimed>,
}

impl Transaction {
    fn alive(&self, now: i64) -> bool {
        now.saturating_sub(self.created_at) < TTL_MS
    }
}

/// A transaction that `start` opened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Opened {
    pub transaction: String,
    pub state: String,
}

/// Why a start was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenError {
    /// The login session has [`MAX_OPEN_PER_SESSION`] open transactions.
    TooMany,
    /// The system random source failed.
    Random,
}

/// What the callback tells about the provider answer.
pub enum Outcome {
    /// The provider sent a code.
    Code(Secret<String>),
    /// The provider sent an `error`, or no code.
    Denied,
}

/// What a finish gets.
pub struct Finish {
    pub station: String,
    pub kind: AccountKind,
    pub redirect_uri: String,
    pub state: String,
    pub code: Secret<String>,
}

impl std::fmt::Debug for Finish {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Finish")
            .field("station", &self.station)
            .field("kind", &self.kind)
            .field("state", &"<redacted>")
            .field("code", &"<redacted>")
            .finish()
    }
}

/// Why a finish was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FinishError {
    /// No such transaction, or it expired.
    Unknown,
    /// The caller is not the login session that started it.
    WrongSession,
    /// The callback has not delivered a code, or the code expired.
    NotReady,
}

/// The open transactions of the deck.
#[derive(Default)]
pub struct Transactions {
    open: Mutex<HashMap<String, Transaction>>,
}

impl std::fmt::Debug for Transactions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Transactions(..)")
    }
}

/// `n` random bytes as base64url.
fn random_id(n: usize) -> Option<String> {
    let mut bytes = vec![0u8; n];
    SystemRandom::new().fill(&mut bytes).ok()?;
    Some(URL_SAFE_NO_PAD.encode(bytes))
}

impl Transactions {
    fn map(&self) -> std::sync::MutexGuard<'_, HashMap<String, Transaction>> {
        self.open.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Opens a transaction for the login session `session_hash`: a `state`
    /// of 32 random bytes and an id of 16 random bytes. Expired transactions
    /// are dropped first.
    ///
    /// # Errors
    /// [`OpenError::TooMany`] at the limit of the session.
    pub fn open(
        &self,
        now: i64,
        session_hash: &str,
        station: &str,
        kind: AccountKind,
        redirect_uri: &str,
    ) -> Result<Opened, OpenError> {
        let state = random_id(32).ok_or(OpenError::Random)?;
        let transaction = random_id(16).ok_or(OpenError::Random)?;
        let mut map = self.map();
        map.retain(|_, t| t.alive(now));
        let mine = map
            .values()
            .filter(|t| ct_eq(t.session_hash.as_bytes(), session_hash.as_bytes()))
            .count();
        if mine >= MAX_OPEN_PER_SESSION {
            return Err(OpenError::TooMany);
        }
        map.insert(
            transaction.clone(),
            Transaction {
                state: state.clone(),
                session_hash: session_hash.to_owned(),
                station: station.to_owned(),
                kind,
                redirect_uri: redirect_uri.to_owned(),
                created_at: now,
                claimed: None,
            },
        );
        Ok(Opened { transaction, state })
    }

    /// Drops a transaction (the station refused the start).
    pub fn discard(&self, transaction: &str) {
        self.map().remove(transaction);
    }

    /// The callback claims the transaction of `state`, once. Returns the
    /// transaction id, or `None` for an unknown, used or expired state. A
    /// [`Outcome::Denied`] drops the transaction.
    pub fn claim(&self, now: i64, state: &str, outcome: Outcome) -> Option<String> {
        let mut map = self.map();
        map.retain(|_, t| t.alive(now));
        let id = map
            .iter()
            .find(|(_, t)| t.claimed.is_none() && ct_eq(t.state.as_bytes(), state.as_bytes()))
            .map(|(id, _)| id.clone())?;
        match outcome {
            Outcome::Code(code) => {
                if let Some(t) = map.get_mut(&id) {
                    t.claimed = Some(Claimed { code, at: now });
                }
            }
            Outcome::Denied => {
                map.remove(&id);
            }
        }
        Some(id)
    }

    /// A finish takes the transaction: the caller must be the login session
    /// that started it, and the callback must have delivered a code. A
    /// success removes the transaction. A code that is older than
    /// [`CODE_TTL_MS`] removes it too.
    ///
    /// # Errors
    /// [`FinishError`].
    pub fn take(
        &self,
        now: i64,
        transaction: &str,
        session_hash: &str,
    ) -> Result<Finish, FinishError> {
        let mut map = self.map();
        map.retain(|_, t| t.alive(now));
        let t = map.get(transaction).ok_or(FinishError::Unknown)?;
        if !ct_eq(t.session_hash.as_bytes(), session_hash.as_bytes()) {
            return Err(FinishError::WrongSession);
        }
        let Some(claimed) = &t.claimed else {
            return Err(FinishError::NotReady);
        };
        if now.saturating_sub(claimed.at) >= CODE_TTL_MS {
            map.remove(transaction);
            return Err(FinishError::NotReady);
        }
        let t = map.remove(transaction).ok_or(FinishError::Unknown)?;
        let claimed = t.claimed.ok_or(FinishError::NotReady)?;
        Ok(Finish {
            station: t.station,
            kind: t.kind,
            redirect_uri: t.redirect_uri,
            state: t.state,
            code: claimed.code,
        })
    }

    /// The number of open transactions (tests).
    #[must_use]
    pub fn len(&self) -> usize {
        self.map().len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// True for a `state` or `code` of the callback: 1 to
/// [`MAX_PARAM_CHARS`] characters of `[A-Za-z0-9._~-]`.
fn clean(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_PARAM_CHARS
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'~' | b'-'))
}

/// The first value of `name` in a raw query. The values that the callback
/// accepts hold no percent escape, so no decoding is done.
fn param<'a>(query: &'a str, name: &str) -> Option<&'a str> {
    query.split('&').find_map(|kv| {
        let (k, v) = kv.split_once('=')?;
        (k == name).then_some(v)
    })
}

/// The 303 answer of the callback. It leaves no callback URL in the history
/// and sends no referrer.
fn see_other(location: &str) -> Response {
    let mut r = StatusCode::SEE_OTHER.into_response();
    let h = r.headers_mut();
    if let Ok(v) = HeaderValue::from_str(location) {
        h.insert(header::LOCATION, v);
    }
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    h.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    r
}

/// Where the board page of the accounts finishes or shows an error.
fn settings(query: &str) -> String {
    format!("/#/settings/accounts?{query}")
}

/// `GET /oauth/callback?code&state`: no cookie and no CSRF (the provider
/// redirect is cross-site, so a `SameSite=Strict` cookie is not sent). The
/// state of an open transaction is the only credential. The handler never
/// logs the query and never echoes it.
pub async fn callback(
    State(board): State<Arc<Board>>,
    Extension(transactions): Extension<Arc<Transactions>>,
    RawQuery(query): RawQuery,
) -> Response {
    let query = query.unwrap_or_default();
    let invalid = || see_other(&settings("oauth_error=invalid_state"));
    let Some(state) = param(&query, "state").filter(|s| clean(s)) else {
        return invalid();
    };
    let outcome = if param(&query, "error").is_some() {
        Outcome::Denied
    } else {
        match param(&query, "code") {
            None => Outcome::Denied,
            Some(c) if clean(c) => Outcome::Code(Secret::new(c.to_owned())),
            Some(_) => return invalid(),
        }
    };
    let denied = matches!(outcome, Outcome::Denied);
    match transactions.claim(board.store.now_ms(), state, outcome) {
        None => invalid(),
        Some(_) if denied => see_other(&settings("oauth_error=denied")),
        Some(id) => see_other(&settings(&format!("finish={id}"))),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    const MIN: i64 = 60_000;

    fn open(t: &Transactions, now: i64, session: &str) -> Result<Opened, OpenError> {
        t.open(
            now,
            session,
            "alpha",
            AccountKind::Jira,
            "https://x/oauth/callback",
        )
    }

    fn code(c: &str) -> Outcome {
        Outcome::Code(Secret::new(c.to_owned()))
    }

    #[test]
    fn state_and_id_are_random_base64url() {
        let t = Transactions::default();
        let a = open(&t, 0, "s1").unwrap();
        let b = open(&t, 0, "s1").unwrap();
        assert_eq!(a.state.len(), 43, "32 bytes");
        assert_eq!(a.transaction.len(), 22, "16 bytes");
        assert_ne!(a.state, b.state);
        assert_ne!(a.transaction, b.transaction);
        for v in [&a.state, &a.transaction] {
            assert!(
                v.bytes()
                    .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
            );
        }
    }

    #[test]
    fn a_session_has_four_open_transactions_and_expired_ones_do_not_count() {
        let t = Transactions::default();
        for _ in 0..MAX_OPEN_PER_SESSION {
            open(&t, 0, "s1").unwrap();
        }
        assert_eq!(open(&t, 1, "s1").unwrap_err(), OpenError::TooMany);
        open(&t, 1, "s2").unwrap();
        open(&t, TTL_MS, "s1").unwrap();
        assert_eq!(t.len(), 2, "the four expired ones are gone");
    }

    #[test]
    fn a_state_is_claimed_once_and_only_while_it_lives() {
        let t = Transactions::default();
        let o = open(&t, 0, "s1").unwrap();
        assert_eq!(t.claim(1, "other", code("c")), None);
        assert_eq!(t.claim(1, &o.state, code("c")), Some(o.transaction.clone()));
        assert_eq!(t.claim(2, &o.state, code("c")), None, "one use");
        let late = open(&t, 0, "s1").unwrap();
        assert_eq!(t.claim(TTL_MS, &late.state, code("c")), None, "expired");
    }

    #[test]
    fn a_denied_claim_drops_the_transaction() {
        let t = Transactions::default();
        let o = open(&t, 0, "s1").unwrap();
        assert!(t.claim(1, &o.state, Outcome::Denied).is_some());
        assert!(t.is_empty());
        assert_eq!(t.claim(2, &o.state, code("c")), None);
    }

    #[test]
    fn a_finish_needs_the_same_session_a_code_and_a_fresh_code() {
        let t = Transactions::default();
        let o = open(&t, 0, "s1").unwrap();
        let err = |r: Result<Finish, FinishError>| r.map(|_| ()).unwrap_err();
        assert_eq!(err(t.take(1, "nope", "s1")), FinishError::Unknown);
        assert_eq!(err(t.take(1, &o.transaction, "s1")), FinishError::NotReady);
        t.claim(2, &o.state, code("the-code")).unwrap();
        assert_eq!(
            err(t.take(3, &o.transaction, "s2")),
            FinishError::WrongSession
        );
        let f = t.take(3, &o.transaction, "s1").unwrap();
        assert_eq!(f.code.expose(), "the-code");
        assert_eq!(
            (f.state.as_str(), f.station.as_str()),
            (o.state.as_str(), "alpha")
        );
        assert_eq!(err(t.take(4, &o.transaction, "s1")), FinishError::Unknown);

        let o = open(&t, 0, "s1").unwrap();
        t.claim(2, &o.state, code("old")).unwrap();
        assert_eq!(
            err(t.take(2 + CODE_TTL_MS, &o.transaction, "s1")),
            FinishError::NotReady
        );
        assert!(t.is_empty(), "an old code takes the transaction with it");
        let o = open(&t, 0, "s1").unwrap();
        t.claim(2, &o.state, code("c")).unwrap();
        assert_eq!(
            err(t.take(10 * MIN, &o.transaction, "s1")),
            FinishError::Unknown
        );
    }

    #[test]
    fn debug_output_hides_the_state_and_the_code() {
        let t = Transactions::default();
        let o = open(&t, 0, "s1").unwrap();
        t.claim(1, &o.state, code("canary-code")).unwrap();
        let f = t.take(2, &o.transaction, "s1").unwrap();
        let text = format!("{f:?} {t:?}");
        assert!(
            !text.contains("canary-code") && !text.contains(&o.state),
            "{text}"
        );
    }

    #[test]
    fn the_callback_parameters_are_checked() {
        assert!(clean("abc-DEF_1.2~3"));
        for bad in [
            "",
            "a b",
            "a%20b",
            "a/b",
            "a=b",
            &"a".repeat(MAX_PARAM_CHARS + 1),
        ] {
            assert!(!clean(bad), "{bad}");
        }
        assert!(clean(&"a".repeat(MAX_PARAM_CHARS)));
        assert_eq!(param("a=1&state=x&state=y", "state"), Some("x"));
        assert_eq!(param("a=1", "state"), None);
    }

    /// The callback never logs: its module has no `tracing` call.
    #[test]
    fn the_callback_module_has_no_log_call() {
        let source = include_str!("oauth.rs");
        let code = source.split("#[cfg(test)]").next().unwrap();
        for forbidden in ["tracing::", "println!", "eprintln!", "dbg!", "log::"] {
            assert!(!code.contains(forbidden), "{forbidden}");
        }
    }
}
