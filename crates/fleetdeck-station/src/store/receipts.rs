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

use fleetdeck_proto::command::{CommandResult, ReceiptState};
use fleetdeck_proto::{
    CommandEnvelope, CommandId, EventEnvelope, IncarnationId, Receipt, Reject, RejectCode,
    SessionId, SessionSummary,
};
use ring::digest::{SHA256, digest};
use rusqlite::{Connection, OptionalExtension, params};

use super::log;
use super::{NewEvent, NewSession, Result, StoreError, to_i64};

/// The authenticated identity that sent a command: a reused `command_id`
/// from another caller is rejected.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Caller(pub String);

impl Caller {
    /// The station-local CLI (owner on the station machine).
    #[must_use]
    pub fn local() -> Self {
        Self("local".to_owned())
    }

    /// A paired deck, by its key.
    #[must_use]
    pub fn deck(key: &str) -> Self {
        Self(format!("deck:{key}"))
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
}

/// The write handle of one command decision.
pub struct Decision<'a> {
    conn: &'a Connection,
    now: i64,
    events: Vec<EventEnvelope>,
}

impl std::fmt::Debug for Decision<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Decision")
            .field("now", &self.now)
            .field("events", &self.events.len())
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
            caller.0,
            payload_sha256,
            state,
            receipt.station_seq.map(to_i64).transpose()?,
            serde_json::to_string(receipt)?,
            receipt.at,
        ],
    )?;
    Ok(())
}

fn not_stored(envelope: &CommandEnvelope, reject: Reject, now: i64) -> Decided {
    Decided {
        receipt: Receipt::rejected(envelope.command_id.clone(), reject, now),
        duplicate: false,
        events: Vec::new(),
    }
}

pub(super) fn decide<F>(
    conn: &mut Connection,
    incarnation: &IncarnationId,
    now: i64,
    envelope: &CommandEnvelope,
    caller: &Caller,
    decide: F,
) -> Result<Decided>
where
    F: FnOnce(&mut Decision<'_>) -> Result<CommandResult, DecideError>,
{
    if let Some(target) = &envelope.target_incarnation
        && target != incarnation
        && let Err(reject) = envelope.admit(now, incarnation)
    {
        return Ok(not_stored(envelope, reject, now));
    }
    let method = envelope.command.method();
    let payload_sha256 = sha256_hex(&envelope.canonical_payload());
    let mut tx = conn.transaction()?;
    if let Some(prior) = stored(&tx, &envelope.command_id)? {
        if prior.method == method
            && prior.caller == caller.0
            && prior.payload_sha256 == payload_sha256
        {
            return Ok(Decided {
                receipt: prior.receipt,
                duplicate: true,
                events: Vec::new(),
            });
        }
        let reject = Reject::new(
            RejectCode::CommandIdReused,
            format!(
                "command {} was used for another method, payload or caller",
                envelope.command_id
            ),
        );
        return Ok(not_stored(envelope, reject, now));
    }
    if let Err(reject) = envelope.admit(now, incarnation) {
        if !reject.code.stores_receipt() {
            return Ok(not_stored(envelope, reject, now));
        }
        let receipt = Receipt::rejected(envelope.command_id.clone(), reject, now);
        store_receipt(&tx, &receipt, method, caller, &payload_sha256)?;
        tx.commit()?;
        return Ok(Decided {
            receipt,
            duplicate: false,
            events: Vec::new(),
        });
    }
    let (outcome, events) = {
        let sp = tx.savepoint()?;
        let mut decision = Decision {
            conn: &sp,
            now,
            events: Vec::new(),
        };
        let outcome = decide(&mut decision);
        let events = std::mem::take(&mut decision.events);
        match outcome {
            Ok(result) => {
                sp.commit()?;
                (Ok(result), events)
            }
            // Dropping the savepoint rolls the decision's writes back.
            Err(DecideError::Reject(reject)) => (Err(reject), Vec::new()),
            Err(DecideError::Store(e)) => return Err(e),
        }
    };
    let receipt = match outcome {
        Ok(result) => {
            let seq = events
                .last()
                .map_or_else(|| log::head(&tx), |e| Ok(e.station_seq))?;
            Receipt::accepted(envelope.command_id.clone(), seq, result, now)
        }
        Err(reject) => Receipt::rejected(envelope.command_id.clone(), reject, now),
    };
    store_receipt(&tx, &receipt, method, caller, &payload_sha256)?;
    tx.commit()?;
    Ok(Decided {
        receipt,
        duplicate: false,
        events,
    })
}
