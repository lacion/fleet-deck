//! The owner's elevation (DL-172, design 1.5): a server-checked proof of a
//! recent sign-in for a danger change and a restart.
//!
//! The owner gets an elevation with the owner password, or with a one-time
//! code from `fleetdeck open --elevate` (the owner-only local socket). A code
//! is single use and lives [`CODE_TTL_MS`]. An elevation is bound to the
//! session hash of the board login that asked, and lives [`ELEVATION_TTL_MS`].
//! Both are kept in memory only, as their SHA-256.

use std::sync::{Mutex, MutexGuard, PoisonError};

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use fleetdeck_core::Secret;
use fleetdeck_core::secret::ct_eq;
use ring::rand::{SecureRandom, SystemRandom};

use crate::store::auth::secret_hash;

/// How long an elevation code lives.
pub const CODE_TTL_MS: i64 = 5 * 60 * 1000;
/// How long an elevation lives.
pub const ELEVATION_TTL_MS: i64 = 10 * 60 * 1000;
/// The most codes that wait for use at one time; a newer code pushes out the
/// oldest.
const MAX_CODES: usize = 8;
/// The most elevations that live at one time.
const MAX_ELEVATIONS: usize = 64;
/// The characters of a code: `[A-HJ-NP-Z2-9]`, 32 of them, so that a random
/// byte masked with 31 picks one without bias.
const ALPHABET: &[u8; 32] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
/// The characters in a code, without the dash.
const CODE_LEN: usize = 10;

/// Why an elevation could not be made.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("the system random source failed")]
pub struct RandomFailed;

#[derive(Debug, Default)]
struct State {
    /// `(sha256 of the code, expires_at)`.
    codes: Vec<(String, i64)>,
    /// `(sha256 of the elevation, session hash, expires_at)`.
    elevations: Vec<(String, String, i64)>,
}

/// The codes and the elevations of this deck.
#[derive(Debug, Default)]
pub struct Elevations(Mutex<State>);

/// The text of a code without dash, space or case: what is hashed.
fn normalize(code: &str) -> String {
    code.chars()
        .filter(|c| !c.is_whitespace() && *c != '-')
        .map(|c| c.to_ascii_uppercase())
        .collect()
}

impl Elevations {
    fn state(&self) -> MutexGuard<'_, State> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Makes a one-time code, shown as `XXXXX-XXXXX`. Returns it with its
    /// expiry time.
    ///
    /// # Errors
    /// [`RandomFailed`].
    pub fn new_code(&self, now: i64) -> Result<(Secret<String>, i64), RandomFailed> {
        let mut bytes = [0u8; CODE_LEN];
        SystemRandom::new()
            .fill(&mut bytes)
            .map_err(|_| RandomFailed)?;
        let plain: String = bytes
            .iter()
            .map(|b| char::from(ALPHABET[usize::from(b & 31)]))
            .collect();
        let expires_at = now + CODE_TTL_MS;
        let mut s = self.state();
        s.codes.retain(|(_, exp)| *exp > now);
        if s.codes.len() >= MAX_CODES {
            s.codes.remove(0);
        }
        s.codes.push((secret_hash(&plain), expires_at));
        let shown = format!("{}-{}", &plain[..5], &plain[5..]);
        Ok((Secret::new(shown), expires_at))
    }

    /// Uses a code: true once for a valid code that has not expired. The
    /// code is gone after the call, also when it was expired.
    pub fn redeem_code(&self, code: &str, now: i64) -> bool {
        let plain = normalize(code);
        if plain.len() != CODE_LEN {
            return false;
        }
        let hash = secret_hash(&plain);
        let mut s = self.state();
        let found = s
            .codes
            .iter()
            .position(|(h, _)| ct_eq(h.as_bytes(), hash.as_bytes()));
        let valid = found.is_some_and(|i| s.codes[i].1 > now);
        if let Some(i) = found {
            s.codes.remove(i);
        }
        s.codes.retain(|(_, exp)| *exp > now);
        valid
    }

    /// Makes an elevation for the login `session_hash`. Returns it with its
    /// expiry time.
    ///
    /// # Errors
    /// [`RandomFailed`].
    pub fn issue(
        &self,
        session_hash: &str,
        now: i64,
    ) -> Result<(Secret<String>, i64), RandomFailed> {
        let mut bytes = [0u8; 32];
        SystemRandom::new()
            .fill(&mut bytes)
            .map_err(|_| RandomFailed)?;
        let token = URL_SAFE_NO_PAD.encode(bytes);
        let expires_at = now + ELEVATION_TTL_MS;
        let mut s = self.state();
        s.elevations.retain(|(_, _, exp)| *exp > now);
        if s.elevations.len() >= MAX_ELEVATIONS {
            s.elevations.remove(0);
        }
        s.elevations
            .push((secret_hash(&token), session_hash.to_owned(), expires_at));
        Ok((Secret::new(token), expires_at))
    }

    /// True when `token` is a live elevation that was made for the login
    /// `session_hash`. An elevation can be used again until it expires.
    pub fn check(&self, token: &str, session_hash: &str, now: i64) -> bool {
        let hash = secret_hash(token);
        let mut s = self.state();
        s.elevations.retain(|(_, _, exp)| *exp > now);
        s.elevations.iter().any(|(h, session, _)| {
            ct_eq(h.as_bytes(), hash.as_bytes())
                && ct_eq(session.as_bytes(), session_hash.as_bytes())
        })
    }

    /// Drops the elevations of a login that ended.
    pub fn end_session(&self, session_hash: &str) {
        self.state()
            .elevations
            .retain(|(_, session, _)| session != session_hash);
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn a_code_has_ten_characters_of_the_alphabet_shown_in_two_groups() {
        let e = Elevations::default();
        let (code, expires) = e.new_code(1000).unwrap();
        assert_eq!(expires, 1000 + CODE_TTL_MS);
        let text = code.expose();
        assert_eq!(text.len(), 11, "{text}");
        assert_eq!(text.as_bytes()[5], b'-');
        assert!(
            text.chars()
                .filter(|c| *c != '-')
                .all(|c| ALPHABET.contains(&(c as u8)))
        );
        for banned in ['I', 'O', '0', '1'] {
            assert!(!text.contains(banned));
        }
    }

    #[test]
    fn a_code_is_single_use_and_ignores_case_and_the_dash() {
        let e = Elevations::default();
        let (code, _) = e.new_code(0).unwrap();
        let typed = code.expose().to_lowercase().replace('-', " ");
        assert!(e.redeem_code(&typed, 10));
        assert!(!e.redeem_code(code.expose(), 11));
    }

    #[test]
    fn a_code_expires_after_five_minutes_and_a_wrong_code_fails() {
        let e = Elevations::default();
        let (code, _) = e.new_code(0).unwrap();
        assert!(!e.redeem_code("AAAAA-AAAAA", 1));
        assert!(!e.redeem_code("short", 1));
        assert!(!e.redeem_code(code.expose(), CODE_TTL_MS));
        // The expired code is gone, not only refused.
        assert!(!e.redeem_code(code.expose(), 1));
        let (fresh, _) = e.new_code(0).unwrap();
        assert!(e.redeem_code(fresh.expose(), CODE_TTL_MS - 1));
    }

    #[test]
    fn an_elevation_is_bound_to_its_session_and_expires_after_ten_minutes() {
        let e = Elevations::default();
        let (token, expires) = e.issue("session-a", 0).unwrap();
        assert_eq!(expires, ELEVATION_TTL_MS);
        assert_eq!(URL_SAFE_NO_PAD.decode(token.expose()).unwrap().len(), 32);
        assert!(e.check(token.expose(), "session-a", 1));
        // It can be used again.
        assert!(e.check(token.expose(), "session-a", 2));
        assert!(!e.check(token.expose(), "session-b", 2));
        assert!(!e.check("not-a-token", "session-a", 2));
        assert!(!e.check(token.expose(), "session-a", ELEVATION_TTL_MS));
    }

    #[test]
    fn a_login_that_ends_loses_its_elevations() {
        let e = Elevations::default();
        let (a, _) = e.issue("a", 0).unwrap();
        let (b, _) = e.issue("b", 0).unwrap();
        e.end_session("a");
        assert!(!e.check(a.expose(), "a", 1));
        assert!(e.check(b.expose(), "b", 1));
    }

    #[test]
    fn the_stores_are_bounded() {
        let e = Elevations::default();
        let (first, _) = e.new_code(0).unwrap();
        for _ in 0..MAX_CODES {
            e.new_code(0).unwrap();
        }
        assert!(!e.redeem_code(first.expose(), 1));
    }
}
