//! `session.coordinator_set` (MCP P4b §3, §4.1): the owner's desired grant of
//! a session. The station keeps one record for each session (V42) and orders
//! every give, change and end of it by one per-session sequence. In the same
//! transaction the decision brings the running binding of the session in line
//! with the record, keeping its credential: on the shim host the row is
//! rewritten; memory is published after the commit ([`publish`]).

use std::collections::BTreeSet;

use fleetdeck_proto::command::{
    CommandOrigin, CommandResult, CoordinatorSpec, GrantKind, GrantScope, SessionSpec,
};
use fleetdeck_proto::event::RunOrigin;
use fleetdeck_proto::version::feature;
use fleetdeck_proto::{Event, RejectCode, SessionId};

use super::{check_structure, owner};
use crate::coordinator::fence::FenceState;
use crate::coordinator::{Binding, BindingState, Bindings, ToolClass};
use crate::manager::coordinator_start::{DeckFeatures, grant_classes};
use crate::manager::coordinator_tools::{FLEET_GRANT, FLEET_SKILL};
use crate::manager::decide::reject;
use crate::store::session_grants::{GrantRecord, RecordState};
use crate::store::shim_bindings::RowState;
use crate::store::{DecideError, Decision, Store};

/// The classes that only a start gives (MCP P4b §3).
const FIXED: [&str; 3] = ["desktop", "capture", "input"];

/// The reason of `coordinator.ended` for an end by the owner.
const REVOKED: &str = "revoked";
/// The reason of `coordinator.ended` for a grant that another grant replaced.
const REPLACED: &str = "replaced";

fn fixed_of(classes: &[String]) -> BTreeSet<&str> {
    classes
        .iter()
        .map(String::as_str)
        .filter(|c| FIXED.contains(c))
        .collect()
}

fn answer(session_id: &SessionId, seq: u64) -> CommandResult {
    CommandResult {
        session_id: Some(session_id.clone()),
        input_id: None,
        seq: Some(seq),
    }
}

/// The checks of a `Some(spec)`: the owner grant kind, the grant of the
/// command, the structure of a start, the classes, and the fixed classes
/// (`desktop`, `capture`, `input`), which only a start gives.
fn check_spec(
    spec: &CoordinatorSpec,
    grant_id: &str,
    old: Option<&GrantRecord>,
    features: DeckFeatures,
) -> Result<(), DecideError> {
    if spec.kind != GrantKind::Coordinator {
        return Err(reject(
            RejectCode::InvalidCommand,
            "a live grant is an owner grant",
        ));
    }
    if spec.grant_id != grant_id {
        return Err(reject(
            RejectCode::InvalidCommand,
            "the spec names another grant",
        ));
    }
    check_structure(RunOrigin::Board, spec, false)?;
    grant_classes(spec, features).map_err(|error| reject(RejectCode::InvalidCommand, error))?;
    let held = old
        .filter(|r| r.state == RecordState::Attached && r.grant_id == grant_id)
        .and_then(|r| r.spec.as_ref())
        .map(|s| fixed_of(&s.classes))
        .unwrap_or_default();
    if fixed_of(&spec.classes) != held {
        return Err(reject(
            RejectCode::InvalidCommand,
            "desktop, screenshot and input access is given only at start",
        ));
    }
    Ok(())
}

/// The new record of a command that has a higher sequence than `old`.
fn next_record(
    d: &Decision<'_>,
    old: Option<&GrantRecord>,
    session_id: &SessionId,
    seq: u64,
    grant_id: &str,
    spec: Option<&CoordinatorSpec>,
) -> GrantRecord {
    let attached_same = old.filter(|r| r.state == RecordState::Attached && r.grant_id == grant_id);
    match (spec, old) {
        (Some(spec), _) => GrantRecord {
            session_id: session_id.clone(),
            grant_id: grant_id.to_owned(),
            seq,
            spec: Some(spec.clone()),
            generation: attached_same.map_or(0, |r| r.generation),
            state: RecordState::Attached,
            ended_reason: None,
            updated_at: d.now(),
        },
        (None, Some(old)) => {
            let mut record = old.clone();
            record.seq = seq;
            record.updated_at = d.now();
            if attached_same.is_some() {
                record.state = RecordState::Ended;
                record.ended_reason = Some(REVOKED.to_owned());
            }
            record
        }
        // An end that was never recorded still fences the later messages.
        (None, None) => GrantRecord {
            session_id: session_id.clone(),
            grant_id: grant_id.to_owned(),
            seq,
            spec: None,
            generation: 0,
            state: RecordState::Ended,
            ended_reason: Some(REVOKED.to_owned()),
            updated_at: d.now(),
        },
    }
}

/// The binding that the record asks for, with the credential, the native
/// identity and the state of `live` (MCP P4b §4.1). The result equals `live`
/// when the record asks for no change: a record that is `Ended` for another
/// grant than the live one, or an `Attached` record without a spec.
pub(super) fn target_form(record: &GrantRecord, live: &Binding, features: DeckFeatures) -> Binding {
    let mut form = live.clone();
    match (record.state, &record.spec) {
        (RecordState::Attached, Some(spec)) => {
            form.binding_id = format!("{}:{}", live.session_id, record.grant_id);
            form.grant_id = record.grant_id.clone();
            form.classes = grant_classes(spec, features).unwrap_or_else(|error| {
                tracing::error!(session = %live.session_id, error, "the record's classes do not resolve; only the session tools stay");
                std::iter::once(ToolClass::Own).collect()
            });
            form.scope = spec.scope;
            form.control_skill = spec.control_skill.text.clone();
            form.kind = GrantKind::Coordinator;
            form.generation = if live.grant_id == record.grant_id {
                live.generation
            } else {
                1
            };
        }
        (RecordState::Ended, _) if live.grant_id == record.grant_id => {
            form.binding_id = format!("{}:{FLEET_GRANT}", live.session_id);
            form.grant_id = String::new();
            form.classes = std::iter::once(ToolClass::Own).collect();
            form.scope = GrantScope::Lineage;
            form.control_skill = FLEET_SKILL.to_owned();
            form.kind = GrantKind::Coordinator;
        }
        _ => {}
    }
    form
}

/// True when the two bindings agree in every field that [`target_form`] sets.
fn in_sync(form: &Binding, live: &Binding) -> bool {
    form.binding_id == live.binding_id
        && form.grant_id == live.grant_id
        && form.classes == live.classes
        && form.scope == live.scope
        && form.control_skill == live.control_skill
        && form.kind == live.kind
        && form.generation == live.generation
}

/// Copies the fields that [`target_form`] sets; the state and the native
/// identity of `b` stay as they are now.
fn apply(b: &mut Binding, form: &Binding) {
    b.binding_id.clone_from(&form.binding_id);
    b.grant_id.clone_from(&form.grant_id);
    b.classes.clone_from(&form.classes);
    b.scope = form.scope;
    b.control_skill.clone_from(&form.control_skill);
    b.kind = form.kind;
    b.generation = form.generation;
}

/// True when the credential is held (a replay is not finished) or the fence
/// names it. An unreadable fence fails closed; a missing one does not block.
fn blocked(bindings: &Bindings, digest: &str) -> bool {
    if bindings.is_held(digest) {
        return true;
    }
    match bindings.fence().map(|fence| fence.read()) {
        Some(FenceState::Unreadable) => true,
        Some(FenceState::Entries(entries)) => entries.iter().any(|(_, d)| d == digest),
        Some(FenceState::Missing) | None => false,
    }
}

fn open(state: RowState) -> bool {
    matches!(state, RowState::Pending | RowState::Active)
}

/// The events of an in-place rewrite from `live` to `form` (MCP P4b §4.1).
fn live_events(record: &GrantRecord, live: &Binding, form: &Binding) -> Vec<Event> {
    let mut out = Vec::new();
    match record.state {
        RecordState::Attached if live.grant_id != record.grant_id => {
            if live.kind == GrantKind::Worker && !live.is_fleet_only() {
                out.push(Event::CoordinatorEnded {
                    binding_id: live.binding_id.clone(),
                    generation: live.generation,
                    reason: REPLACED.to_owned(),
                });
            }
            if live.state == BindingState::Active
                && let Some(native_id) = &live.native_id
            {
                out.push(Event::CoordinatorBound {
                    binding_id: form.binding_id.clone(),
                    grant_id: form.grant_id.clone(),
                    generation: form.generation,
                    provider_instance: live.provider_instance.clone(),
                    native_id: native_id.clone(),
                });
            }
        }
        RecordState::Attached => {}
        RecordState::Ended => out.push(Event::CoordinatorEnded {
            binding_id: format!("{}:{}", live.session_id, record.grant_id),
            generation: live.generation,
            reason: REVOKED.to_owned(),
        }),
    }
    out
}

/// The sequence of the record that a start writes (MCP P4b §9).
const START_SEQ: u64 = 1;

/// True when the deck link negotiated `coordinator_live.v1`.
fn live_deck(bindings: &Bindings) -> bool {
    bindings
        .deck()
        .is_some_and(|deck| deck.has_feature(feature::COORDINATOR_LIVE))
}

/// The start-time record (MCP P4b §9), written in the decision of a start
/// right after the session exists: an owner grant (`kind` coordinator, never
/// a worker) on a deck link that negotiated `coordinator_live.v1` gets an
/// `attached` record at generation 0 and `seq` 1. Returns that `seq`, for the
/// receipt of the start; `None` when the start has no record.
///
/// # Errors
/// A database error.
pub(in crate::manager) fn record_start(
    d: &mut Decision<'_>,
    bindings: &Bindings,
    session_id: &SessionId,
    spec: &SessionSpec,
) -> Result<Option<u64>, DecideError> {
    let Some(coordinator) = spec
        .coordinator
        .as_deref()
        .filter(|c| c.kind == GrantKind::Coordinator)
    else {
        return Ok(None);
    };
    if !live_deck(bindings) {
        return Ok(None);
    }
    d.session_grant_put(&GrantRecord {
        session_id: session_id.clone(),
        grant_id: coordinator.grant_id.clone(),
        seq: START_SEQ,
        spec: Some(coordinator.clone()),
        generation: 0,
        state: RecordState::Attached,
        ended_reason: None,
        updated_at: d.now(),
    })?;
    Ok(Some(START_SEQ))
}

/// The decision of `session.coordinator_set`: the record, then the running
/// binding (shim row in this transaction, memory after the commit).
///
/// # Errors
/// A stored rejection, or a database error.
pub(in crate::manager) fn set(
    d: &mut Decision<'_>,
    bindings: &Bindings,
    origin: CommandOrigin,
    session_id: &SessionId,
    seq: u64,
    grant_id: &str,
    spec: Option<&CoordinatorSpec>,
) -> Result<CommandResult, DecideError> {
    owner(origin)?;
    if !live_deck(bindings) {
        return Err(reject(
            RejectCode::InvalidState,
            "the deck link has no coordinator_live.v1",
        ));
    }
    d.summary(session_id)?
        .ok_or_else(|| reject(RejectCode::UnknownSession, "no such session"))?;
    if i64::try_from(seq).is_err() {
        return Err(reject(RejectCode::InvalidCommand, "seq is out of range"));
    }
    let features = DeckFeatures::of(bindings);
    let old = d.session_grant(session_id)?;
    match spec {
        Some(spec) => check_spec(spec, grant_id, old.as_ref(), features)?,
        None if grant_id.is_empty() => {
            return Err(reject(RejectCode::InvalidCommand, "grant_id is empty"));
        }
        None => {}
    }
    let old_seq = old.as_ref().map_or(0, |r| r.seq);
    if seq <= old_seq {
        return Ok(answer(session_id, old_seq));
    }
    let mut record = next_record(d, old.as_ref(), session_id, seq, grant_id, spec);
    if spec.is_some()
        && let Some(old) = old
            .as_ref()
            .filter(|r| r.state == RecordState::Attached && r.grant_id != grant_id)
    {
        d.append(
            session_id,
            Event::CoordinatorEnded {
                binding_id: format!("{session_id}:{}", old.grant_id),
                generation: old.generation,
                reason: REPLACED.to_owned(),
            }
            .into(),
        )?;
    }
    rewrite_live(d, bindings, &mut record, features)?;
    d.session_grant_put(&record)?;
    Ok(answer(session_id, seq))
}

/// Step 3.e: the running binding of the session, rewritten in the shim row
/// when the target form differs from it. `record.generation` becomes 1 when
/// the record's grant attaches to this process for the first time.
fn rewrite_live(
    d: &mut Decision<'_>,
    bindings: &Bindings,
    record: &mut GrantRecord,
    features: DeckFeatures,
) -> Result<(), DecideError> {
    let Some((digest, live)) = bindings.live_of_session(&record.session_id) else {
        return Ok(());
    };
    if blocked(bindings, &digest) {
        return Ok(());
    }
    let row = d.shim_binding_row(&digest)?;
    if row.as_ref().is_some_and(|r| !open(r.state)) {
        return Ok(());
    }
    let form = target_form(record, &live, features);
    if in_sync(&form, &live) {
        return Ok(());
    }
    if bindings.id_live_elsewhere(&digest, &form.binding_id) {
        tracing::warn!(session = %record.session_id, binding = %form.binding_id, "another live binding has the target id; the running binding is not rewritten");
        return Ok(());
    }
    if record.state == RecordState::Attached && live.grant_id != record.grant_id {
        record.generation = form.generation;
    }
    for event in live_events(record, &live, &form) {
        d.append(&record.session_id, event.into())?;
    }
    if row.is_some() {
        let expires_at = if form.is_fleet_only() {
            None
        } else {
            record.spec.as_ref().map(|s| s.expires_at)
        };
        d.shim_binding_rewrite(&digest, &form, expires_at)?;
    }
    Ok(())
}

/// Publishes the running binding after the commit of a `coordinator_set`
/// (MCP P4b §4.1), under the skip rules of the decision. A failure is
/// logged: the row (shim host) is already right, and the next reload
/// restores memory from it.
pub(super) async fn publish(store: &Store, bindings: &Bindings, session_id: &SessionId) {
    let record = match store.session_grant(session_id.clone()).await {
        Ok(Some(record)) => record,
        Ok(None) => return,
        Err(error) => {
            tracing::error!(%error, session = %session_id, "the grant record was not read after a set");
            return;
        }
    };
    let Some((digest, live)) = bindings.live_of_session(session_id) else {
        return;
    };
    if blocked(bindings, &digest) {
        return;
    }
    let row = match store.shim_bindings_load().await {
        Ok(rows) => rows.into_iter().find(|r| r.digest == digest),
        Err(error) => {
            tracing::error!(%error, session = %session_id, "the binding rows were not read after a set");
            return;
        }
    };
    if row.as_ref().is_some_and(|r| !open(r.state)) {
        return;
    }
    let form = target_form(&record, &live, DeckFeatures::of(bindings));
    if in_sync(&form, &live) {
        return;
    }
    // Memory never gets ahead of the durable row: the decision rewrote the
    // row, or it skipped, and then memory stays as it is.
    if let Some(row) = &row
        && !row.to_binding().is_ok_and(|stored| in_sync(&form, &stored))
    {
        tracing::warn!(session = %session_id, binding = %form.binding_id, "the binding row was not rewritten by the set; memory is not rewritten");
        return;
    }
    if !bindings.claim_binding_id(&digest, &form.binding_id) {
        tracing::warn!(session = %session_id, binding = %form.binding_id, "another live binding has the target id; the running binding is not rewritten");
        return;
    }
    bindings.rewrite(&digest, |b| apply(b, &form));
}

#[path = "coordinator_record.rs"]
pub(in crate::manager) mod record;

#[cfg(test)]
#[path = "coordinator_set_tests.rs"]
mod tests;
