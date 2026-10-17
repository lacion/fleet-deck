//! Pairing of a wall display by an on-screen code (R13 §5A item 7, §5B item
//! 2, §5C item 1): `pending` -> `approved` -> claimed (the row is deleted),
//! or expired.
//!
//! The kiosk asks for a pairing and gets a 6-digit code and a secret in a
//! cookie; the deck stores only the hash of the secret (the verifier). The
//! owner approves by pairing id and code. The kiosk's next poll turns the
//! secret into a display session in one transaction.

use fleetdeck_core::Secret;
use fleetdeck_core::secret::ct_eq;
use fleetdeck_proto::display::PendingPairing;
use ring::rand::{SecureRandom, SystemRandom};
use rusqlite::{Connection, OptionalExtension, params};

use super::auth::{random_token, secret_hash};
use super::displays::{DisplayRecord, NewDisplay, check_ceiling, clean_name, insert};
use super::{DeckError, DeckStore, Result, call};

/// A pending pairing lasts 5 minutes.
pub const PAIRING_PENDING_MS: i64 = 5 * 60 * 1000;
/// An approved pairing lasts 2 minutes.
pub const PAIRING_APPROVED_MS: i64 = 2 * 60 * 1000;
/// The most pairing rows at a time.
pub const MAX_PAIRINGS: i64 = 10;
/// A code that is taken is tried again this many times.
pub const CODE_TRIES: usize = 5;
/// The longest stored user agent, in characters.
pub const MAX_USER_AGENT: usize = 120;

const PENDING_PREFIX: &str = "fdpend1_";

/// The answer of [`DeckStore::pairing_approve_new`].
#[derive(Debug)]
pub enum Approval {
    Approved(DisplayRecord),
    /// No pending pairing with this id (unknown, expired, or approved
    /// already).
    Unknown,
    /// The code does not match.
    Mismatch,
}

/// The answer of [`DeckStore::pairing_claim`].
#[derive(Debug, PartialEq, Eq)]
pub enum Claim {
    /// The pairing waits for the owner.
    Pending,
    /// The pairing was approved and is consumed: the display id.
    Claimed(String),
    /// No such pairing for this secret.
    Unknown,
}

/// A stored pairing, without its verifier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairingRecord {
    pub pairing_id: String,
    pub code: String,
    pub approved: bool,
    pub expires_at: i64,
}

/// Six random digits.
///
/// # Errors
/// The random source failed.
pub fn random_code() -> Result<String> {
    loop {
        let mut bytes = [0u8; 4];
        SystemRandom::new()
            .fill(&mut bytes)
            .map_err(|_| DeckError::Hash("the system random source failed".to_owned()))?;
        let n = u32::from_be_bytes(bytes);
        // Rejection keeps the digits uniform.
        if n < 4_294_000_000 {
            return Ok(format!("{:06}", n % 1_000_000));
        }
    }
}

fn prune(conn: &Connection, now: i64) -> Result<usize> {
    Ok(conn.execute("DELETE FROM display_pairings WHERE expires_at <= ?1", [now])?)
}

impl DeckStore {
    /// Starts a pairing: `(pairing_id, code, pending_secret)`. The secret
    /// (`fdpend1_…`) goes to the kiosk once.
    ///
    /// # Errors
    /// [`DeckError::Busy`] at 10 pairing rows or after 5 codes that are
    /// taken; a database error.
    pub async fn pairing_create(
        &self,
        source: String,
        user_agent: String,
    ) -> Result<(String, String, Secret<String>)> {
        self.pairing_create_with(source, user_agent, random_code)
            .await
    }

    /// [`Self::pairing_create`] with the source of the codes (a seam for the
    /// code collision test).
    ///
    /// # Errors
    /// As [`Self::pairing_create`].
    pub async fn pairing_create_with(
        &self,
        source: String,
        user_agent: String,
        mut next_code: impl FnMut() -> Result<String> + Send + 'static,
    ) -> Result<(String, String, Secret<String>)> {
        let id: String = fleetdeck_core::ids::new_id(self.inner.ids.as_ref());
        let secret = format!("{PENDING_PREFIX}{}", random_token()?);
        let verifier = secret_hash(&secret);
        let user_agent: String = user_agent.chars().take(MAX_USER_AGENT).collect();
        let now = self.now_ms();
        let row_id = id.clone();
        let code = call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            prune(&tx, now)?;
            let rows: i64 =
                tx.query_row("SELECT count(*) FROM display_pairings", [], |r| r.get(0))?;
            if rows >= MAX_PAIRINGS {
                return Err(DeckError::Busy("too many display pairings".to_owned()));
            }
            for _ in 0..CODE_TRIES {
                let code = next_code()?;
                let n = tx.execute(
                    "INSERT OR IGNORE INTO display_pairings
                     (pairing_id, code, verifier_hash, state, source, user_agent,
                      display_id, created_at, expires_at)
                     VALUES (?1, ?2, ?3, 'pending', ?4, ?5, NULL, ?6, ?7)",
                    params![
                        row_id,
                        code,
                        verifier,
                        source,
                        user_agent,
                        now,
                        now + PAIRING_PENDING_MS
                    ],
                )?;
                if n == 1 {
                    tx.commit()?;
                    return Ok(code);
                }
            }
            Err(DeckError::Busy("no free pairing code".to_owned()))
        })
        .await?;
        Ok((id, code, Secret::new(secret)))
    }

    /// A pairing that has not expired.
    ///
    /// # Errors
    /// A database error.
    pub async fn pairing_get(&self, pairing_id: String) -> Result<Option<PairingRecord>> {
        let now = self.now_ms();
        call(&self.inner.db, move |conn| {
            Ok(conn
                .query_row(
                    "SELECT pairing_id, code, state, expires_at FROM display_pairings
                     WHERE pairing_id = ?1 AND expires_at > ?2",
                    params![pairing_id, now],
                    |r| {
                        Ok(PairingRecord {
                            pairing_id: r.get(0)?,
                            code: r.get(1)?,
                            approved: r.get::<_, String>(2)? == "approved",
                            expires_at: r.get(3)?,
                        })
                    },
                )
                .optional()?)
        })
        .await
    }

    /// The pairings that wait for the owner, oldest first (at most 10).
    ///
    /// # Errors
    /// A database error.
    pub async fn pairing_pending(&self) -> Result<Vec<PendingPairing>> {
        let now = self.now_ms();
        call(&self.inner.db, move |conn| {
            let mut stmt = conn.prepare(
                "SELECT pairing_id, code, source, user_agent, created_at, expires_at
                 FROM display_pairings WHERE state = 'pending' AND expires_at > ?1
                 ORDER BY created_at, pairing_id LIMIT ?2",
            )?;
            let rows = stmt.query_map(params![now, MAX_PAIRINGS], |r| {
                Ok(PendingPairing {
                    pairing_id: r.get(0)?,
                    code: r.get(1)?,
                    source: r.get(2)?,
                    user_agent: r.get(3)?,
                    created_at: r.get(4)?,
                    expires_at: r.get(5)?,
                    expected_display_id: None,
                })
            })?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })
        .await
    }

    /// The owner approves a pending pairing by id and code: one transaction
    /// makes the display and moves the pairing to `approved` (2 minutes).
    /// The verifier stays. The name and ceiling are checked first.
    ///
    /// # Errors
    /// [`DeckError::Invalid`] for a bad name or ceiling; a database error.
    pub async fn pairing_approve_new(
        &self,
        pairing_id: String,
        code: String,
        new: NewDisplay,
    ) -> Result<Approval> {
        let new = NewDisplay {
            name: clean_name(&new.name)?,
            ..new
        };
        let display_id: String = fleetdeck_core::ids::new_id(self.inner.ids.as_ref());
        let now = self.now_ms();
        call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            check_ceiling(&tx, &new.ceiling)?;
            let found: Option<String> = tx
                .query_row(
                    "SELECT code FROM display_pairings
                     WHERE pairing_id = ?1 AND state = 'pending' AND expires_at > ?2",
                    params![pairing_id, now],
                    |r| r.get(0),
                )
                .optional()?;
            let Some(stored) = found else {
                return Ok(Approval::Unknown);
            };
            if !ct_eq(stored.as_bytes(), code.as_bytes()) {
                return Ok(Approval::Mismatch);
            }
            let record = insert(&tx, &display_id, &new, now)?;
            tx.execute(
                "UPDATE display_pairings SET state = 'approved', display_id = ?2, expires_at = ?3
                 WHERE pairing_id = ?1",
                params![pairing_id, display_id, now + PAIRING_APPROVED_MS],
            )?;
            tx.commit()?;
            Ok(Approval::Approved(record))
        })
        .await
    }

    /// The kiosk's poll. In one transaction: an approved pairing with the
    /// right secret is deleted and its display id returned.
    ///
    /// # Errors
    /// A database error.
    pub async fn pairing_claim(
        &self,
        pairing_id: String,
        pending_secret: &Secret<String>,
    ) -> Result<Claim> {
        let hash = secret_hash(pending_secret.expose());
        let now = self.now_ms();
        call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            let row: Option<(Option<String>, String, Option<String>)> = tx
                .query_row(
                    "SELECT verifier_hash, state, display_id FROM display_pairings
                     WHERE pairing_id = ?1 AND expires_at > ?2",
                    params![pairing_id, now],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .optional()?;
            let Some((Some(verifier), state, display_id)) = row else {
                return Ok(Claim::Unknown);
            };
            if !ct_eq(verifier.as_bytes(), hash.as_bytes()) {
                return Ok(Claim::Unknown);
            }
            let (true, Some(display_id)) = (state == "approved", display_id) else {
                return Ok(Claim::Pending);
            };
            tx.execute(
                "DELETE FROM display_pairings WHERE pairing_id = ?1",
                [&pairing_id],
            )?;
            tx.commit()?;
            Ok(Claim::Claimed(display_id))
        })
        .await
    }

    /// Deletes the pairings that expired before `now`. Returns how many.
    ///
    /// # Errors
    /// A database error.
    pub async fn pairing_prune(&self, now: i64) -> Result<usize> {
        call(&self.inner.db, move |conn| prune(conn, now)).await
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use std::time::Duration;

    use fleetdeck_core::identity::Identity;
    use fleetdeck_proto::StationId;
    use fleetdeck_testkit::Det;

    use super::*;
    use crate::store::stations::NewStation;

    async fn setup() -> (DeckStore, Det, StationId) {
        let det = Det::new();
        let s = DeckStore::open_in_memory(det.clock(), det.ids())
            .await
            .unwrap();
        let rec = s
            .pin_station(NewStation {
                key: Identity::generate().unwrap().public_key(),
                name: "alpha".to_owned(),
                scopes: std::collections::BTreeSet::from([fleetdeck_proto::link::Scope::Read]),
                dial_url: None,
            })
            .await
            .unwrap();
        (s, det, rec.station_id)
    }

    fn new_display(station: &StationId) -> NewDisplay {
        NewDisplay {
            name: "Wall".to_owned(),
            ceiling: vec![station.clone()],
        }
    }

    #[tokio::test]
    async fn a_code_that_is_taken_is_tried_again_five_times_then_busy() {
        let (s, _det, _) = setup().await;
        s.pairing_create_with("a".into(), String::new(), || Ok("111111".to_owned()))
            .await
            .unwrap();
        // The same code again and again: five tries, then busy.
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let seen = calls.clone();
        let err = s
            .pairing_create_with("b".into(), String::new(), move || {
                seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok("111111".to_owned())
            })
            .await
            .unwrap_err();
        assert!(matches!(err, DeckError::Busy(_)), "{err}");
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), CODE_TRIES);
        // A second try with a free code on the 3rd call works.
        let n = std::sync::atomic::AtomicUsize::new(0);
        let (_, code, _) = s
            .pairing_create_with("c".into(), String::new(), move || {
                Ok(if n.fetch_add(1, std::sync::atomic::Ordering::SeqCst) < 2 {
                    "111111".to_owned()
                } else {
                    "222222".to_owned()
                })
            })
            .await
            .unwrap();
        assert_eq!(code, "222222");
    }

    #[tokio::test]
    async fn the_eleventh_pairing_is_busy_and_an_expired_one_makes_room() {
        let (s, det, _) = setup().await;
        for i in 0..10 {
            s.pairing_create_with("a".into(), String::new(), move || Ok(format!("{i:06}")))
                .await
                .unwrap();
        }
        let err = s
            .pairing_create_with("a".into(), String::new(), || Ok("999999".to_owned()))
            .await
            .unwrap_err();
        assert!(matches!(err, DeckError::Busy(_)));
        det.advance(Duration::from_millis(PAIRING_PENDING_MS as u64 + 1));
        s.pairing_create_with("a".into(), String::new(), || Ok("999999".to_owned()))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn a_pairing_is_claimed_once_and_only_with_its_secret() {
        let (s, _det, station) = setup().await;
        let (id, code, secret) = s
            .pairing_create("1.2.3.4".into(), "ua".into())
            .await
            .unwrap();
        let wrong = Secret::new("fdpend1_nope".to_owned());
        assert_eq!(
            s.pairing_claim(id.clone(), &secret).await.unwrap(),
            Claim::Pending
        );
        assert_eq!(
            s.pairing_claim(id.clone(), &wrong).await.unwrap(),
            Claim::Unknown
        );
        assert!(matches!(
            s.pairing_approve_new(id.clone(), "000000x".into(), new_display(&station))
                .await
                .unwrap(),
            Approval::Mismatch
        ));
        let Approval::Approved(display) = s
            .pairing_approve_new(id.clone(), code.clone(), new_display(&station))
            .await
            .unwrap()
        else {
            panic!("approved");
        };
        // A second approval is refused.
        assert!(matches!(
            s.pairing_approve_new(id.clone(), code, new_display(&station))
                .await
                .unwrap(),
            Approval::Unknown
        ));
        assert_eq!(
            s.pairing_claim(id.clone(), &wrong).await.unwrap(),
            Claim::Unknown
        );
        assert_eq!(
            s.pairing_claim(id.clone(), &secret).await.unwrap(),
            Claim::Claimed(display.display_id)
        );
        assert_eq!(s.pairing_claim(id, &secret).await.unwrap(), Claim::Unknown);
    }

    #[tokio::test]
    async fn two_concurrent_claims_give_one_winner() {
        let (s, _det, station) = setup().await;
        let (id, code, secret) = s.pairing_create("a".into(), String::new()).await.unwrap();
        s.pairing_approve_new(id.clone(), code, new_display(&station))
            .await
            .unwrap();
        let (a, b) = tokio::join!(
            s.pairing_claim(id.clone(), &secret),
            s.pairing_claim(id.clone(), &secret)
        );
        let (a, b) = (a.unwrap(), b.unwrap());
        let wins = [&a, &b]
            .iter()
            .filter(|c| matches!(c, Claim::Claimed(_)))
            .count();
        assert_eq!(wins, 1, "{a:?} {b:?}");
    }

    #[tokio::test]
    async fn an_approved_pairing_expires_after_two_minutes() {
        let (s, det, station) = setup().await;
        let (id, code, secret) = s.pairing_create("a".into(), String::new()).await.unwrap();
        s.pairing_approve_new(id.clone(), code, new_display(&station))
            .await
            .unwrap();
        det.advance(Duration::from_millis(PAIRING_APPROVED_MS as u64 + 1));
        assert_eq!(s.pairing_claim(id, &secret).await.unwrap(), Claim::Unknown);
        assert_eq!(s.pairing_prune(s.now_ms()).await.unwrap(), 1);
    }
}
