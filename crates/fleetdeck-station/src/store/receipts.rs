//! Command decisions and receipts (PLAN §6.4, §8.4, G3).
//!
//! [`decide`] runs on the DB thread in one transaction:
//! 1. A `target_incarnation` that is not this station's is rejected with no
//!    receipt (G3).
//! 2. A stored receipt with the same `command_id`: the same method, caller
//!    and payload return it unchanged (`duplicate`); anything else is
//!    rejected with `command_id_reused` and the stored receipt stays.
//! 3. Admission (`retry_until`, G3). `expired` stores no receipt; other
//!    rejections do.
//! 4. The decision runs inside a savepoint. A rejection rolls the decision's
//!    writes back and stores a rejected receipt; an acceptance keeps them and
//!    stores an accepted receipt at the last committed `station_seq`.
//!
//! A database error aborts everything: no events, no receipt.

use fleetdeck_core::IdSource;
use fleetdeck_core::identity::Fingerprint;
use fleetdeck_proto::RequestId;
use fleetdeck_proto::command::{CommandResult, ReceiptState};
use fleetdeck_proto::event::ResolvedBy;
use fleetdeck_proto::link::Scope;
use fleetdeck_proto::mutation::Mutation;
use fleetdeck_proto::{
    CommandId, EventEnvelope, IncarnationId, Receipt, Reject, RejectCode, SessionId, SessionSummary,
};
use ring::digest::{SHA256, digest};
use rusqlite::{Connection, OptionalExtension, params};

use super::effects::{self, Effect, EffectPayload, ProcessIncarnation};
use super::requests::{self, AnswerClaim, RuntimeRequest};
use super::{NewEvent, NewSession, Result, StoreError, log, to_i64};

/// The authenticated identity that sent a command: a reused `command_id`
/// from another caller is rejected. A link caller also names the grant that
/// the method needs; the decision transaction checks that the caller's pin
/// and that grant still exist (G5).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Caller {
    name: String,
    grant: Option<(Fingerprint, Scope)>,
}

impl Caller {
    /// True for a paired deck peer, false for the station-local CLI.
    #[must_use]
    pub fn is_peer(&self) -> bool {
        self.grant.is_some()
    }

    /// The station-local CLI (owner on the station machine).
    #[must_use]
    pub fn local() -> Self {
        Self {
            name: "local".to_owned(),
            grant: None,
        }
    }

    /// A paired deck on a link; the method needs `scope`.
    #[must_use]
    pub fn peer(fingerprint: Fingerprint, scope: Scope) -> Self {
        Self {
            name: format!("deck:{fingerprint}"),
            grant: Some((fingerprint, scope)),
        }
    }

    /// A named caller without a grant check (store tests).
    #[cfg(test)]
    #[must_use]
    pub fn deck(key: &str) -> Self {
        Self {
            name: format!("deck:{key}"),
            grant: None,
        }
    }

    /// The name that receipts store.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }
}

/// Why a decision did not accept the command.
#[derive(Debug, thiserror::Error)]
pub enum DecideError {
    /// A rejection: stored as the command's receipt.
    #[error(transparent)]
    Reject(#[from] Reject),
    /// A store failure: nothing is stored.
    #[error(transparent)]
    Store(#[from] StoreError),
}

impl From<rusqlite::Error> for DecideError {
    fn from(e: rusqlite::Error) -> Self {
        Self::Store(e.into())
    }
}

/// The result of [`super::Store::decide`].
#[derive(Debug, Clone, PartialEq)]
pub struct Decided {
    pub receipt: Receipt,
    /// True when the receipt was stored earlier: nothing was done now.
    pub duplicate: bool,
    /// The events that this decision committed.
    pub events: Vec<EventEnvelope>,
    /// The effects that this decision queued.
    pub effects: Vec<Effect>,
}

/// The write handle of one command decision.
pub struct Decision<'a> {
    conn: &'a Connection,
    ids: &'a dyn IdSource,
    now: i64,
    command_id: CommandId,
    events: Vec<EventEnvelope>,
    effects: Vec<Effect>,
}

impl std::fmt::Debug for Decision<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Decision")
            .field("now", &self.now)
            .field("events", &self.events.len())
            .field("effects", &self.effects.len())
            .finish_non_exhaustive()
    }
}

impl Decision<'_> {
    /// The decision time (Unix ms).
    #[must_use]
    pub fn now(&self) -> i64 {
        self.now
    }

    /// The decision's transaction, for the tables of later phases (effects).
    #[must_use]
    pub fn connection(&self) -> &Connection {
        self.conn
    }

    /// A new id from the station's id source.
    #[must_use]
    pub fn new_id<I: From<String>>(&self) -> I {
        fleetdeck_core::new_id(self.ids)
    }

    /// The number of sessions that have not ended (`max_sessions` policy).
    ///
    /// # Errors
    /// A database error.
    pub fn active_sessions(&self) -> Result<u64> {
        let n: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM sessions WHERE state NOT IN ('exited', 'lost')",
            [],
            |r| r.get(0),
        )?;
        Ok(u64::try_from(n).unwrap_or(0))
    }

    /// A session's current projection.
    ///
    /// # Errors
    /// A database error.
    pub fn summary(&self, session_id: &SessionId) -> Result<Option<SessionSummary>> {
        log::summary(self.conn, session_id)
    }

    /// Creates a session row.
    ///
    /// # Errors
    /// [`StoreError::SessionExists`] or a database error.
    pub fn create_session(&mut self, session: &NewSession) -> Result<()> {
        log::insert_session(self.conn, session, self.now)
    }

    /// Appends one event in the decision's transaction.
    ///
    /// # Errors
    /// [`StoreError::UnknownSession`] or a database error.
    pub fn append(&mut self, session_id: &SessionId, event: NewEvent) -> Result<&EventEnvelope> {
        let envelope = log::append_one(self.conn, session_id, event, self.now)?;
        self.events.push(envelope);
        self.events
            .last()
            .ok_or_else(|| StoreError::Corrupt("the appended event is missing".to_owned()))
    }

    /// Runs a store helper that appends events in the decision's
    /// transaction; the events become part of the decision.
    pub(super) fn with_log<T>(
        &mut self,
        write: impl FnOnce(&Connection, i64, &mut Vec<EventEnvelope>) -> Result<T>,
    ) -> Result<T> {
        write(self.conn, self.now, &mut self.events)
    }

    /// The live provider process of a session, if any.
    ///
    /// # Errors
    /// A database error.
    pub fn live_process(&self, session_id: &SessionId) -> Result<Option<ProcessIncarnation>> {
        effects::live_process(self.conn, session_id)
    }

    /// Queues one effect in the decision's transaction (§6.4). An effect
    /// that acts on a process targets the session's live process.
    ///
    /// # Errors
    /// [`StoreError::NoProcess`] or a database error.
    pub fn enqueue(&mut self, session_id: &SessionId, payload: EffectPayload) -> Result<&Effect> {
        let effect = effects::enqueue(
            self.conn,
            self.ids,
            self.now,
            Some(&self.command_id),
            session_id,
            payload,
        )?;
        self.effects.push(effect);
        self.effects
            .last()
            .ok_or_else(|| StoreError::Corrupt("the queued effect is missing".to_owned()))
    }

    /// A runtime request (approval, question).
    ///
    /// # Errors
    /// A database or JSON error.
    pub fn request(&self, request_id: &RequestId) -> Result<Option<RuntimeRequest>> {
        requests::request(self.conn, request_id)
    }

    /// Records this command as the first answer of a request (L3).
    ///
    /// # Errors
    /// A database or JSON error.
    pub fn claim_answer(
        &mut self,
        request_id: &RequestId,
        option_id: &str,
        by: ResolvedBy,
    ) -> Result<AnswerClaim> {
        requests::claim_answer(
            self.conn,
            request_id,
            option_id,
            by,
            Some(&self.command_id),
            self.now,
        )
    }
}

/// A stored receipt with its identity columns.
pub(super) struct StoredReceipt {
    pub method: String,
    pub caller: String,
    pub payload_sha256: String,
    pub receipt: Receipt,
}

pub(super) fn stored(conn: &Connection, command_id: &CommandId) -> Result<Option<StoredReceipt>> {
    let row: Option<(String, String, String, String)> = conn
        .query_row(
            "SELECT method, caller, payload_sha256, receipt FROM commands WHERE command_id = ?1",
            [command_id.as_str()],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?;
    row.map(|(method, caller, payload_sha256, receipt)| {
        Ok(StoredReceipt {
            method,
            caller,
            payload_sha256,
            receipt: serde_json::from_str(&receipt)?,
        })
    })
    .transpose()
}

fn sha256_hex(text: &str) -> String {
    use std::fmt::Write;
    digest(&SHA256, text.as_bytes())
        .as_ref()
        .iter()
        .fold(String::with_capacity(64), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
}

fn store_receipt(
    conn: &Connection,
    receipt: &Receipt,
    method: &str,
    caller: &Caller,
    payload_sha256: &str,
) -> Result<()> {
    let state = match receipt.state {
        ReceiptState::Accepted => "accepted",
        ReceiptState::Rejected => "rejected",
    };
    conn.execute(
        "INSERT INTO commands
             (command_id, method, caller, payload_sha256, state, station_seq, receipt, at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            receipt.command_id.as_str(),
            method,
            caller.name,
            payload_sha256,
            state,
            receipt.station_seq.map(to_i64).transpose()?,
            serde_json::to_string(receipt)?,
            receipt.at,
        ],
    )?;
    Ok(())
}

fn not_stored(mutation: &Mutation, reject: Reject, now: i64) -> Decided {
    Decided {
        receipt: Receipt::rejected(mutation.command_id().clone(), reject, now),
        duplicate: false,
        events: Vec::new(),
        effects: Vec::new(),
    }
}

pub(super) fn decide<F>(
    conn: &mut Connection,
    ids: &dyn IdSource,
    incarnation: &IncarnationId,
    now: i64,
    mutation: &Mutation,
    caller: &Caller,
    decide: F,
) -> Result<Decided>
where
    F: FnOnce(&mut Decision<'_>) -> Result<CommandResult, DecideError>,
{
    if let Some(target) = target_of(mutation)
        && target != incarnation
        && let Err(reject) = mutation.admit(now, incarnation)
    {
        return Ok(not_stored(mutation, reject, now));
    }
    let command_id = mutation.command_id();
    let method = mutation.method();
    let payload_sha256 = sha256_hex(&mutation.canonical_payload());
    let mut tx = conn.transaction()?;
    // G5: the caller's pin and grant, read in the decision transaction. A
    // command that was queued on a link whose key was revoked meanwhile is
    // refused here; no receipt is stored for a caller that is not paired.
    if let Some((fingerprint, scope)) = &caller.grant
        && !super::peers::granted(&tx, fingerprint, *scope)?
    {
        let reject = Reject::new(
            RejectCode::Forbidden,
            format!(
                "the caller is not paired or has no `{}` grant",
                scope.name()
            ),
        );
        return Ok(not_stored(mutation, reject, now));
    }
    if let Some(prior) = stored(&tx, command_id)? {
        if prior.method == method
            && prior.caller == caller.name
            && prior.payload_sha256 == payload_sha256
        {
            return Ok(Decided {
                receipt: prior.receipt,
                duplicate: true,
                events: Vec::new(),
                effects: Vec::new(),
            });
        }
        let reject = Reject::new(
            RejectCode::CommandIdReused,
            format!(
                "command {} was used for another method, payload or caller",
                command_id
            ),
        );
        return Ok(not_stored(mutation, reject, now));
    }
    if let Err(reject) = mutation.admit(now, incarnation) {
        if !reject.code.stores_receipt() {
            return Ok(not_stored(mutation, reject, now));
        }
        let receipt = Receipt::rejected(command_id.clone(), reject, now);
        store_receipt(&tx, &receipt, method, caller, &payload_sha256)?;
        tx.commit()?;
        return Ok(Decided {
            receipt,
            duplicate: false,
            events: Vec::new(),
            effects: Vec::new(),
        });
    }
    let (outcome, events, queued) = {
        let sp = tx.savepoint()?;
        let mut decision = Decision {
            conn: &sp,
            ids,
            now,
            command_id: command_id.clone(),
            events: Vec::new(),
            effects: Vec::new(),
        };
        let outcome = decide(&mut decision);
        let events = std::mem::take(&mut decision.events);
        let queued = std::mem::take(&mut decision.effects);
        match outcome {
            Ok(result) => {
                sp.commit()?;
                (Ok(result), events, queued)
            }
            // Dropping the savepoint rolls the decision's writes back.
            Err(DecideError::Reject(reject)) => (Err(reject), Vec::new(), Vec::new()),
            Err(DecideError::Store(e)) => return Err(e),
        }
    };
    let receipt = match outcome {
        Ok(result) => {
            let seq = events
                .last()
                .map_or_else(|| log::head(&tx), |e| Ok(e.station_seq))?;
            Receipt::accepted(command_id.clone(), seq, result, now)
        }
        Err(reject) => Receipt::rejected(command_id.clone(), reject, now),
    };
    store_receipt(&tx, &receipt, method, caller, &payload_sha256)?;
    crate::fault::hit(crate::fault::FaultPoint::BeforeCommit, method);
    tx.commit()?;
    crate::fault::hit(crate::fault::FaultPoint::AfterCommit, method);
    Ok(Decided {
        receipt,
        duplicate: false,
        events,
        effects: queued,
    })
}

/// The `target_incarnation` of an envelope (optional only for a session
/// command of the local socket).
fn target_of(mutation: &Mutation) -> Option<&IncarnationId> {
    match mutation {
        Mutation::Session(e) => e.target_incarnation.as_ref(),
        Mutation::Configure(e) => Some(&e.target_incarnation),
        Mutation::RotateKey(e) => Some(&e.target_incarnation),
        Mutation::Coordinator(e) => Some(&e.target_incarnation),
    }
}
