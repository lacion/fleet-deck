//! The digest rows of the shim-hosted session bindings and their
//! transitions (DL-194 §2.3, §9.1, §9.11, §9.15).
//!
//! - **Rows.** `shim_bindings` keeps the SHA-256 digest of each credential,
//!   never the credential, tied to the shim execution.
//! - **Compute, then apply.** [`transitions`] is pure: from the rows of a
//!   shim and one output batch it computes `bound` and `ended`. The output
//!   transaction ([`super::shims`]) writes the row changes and the
//!   lifecycle events ([`lifecycle_events`]) with the batch.
//! - **Memory.** A [`BindingSink`] registered on the store changes memory:
//!   `invalidate` runs inside the write closure before the commit,
//!   `activate` on the database thread right after it.

use std::sync::Arc;

use fleetdeck_proto::{Event, EventBody, SessionId};
use rusqlite::{Connection, OptionalExtension as _, Row, params};

use super::{NewEvent, Result, Store, StoreError, call};
use crate::coordinator::{BindingState, Bindings};

/// The reason of `coordinator.ended` at the end of the process (§5.5).
pub const REASON_PROCESS_EXIT: &str = "process_exit";
/// The reason of `coordinator.ended` when the root native identity changed.
pub const REASON_NATIVE_REPLACED: &str = "native_replaced";

const COLUMNS: &str = "digest, shim_id, session_id, binding_id, grant_id, provider_instance,
    native_id, generation, classes, scope, state, control_skill, expires_at, created_at";

/// The state of a stored binding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowState {
    Pending,
    Active,
    Ended,
    /// Refused until a resume gives a new token (DL-194 §2.3).
    Suspended,
}

impl RowState {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Active => "active",
            Self::Ended => "ended",
            Self::Suspended => "suspended",
        }
    }

    fn parse(s: &str) -> Result<Self> {
        Ok(match s {
            "pending" => Self::Pending,
            "active" => Self::Active,
            "ended" => Self::Ended,
            "suspended" => Self::Suspended,
            other => return Err(StoreError::Corrupt(format!("binding state {other}"))),
        })
    }
}

/// One `shim_bindings` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BindingRow {
    /// The lowercase hex SHA-256 of the credential.
    pub digest: String,
    pub shim_id: String,
    pub session_id: SessionId,
    pub binding_id: String,
    /// Empty for a fleet-only binding.
    pub grant_id: String,
    pub provider_instance: String,
    pub native_id: Option<String>,
    pub generation: u32,
    /// A JSON array of the class names.
    pub classes: String,
    /// The JSON of the grant scope.
    pub scope: String,
    pub state: RowState,
    /// The JSON of the control skill.
    pub control_skill: String,
    /// Unix ms; `None` for a fleet-only binding.
    pub expires_at: Option<i64>,
    pub created_at: i64,
}

impl BindingRow {
    /// True for a binding without a grant: the deck knows nothing of it.
    #[must_use]
    pub fn is_fleet_only(&self) -> bool {
        self.grant_id.is_empty()
    }
}

/// A root native identity that the adapter stated between the events of a
/// batch: `after` events of the batch come before it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdentitySignal {
    pub native_id: String,
    pub after: usize,
}

/// A change of one binding, computed by [`transitions`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Transition {
    Bound {
        digest: String,
        native_id: String,
        generation: u32,
    },
    Ended {
        digest: String,
        reason: String,
    },
}

/// The working state of a row while a batch is walked.
struct Work<'a> {
    row: &'a BindingRow,
    state: RowState,
    native: Option<String>,
}

fn bind(work: &mut [Work<'_>], native_id: &str, out: &mut Vec<Transition>) {
    for w in work.iter_mut().filter(|w| w.state == RowState::Pending) {
        w.state = RowState::Active;
        w.native = Some(native_id.to_owned());
        out.push(Transition::Bound {
            digest: w.row.digest.clone(),
            native_id: native_id.to_owned(),
            generation: 1,
        });
    }
}

fn identity(work: &mut [Work<'_>], native_id: &str, out: &mut Vec<Transition>) {
    bind(work, native_id, out);
    for w in work
        .iter_mut()
        .filter(|w| w.state == RowState::Active && w.native.as_deref() != Some(native_id))
    {
        w.state = RowState::Ended;
        out.push(Transition::Ended {
            digest: w.row.digest.clone(),
            reason: REASON_NATIVE_REPLACED.to_owned(),
        });
    }
}

/// The native id of a `session.started` event: `session_id`, else
/// `thread_id` of its provider refs.
fn started_native(event: &NewEvent) -> Option<&str> {
    let EventBody::Known(Event::SessionStarted { provider_refs, .. }) = &event.event else {
        return None;
    };
    provider_refs
        .get("session_id")
        .or_else(|| provider_refs.get("thread_id"))
        .and_then(serde_json::Value::as_str)
}

/// The transitions of one output batch (pure; no memory change). The walk
/// merges `events` and `identities` by `after`, keeps a working copy of
/// each row, and ends the open rows at the end of the batch when `exited`.
///
/// - `session.started` with a native id binds each pending row (generation
///   1).
/// - An identity signal binds each pending row, and ends each active row
///   whose native id differs (`native_replaced`).
/// - `ended` and `suspended` rows never change.
#[must_use]
pub fn transitions(
    rows: &[BindingRow],
    events: &[NewEvent],
    identities: &[IdentitySignal],
    exited: bool,
) -> Vec<Transition> {
    let mut work: Vec<Work<'_>> = rows
        .iter()
        .map(|row| Work {
            row,
            state: row.state,
            native: row.native_id.clone(),
        })
        .collect();
    let mut out = Vec::new();
    let mut next = 0;
    for (i, event) in events.iter().enumerate() {
        while next < identities.len() && identities[next].after <= i {
            identity(&mut work, &identities[next].native_id, &mut out);
            next += 1;
        }
        if let Some(native) = started_native(event) {
            bind(&mut work, native, &mut out);
        }
    }
    for signal in &identities[next..] {
        identity(&mut work, &signal.native_id, &mut out);
    }
    if exited {
        for w in work
            .iter_mut()
            .filter(|w| matches!(w.state, RowState::Pending | RowState::Active))
        {
            w.state = RowState::Ended;
            out.push(Transition::Ended {
                digest: w.row.digest.clone(),
                reason: REASON_PROCESS_EXIT.to_owned(),
            });
        }
    }
    out
}

/// The lifecycle events of `ts` (`coordinator.bound`, `coordinator.ended`).
/// A fleet-only row gives no event. An `ended` event carries the
/// generation that the row has at that point of the sequence.
#[must_use]
pub fn lifecycle_events(rows: &[BindingRow], ts: &[Transition]) -> Vec<NewEvent> {
    let mut generations: Vec<(&str, u32)> = rows
        .iter()
        .map(|r| (r.digest.as_str(), r.generation))
        .collect();
    let mut out = Vec::new();
    for t in ts {
        let digest = match t {
            Transition::Bound { digest, .. } | Transition::Ended { digest, .. } => digest,
        };
        let Some(row) = rows.iter().find(|r| &r.digest == digest) else {
            continue;
        };
        let Some(generation) = generations
            .iter_mut()
            .find(|(d, _)| *d == digest.as_str())
            .map(|(_, g)| g)
        else {
            continue;
        };
        match t {
            Transition::Bound {
                native_id,
                generation: bound,
                ..
            } => {
                *generation = *bound;
                if !row.is_fleet_only() {
                    out.push(NewEvent::from(Event::CoordinatorBound {
                        binding_id: row.binding_id.clone(),
                        grant_id: row.grant_id.clone(),
                        generation: *bound,
                        provider_instance: row.provider_instance.clone(),
                        native_id: native_id.clone(),
                    }));
                }
            }
            Transition::Ended { reason, .. } => {
                if !row.is_fleet_only() {
                    out.push(NewEvent::from(Event::CoordinatorEnded {
                        binding_id: row.binding_id.clone(),
                        generation: *generation,
                        reason: reason.clone(),
                    }));
                }
            }
        }
    }
    out
}

/// The memory side of the bindings, called by the output transaction.
pub trait BindingSink: Send + Sync {
    /// Called inside the write closure, before the commit: the credentials
    /// of the `Ended` transitions stop working at once. Memory stays
    /// invalidated when the transaction fails.
    fn invalidate(&self, ts: &[Transition]);
    /// Called on the database thread right after the commit, before the
    /// next job: the credentials of the `Bound` transitions become active.
    fn activate(&self, ts: &[Transition]);
}

/// A [`BindingSink`] over the station's [`Bindings`].
#[derive(Debug, Clone)]
pub struct MemorySink(pub Bindings);

impl BindingSink for MemorySink {
    fn invalidate(&self, ts: &[Transition]) {
        for t in ts {
            if let Transition::Ended { digest, .. } = t {
                self.0
                    .update_digest(digest, |b| b.state = BindingState::Revoked);
            }
        }
    }

    fn activate(&self, ts: &[Transition]) {
        for t in ts {
            if let Transition::Bound {
                digest,
                native_id,
                generation,
            } = t
            {
                // Never from a terminal state (DL-194 §9.15).
                self.0.update_digest(digest, |b| {
                    if b.state == BindingState::Pending {
                        b.state = BindingState::Active;
                        b.generation = *generation;
                        b.native_id = Some(native_id.clone());
                    }
                });
            }
        }
    }
}

fn row(r: &Row<'_>) -> rusqlite::Result<RawRow> {
    Ok(RawRow {
        digest: r.get(0)?,
        shim_id: r.get(1)?,
        session_id: r.get(2)?,
        binding_id: r.get(3)?,
        grant_id: r.get(4)?,
        provider_instance: r.get(5)?,
        native_id: r.get(6)?,
        generation: r.get(7)?,
        classes: r.get(8)?,
        scope: r.get(9)?,
        state: r.get(10)?,
        control_skill: r.get(11)?,
        expires_at: r.get(12)?,
        created_at: r.get(13)?,
    })
}

struct RawRow {
    digest: String,
    shim_id: String,
    session_id: String,
    binding_id: String,
    grant_id: String,
    provider_instance: String,
    native_id: Option<String>,
    generation: i64,
    classes: String,
    scope: String,
    state: String,
    control_skill: String,
    expires_at: Option<i64>,
    created_at: i64,
}

impl RawRow {
    fn decode(self) -> Result<BindingRow> {
        Ok(BindingRow {
            digest: self.digest,
            shim_id: self.shim_id,
            session_id: SessionId::from(self.session_id),
            binding_id: self.binding_id,
            grant_id: self.grant_id,
            provider_instance: self.provider_instance,
            native_id: self.native_id,
            generation: u32::try_from(self.generation)
                .map_err(|_| StoreError::Corrupt("binding generation".to_owned()))?,
            classes: self.classes,
            scope: self.scope,
            state: RowState::parse(&self.state)?,
            control_skill: self.control_skill,
            expires_at: self.expires_at,
            created_at: self.created_at,
        })
    }
}

/// The rows of one shim, in creation order.
pub(super) fn rows_of_shim(conn: &Connection, shim_id: &str) -> Result<Vec<BindingRow>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLUMNS} FROM shim_bindings WHERE shim_id = ?1 ORDER BY created_at, digest"
    ))?;
    let raws = stmt
        .query_map([shim_id], row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    raws.into_iter().map(RawRow::decode).collect()
}

/// Writes the row changes of `ts` (state, native id, generation).
pub(super) fn write_changes(conn: &Connection, ts: &[Transition]) -> Result<()> {
    for t in ts {
        match t {
            Transition::Bound {
                digest,
                native_id,
                generation,
            } => conn.execute(
                "UPDATE shim_bindings SET state = 'active', native_id = ?2, generation = ?3
                 WHERE digest = ?1",
                params![digest, native_id, generation],
            )?,
            Transition::Ended { digest, .. } => conn.execute(
                "UPDATE shim_bindings SET state = 'ended' WHERE digest = ?1",
                [digest],
            )?,
        };
    }
    Ok(())
}

impl Store {
    /// Registers the memory side of the bindings. A second call replaces
    /// the first sink.
    pub fn set_binding_sink(&self, sink: Arc<dyn BindingSink>) {
        *self
            .inner
            .binding_sink
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(sink);
    }

    pub(super) fn binding_sink(&self) -> Option<Arc<dyn BindingSink>> {
        self.inner
            .binding_sink
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Stores a binding row.
    ///
    /// # Errors
    /// A duplicate digest or a database error.
    pub async fn shim_binding_insert(&self, row: BindingRow) -> Result<()> {
        self.write(move |conn, _, _| {
            conn.execute(
                &format!(
                    "INSERT INTO shim_bindings ({COLUMNS})
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)"
                ),
                params![
                    row.digest,
                    row.shim_id,
                    row.session_id.as_str(),
                    row.binding_id,
                    row.grant_id,
                    row.provider_instance,
                    row.native_id,
                    row.generation,
                    row.classes,
                    row.scope,
                    row.state.as_str(),
                    row.control_skill,
                    row.expires_at,
                    row.created_at
                ],
            )?;
            Ok(())
        })
        .await
        .map(|((), _)| ())
    }

    /// Every binding row, in creation order.
    ///
    /// # Errors
    /// A database error, or a row that does not decode.
    pub async fn shim_bindings_load(&self) -> Result<Vec<BindingRow>> {
        call(&self.inner.db, move |conn| {
            let mut stmt = conn.prepare(&format!(
                "SELECT {COLUMNS} FROM shim_bindings ORDER BY created_at, digest"
            ))?;
            let raws = stmt
                .query_map([], row)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            raws.into_iter().map(RawRow::decode).collect()
        })
        .await
    }

    /// Deletes the rows of every shim that is not in `shim_ids`, in one
    /// transaction. Returns the number of deleted rows.
    ///
    /// # Errors
    /// A database error.
    pub async fn shim_bindings_retain(&self, shim_ids: Vec<String>) -> Result<usize> {
        self.write(move |conn, _, _| {
            let mut stmt = conn.prepare("SELECT DISTINCT shim_id FROM shim_bindings")?;
            let present = stmt
                .query_map([], |r| r.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            let mut deleted = 0;
            for shim_id in present.iter().filter(|id| !shim_ids.contains(id)) {
                deleted +=
                    conn.execute("DELETE FROM shim_bindings WHERE shim_id = ?1", [shim_id])?;
            }
            Ok(deleted)
        })
        .await
        .map(|(n, _)| n)
    }

    /// Sets the state of one row; `false` for an unknown digest.
    ///
    /// # Errors
    /// A database error.
    pub async fn shim_binding_set_state(&self, digest: String, state: RowState) -> Result<bool> {
        self.write(move |conn, _, _| {
            let n = conn.execute(
                "UPDATE shim_bindings SET state = ?2 WHERE digest = ?1",
                params![digest, state.as_str()],
            )?;
            Ok(n == 1)
        })
        .await
        .map(|(ok, _)| ok)
    }
}

impl super::Decision<'_> {
    /// Ends the row of `digest` in the decide transaction and returns the
    /// row as it was before, for its lifecycle event. `None` for an unknown
    /// digest, or a row that ended or is suspended already. The row stores
    /// no reason: `_reason` is for the caller's event.
    ///
    /// # Errors
    /// A database error.
    pub fn shim_binding_end(&mut self, digest: &str, _reason: &str) -> Result<Option<BindingRow>> {
        let conn = self.connection();
        let found = conn
            .query_row(
                &format!("SELECT {COLUMNS} FROM shim_bindings WHERE digest = ?1"),
                [digest],
                row,
            )
            .optional()?
            .map(RawRow::decode)
            .transpose()?;
        let Some(before) = found else {
            return Ok(None);
        };
        if matches!(before.state, RowState::Ended | RowState::Suspended) {
            return Ok(None);
        }
        conn.execute(
            "UPDATE shim_bindings SET state = 'ended' WHERE digest = ?1",
            [digest],
        )?;
        Ok(Some(before))
    }
}

#[cfg(test)]
#[path = "shim_bindings_tests.rs"]
mod tests;
