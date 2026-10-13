//! The receipts of `coordinator.call` (M2 P3-7b; G7, ADR-004 §9.3, §9.6).
//!
//! A station sends `coordinator.call` for a coordinator agent. The deck
//! decides it with the rules of the station's receipt store:
//!
//! 1. Another `target_incarnation` (the deck database was replaced) →
//!    `incarnation_mismatch`, not stored.
//! 2. In the decision transaction: the station must still be paired and
//!    hold the `coordinate` grant, else `forbidden`, not stored (G5).
//! 3. The grant of the caller (M3 6-5, ADR-006 §6.2 step 3): none, revoked,
//!    past `expires_at` or idle past its TTL → `forbidden`, not stored; a
//!    binding that differs from the deck's → `binding_stale`, not stored.
//!    Because this comes before the replay, a replay after a revocation
//!    never returns the old acceptance.
//!    A call without a grant id is the worker path (R2.3): only
//!    `ask_coordinator`, for a session of the calling station, at most ten
//!    per hour ([`super::coordinator_fleet::decide_worker`]); the grant
//!    checks do not apply, and the call is audited as `session:<id>`.
//! 4. A stored receipt with the same method, caller and payload hash → that
//!    receipt (also after `retry_until`); the same id with another method,
//!    caller or payload → `command_id_reused`, not stored.
//! 5. A passed `retry_until` → `expired`, not stored.
//! 6. Else the class, target and cap checks of [`super::coordinator_ops`]
//!    and the intent commit with the stored receipt; `last_used_at` moves
//!    only for an accepted new call.

use std::collections::BTreeSet;
use std::sync::Arc;

use fleetdeck_proto::command::CommandResult;
use fleetdeck_proto::link::Scope;
use fleetdeck_proto::mutation::{CoordinatorCallEnvelope, Mutation};
use fleetdeck_proto::{CommandId, Receipt, Reject, RejectCode, SessionId, StationId};
use ring::digest::{SHA256, digest};
use rusqlite::{OptionalExtension, params};

use super::coordinator_fleet::decide_worker;
use super::coordinator_ops::{Context, Decision, decide};
use super::grants::{GrantState, caller_grant};
use super::{AuditResult, DeckStore, NewAudit, Result, call, new_id};

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
        let caller_session = envelope.call.caller.clone();
        let binding = envelope.call.binding.clone();
        let op = envelope.call.op.clone();
        let ctx = Context {
            sessions: self.sessions().await?,
            infos: self
                .station_infos()
                .into_iter()
                .map(|(id, (info, _))| (id, info))
                .collect(),
            buried: self.undelivered_mail().await?,
            outbox_id: new_id(self.inner.ids.as_ref()),
        };
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
        let worker = binding.grant_id.is_empty();
        let inner = Arc::clone(&self.inner);
        let audit = (station.clone(), caller_session.clone(), command_id.clone());
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
            let grant = if worker {
                None
            } else {
                let Some(grant) = caller_grant(&tx, &station, &caller_session)? else {
                    return Ok(rejected(
                        RejectCode::Forbidden,
                        "the caller has no coordinator grant",
                    ));
                };
                if grant.state != GrantState::Active || now > grant.expires_at {
                    return Ok(rejected(
                        RejectCode::Forbidden,
                        "the coordinator grant is revoked or expired",
                    ));
                }
                if grant.binding.as_ref() != Some(&binding) {
                    return Ok(rejected(
                        RejectCode::BindingStale,
                        "the coordinator binding is not the deck's binding of the grant",
                    ));
                }
                if grant
                    .last_used_at
                    .is_some_and(|t| now.saturating_sub(t) > grant.idle_ttl_ms)
                {
                    tx.execute(
                        "UPDATE tokens SET state = 'expired', revoked_reason = 'idle'
                         WHERE grant_id = ?1 AND state = 'active'",
                        [grant.grant_id.as_str()],
                    )?;
                    tx.commit()?;
                    return Ok(rejected(
                        RejectCode::Forbidden,
                        "the coordinator grant expired after its idle time",
                    ));
                }
                Some(grant)
            };
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
            let decision = match &grant {
                Some(grant) => decide(&tx, grant, &op, &ctx, command_id.as_str(), now)?,
                None => decide_worker(
                    &tx,
                    &inner.ask_limits,
                    &ctx,
                    (&station, &caller_session),
                    &op,
                    command_id.as_str(),
                    now,
                )?,
            };
            let receipt = match decision {
                Decision::Accepted(session_id) => {
                    if let Some(grant) = &grant {
                        tx.execute(
                            "UPDATE tokens SET last_used_at = ?2 WHERE grant_id = ?1",
                            params![grant.grant_id, now],
                        )?;
                    }
                    Receipt::accepted(
                        command_id.clone(),
                        0,
                        CommandResult {
                            session_id,
                            input_id: None,
                        },
                        now,
                    )
                }
                Decision::Rejected(code, message) => rejected(code, &message),
            };
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
        if worker {
            self.audit_worker_ask(audit, &receipt).await;
        }
        Ok(receipt)
    }

    /// The audit row of a worker's `ask_coordinator` (R2.3): the actor is
    /// `session:<id>`; the question is not in the row.
    async fn audit_worker_ask(
        &self,
        (station, session, command_id): (StationId, SessionId, CommandId),
        receipt: &Receipt,
    ) {
        let row = match &receipt.reject {
            None => NewAudit::new(
                format!("session:{session}"),
                "coordinator.ask",
                AuditResult::Ok,
            ),
            Some(reject) => NewAudit::new(
                format!("session:{session}"),
                "coordinator.ask",
                AuditResult::Refused,
            )
            .detail(reject.message.clone()),
        };
        self.audit_or_log(
            row.target(format!("{station}/{session}"))
                .command(command_id),
        )
        .await;
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

#[cfg(test)]
#[path = "coordinator_tests.rs"]
mod tests;
