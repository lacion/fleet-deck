//! The store side of the binding start order, the revoke and the rotation
//! (DL-194 T4b, §2.3, §9.1, §9.13): the row form of a [`Binding`], the
//! expiry and the removal of rows at the station start, and the transaction
//! that ends the rows of a rotated binding. Each function that ends a
//! `pending` or `active` row appends `coordinator.ended` for a binding with
//! a grant, in the same transaction.

use std::collections::BTreeSet;

use fleetdeck_proto::command::GrantScope;
use fleetdeck_proto::{Event, EventEnvelope};
use rusqlite::{Connection, OptionalExtension as _, params};

use super::shim_bindings::{BindingRow, COLUMNS, RawRow, RowState, row};
use super::{NewEvent, Result, Store, StoreError, log};
use crate::coordinator::{Binding, BindingState, ToolClass};

/// The reason of `coordinator.ended` at the grant expiry (§9.1).
pub const REASON_EXPIRED: &str = "expired";
/// The reason of `coordinator.ended` when a resume rotated the credential.
pub const REASON_ROTATED: &str = "rotated";

impl BindingRow {
    /// The row of a binding. `Own` is never stored (every binding has it);
    /// `classes` is a JSON array of class names (the grant classes and the
    /// station-internal `recording`, R13 P5b-2), `scope` the JSON of
    /// the scope and `control_skill` the JSON string of the text. A
    /// binding that is not `Pending` or `Active` gives an `ended` row.
    #[must_use]
    pub fn from_binding(
        digest: String,
        shim_id: String,
        binding: &Binding,
        expires_at: Option<i64>,
        created_at: i64,
    ) -> Self {
        let names: Vec<&str> = binding
            .classes
            .iter()
            .filter(|c| **c != ToolClass::Own)
            .map(|c| c.name())
            .collect();
        Self {
            digest,
            shim_id,
            session_id: binding.session_id.clone(),
            binding_id: binding.binding_id.clone(),
            grant_id: binding.grant_id.clone(),
            provider_instance: binding.provider_instance.clone(),
            native_id: binding.native_id.clone(),
            generation: binding.generation,
            classes: serde_json::Value::from(names).to_string(),
            scope: match binding.scope {
                GrantScope::Lineage => "\"lineage\"",
                GrantScope::Fleet => "\"fleet\"",
            }
            .to_owned(),
            state: match binding.state {
                BindingState::Pending => RowState::Pending,
                BindingState::Active => RowState::Active,
                BindingState::Expired | BindingState::Revoked => RowState::Ended,
            },
            control_skill: serde_json::Value::String(binding.control_skill.clone()).to_string(),
            expires_at,
            created_at,
            kind: binding.kind,
        }
    }

    /// The binding of a row. `Own` is always added. An `ended` or
    /// `suspended` row gives a `Revoked` binding.
    ///
    /// # Errors
    /// The name of the column that does not parse: invalid JSON, an unknown
    /// class name, or a stored `own`. An old row has no `recording`, so it
    /// reads without the recording allowance.
    pub fn to_binding(&self) -> std::result::Result<Binding, String> {
        let names: Vec<String> =
            serde_json::from_str(&self.classes).map_err(|_| "classes".to_owned())?;
        let mut classes = BTreeSet::new();
        for name in &names {
            classes.insert(ToolClass::from_stored(name).ok_or_else(|| "classes".to_owned())?);
        }
        classes.insert(ToolClass::Own);
        let scope: GrantScope =
            serde_json::from_str(&self.scope).map_err(|_| "scope".to_owned())?;
        let control_skill: String =
            serde_json::from_str(&self.control_skill).map_err(|_| "control_skill".to_owned())?;
        Ok(Binding {
            binding_id: self.binding_id.clone(),
            grant_id: self.grant_id.clone(),
            session_id: self.session_id.clone(),
            provider_instance: self.provider_instance.clone(),
            native_id: self.native_id.clone(),
            generation: self.generation,
            classes,
            scope,
            state: match self.state {
                RowState::Pending => BindingState::Pending,
                RowState::Active => BindingState::Active,
                RowState::Ended | RowState::Suspended => BindingState::Revoked,
            },
            control_skill,
            kind: self.kind,
        })
    }
}

fn open(state: RowState) -> bool {
    matches!(state, RowState::Pending | RowState::Active)
}

fn row_of(conn: &Connection, digest: &str) -> Result<Option<BindingRow>> {
    conn.query_row(
        &format!("SELECT {COLUMNS} FROM shim_bindings WHERE digest = ?1"),
        [digest],
        row,
    )
    .optional()?
    .map(RawRow::decode)
    .transpose()
}

/// Appends `coordinator.ended` for a binding with a grant. A row whose
/// session is gone gives no event (logged).
fn ended_event(
    conn: &Connection,
    now: i64,
    out: &mut Vec<EventEnvelope>,
    row: &BindingRow,
    reason: &str,
) -> Result<()> {
    if row.is_fleet_only() {
        return Ok(());
    }
    let event = NewEvent::from(Event::CoordinatorEnded {
        binding_id: row.binding_id.clone(),
        generation: row.generation,
        reason: reason.to_owned(),
    });
    match log::append_one(conn, &row.session_id, event, now) {
        Ok(envelope) => out.push(envelope),
        Err(StoreError::UnknownSession(_)) => {
            tracing::warn!(binding = %row.binding_id, reason, "the session of an ended binding is gone");
        }
        Err(e) => return Err(e),
    }
    Ok(())
}

/// Ends the open row of `digest` and appends its event. `false` when the
/// row is missing or not open.
fn end_row(
    conn: &Connection,
    now: i64,
    out: &mut Vec<EventEnvelope>,
    digest: &str,
    reason: &str,
) -> Result<bool> {
    let Some(before) = row_of(conn, digest)?.filter(|r| open(r.state)) else {
        return Ok(false);
    };
    conn.execute(
        "UPDATE shim_bindings SET state = 'ended' WHERE digest = ?1",
        [digest],
    )?;
    ended_event(conn, now, out, &before, reason)?;
    Ok(true)
}

impl Store {
    /// Ends the row of an expired grant at the station start (§9.1) and
    /// appends `coordinator.ended { reason: "expired" }` for a binding with
    /// a grant. `false` when the row is missing or not open.
    ///
    /// # Errors
    /// A database error.
    pub async fn shim_binding_expire(&self, digest: String) -> Result<bool> {
        self.write(move |conn, now, out| end_row(conn, now, out, &digest, REASON_EXPIRED))
            .await
            .map(|(ended, _)| ended)
    }

    /// Ends the open rows of `digests` after a rotation, in one
    /// transaction, with `coordinator.ended { reason: "rotated" }` for each
    /// binding with a grant. Returns the number of rows ended.
    ///
    /// # Errors
    /// A database error; no row changes.
    pub async fn shim_bindings_rotate_end(&self, digests: Vec<String>) -> Result<usize> {
        self.write(move |conn, now, out| {
            let mut ended = 0;
            for digest in &digests {
                if end_row(conn, now, out, digest, REASON_ROTATED)? {
                    ended += 1;
                }
            }
            Ok(ended)
        })
        .await
        .map(|(n, _)| n)
    }

    /// The `pending` and `active` rows of one binding id, in creation
    /// order.
    ///
    /// # Errors
    /// A database error, or a row that does not decode.
    pub async fn shim_bindings_by_binding(&self, binding_id: String) -> Result<Vec<BindingRow>> {
        Ok(self
            .shim_bindings_load()
            .await?
            .into_iter()
            .filter(|r| r.binding_id == binding_id && open(r.state))
            .collect())
    }

    /// Deletes, in one transaction, the rows of every shim that is not in
    /// `keep`, and appends `coordinator.ended { reason: "process_exit" }`
    /// for each deleted `pending` or `active` row of a binding with a grant.
    /// Returns the deleted rows.
    ///
    /// # Errors
    /// A database error; no row changes.
    pub async fn shim_bindings_drop_others(&self, keep: Vec<String>) -> Result<Vec<BindingRow>> {
        self.write(move |conn, now, out| {
            let mut stmt = conn.prepare(&format!(
                "SELECT {COLUMNS} FROM shim_bindings ORDER BY created_at, digest"
            ))?;
            let raws = stmt
                .query_map([], row)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            let mut dropped = Vec::new();
            for raw in raws {
                let found = raw.decode()?;
                if keep.contains(&found.shim_id) {
                    continue;
                }
                conn.execute(
                    "DELETE FROM shim_bindings WHERE digest = ?1",
                    params![found.digest],
                )?;
                if open(found.state) {
                    ended_event(
                        conn,
                        now,
                        out,
                        &found,
                        super::shim_bindings::REASON_PROCESS_EXIT,
                    )?;
                }
                dropped.push(found);
            }
            Ok(dropped)
        })
        .await
        .map(|(rows, _)| rows)
    }
}

impl super::Decision<'_> {
    /// The row of `digest`, read in the decide transaction (the revoke
    /// reads the durable row, not memory: memory is already gated).
    ///
    /// # Errors
    /// A database error, or a row that does not decode.
    pub fn shim_binding_row(&self, digest: &str) -> Result<Option<BindingRow>> {
        row_of(self.connection(), digest)
    }
}

#[cfg(test)]
#[path = "shim_bindings_start_tests.rs"]
mod tests;
