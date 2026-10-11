//! The receipts of `coordinator.call` (M2 P3-7b; G7, ADR-004 §9.3, §9.6).
//!
//! A station sends `coordinator.call` for a coordinator agent. The deck
//! decides it with the rules of the station's receipt store:
//!
//! 1. Another `target_incarnation` (the deck database was replaced) →
//!    `incarnation_mismatch`, not stored.
//! 2. In the decision transaction: the station must still be paired and
//!    hold the `coordinate` grant, else `forbidden`, not stored (G5).
//! 3. A stored receipt with the same method, caller and payload hash → that
//!    receipt (also after `retry_until`); the same id with another method,
//!    caller or payload → `command_id_reused`, not stored.
//! 4. A passed `retry_until` → `expired`, not stored.
//! 5. Else the deck decides and stores the receipt. The coordinator
//!    operations (spawn, send, stop, assign) come with PLAN Phase 6; until
//!    then every new call is a stored rejection `unsupported` (DL-053).

use std::collections::BTreeSet;

use fleetdeck_proto::link::Scope;
use fleetdeck_proto::mutation::{CoordinatorCallEnvelope, Mutation};
use fleetdeck_proto::{Receipt, Reject, RejectCode, StationId};
use ring::digest::{SHA256, digest};
use rusqlite::{OptionalExtension, params};

use super::{DeckStore, Result, call};

const METHOD: &str = "coordinator.call";

fn sha256_hex(text: &str) -> String {
    digest(&SHA256, text.as_bytes())
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

impl DeckStore {
    /// Decides one `coordinator.call` from `station` and returns its
    /// receipt (see the module rules).
    ///
    /// # Errors
    /// A database error.
    pub async fn decide_coordinator(
        &self,
        station: StationId,
        envelope: CoordinatorCallEnvelope,
    ) -> Result<Receipt> {
        let now = self.now_ms();
        let incarnation = self.incarnation().clone();
        let caller = format!("{}/{}", station, envelope.call.caller);
        let other_deck = envelope.target_incarnation != incarnation;
        let mutation = Mutation::Coordinator(envelope);
        let command_id = mutation.command_id().clone();
        // Another deck incarnation: the call was meant for a replaced
        // database; nothing of it is stored here.
        if other_deck && let Err(reject) = mutation.admit(now, &incarnation) {
            return Ok(Receipt::rejected(command_id, reject, now));
        }
        let admitted = mutation.admit(now, &incarnation);
        let payload = sha256_hex(&mutation.canonical_payload());
        let receipt = call(&self.inner.db, move |conn| {
            let rejected = |code, message: &str| {
                Receipt::rejected(command_id.clone(), Reject::new(code, message), now)
            };
            let tx = conn.transaction()?;
            let scopes: Option<String> = tx
                .query_row(
                    "SELECT scopes FROM stations WHERE station_id = ?1",
                    [station.as_str()],
                    |r| r.get(0),
                )
                .optional()?;
            let granted = match scopes {
                Some(text) => {
                    serde_json::from_str::<BTreeSet<Scope>>(&text)?.contains(&Scope::Coordinate)
                }
                None => false,
            };
            if !granted {
                return Ok(rejected(
                    RejectCode::Forbidden,
                    "the station is not paired or has no `coordinate` grant",
                ));
            }
            let prior: Option<(String, String, String, String)> = tx
                .query_row(
                    "SELECT method, caller, payload_sha256, receipt FROM coordinator_receipts
                     WHERE command_id = ?1",
                    [command_id.as_str()],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                )
                .optional()?;
            if let Some((method, prior_caller, prior_payload, receipt)) = prior {
                if method == METHOD && prior_caller == caller && prior_payload == payload {
                    return Ok(serde_json::from_str(&receipt)?);
                }
                return Ok(rejected(
                    RejectCode::CommandIdReused,
                    "this command id was used for another method, payload or caller",
                ));
            }
            // A passed `retry_until` with no stored receipt: not stored.
            if let Err(reject) = admitted {
                return Ok(Receipt::rejected(command_id.clone(), reject, now));
            }
            let receipt = rejected(
                RejectCode::Unsupported,
                "coordinator operations come with PLAN Phase 6",
            );
            tx.execute(
                "INSERT INTO coordinator_receipts
                     (command_id, station_id, method, caller, payload_sha256, receipt, at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    command_id.as_str(),
                    station.as_str(),
                    METHOD,
                    caller,
                    payload,
                    serde_json::to_string(&receipt)?,
                    now
                ],
            )?;
            tx.commit()?;
            Ok(receipt)
        })
        .await?;
        Ok(receipt)
    }

    /// The number of stored coordinator receipts.
    ///
    /// # Errors
    /// A database error.
    pub async fn coordinator_receipts(&self) -> Result<u64> {
        call(&self.inner.db, |conn| {
            let n: i64 = conn.query_row("SELECT COUNT(*) FROM coordinator_receipts", [], |r| {
                r.get(0)
            })?;
            super::to_u64(n)
        })
        .await
    }
}
