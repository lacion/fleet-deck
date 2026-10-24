//! The live delivery of a coordinator wall directive (R13 P6 §4.2, the T5
//! brief items 7 to 9).
//!
//! A synchronous reader ([`crate::store::DeckStore::display_directive`] and
//! `display_scope`) shows `mission` for a coordinator row, so it fails closed.
//! The delivery points of a display call [`effective_directive`] and
//! [`effective_scope`] instead. For a coordinator row they run, on every
//! delivery:
//!
//! 1. the deck switches (`fleet_control`, `desktop_control`) and the local
//!    pass (display, binding, ceiling, grant, generation, reference
//!    rules, tombstones) in one read transaction;
//! 2. the caller policy (the full current binding of the grant) and the
//!    target policy (purpose `desktop`, the station that the display is bound
//!    to), each bounded by 3 s;
//! 3. the switches and the local pass again, for the same generation.
//!
//! Only a full pass delivers the coordinator directive. A failed local pass
//! and a policy `false` invalidate the stored directive (a new generation) and
//! show `mission`. An uncertain policy (a timeout, a lost link, a malformed
//! answer, no route to the target) and a deck switch that is off show
//! `mission` for this response only and keep the row. A coordinator row never becomes an owner payload.
//!
//! No database transaction is held across a network await.

use std::time::{Duration, Instant};

use fleetdeck_proto::coordinator_desktop_text::{code, deadline_ms};
use fleetdeck_proto::coordinator_desktop_wire::PolicyPurpose;
use fleetdeck_proto::display_directive::DirectiveView;

use super::authorize::switches;
use super::policy::{caller_policy, target_policy};
use crate::link::DeckLink;
use crate::link::desktop_authority::resolve;
use crate::store::Result;
use crate::store::display_directives::EffectiveScope;
use crate::store::display_directives_coordinator::CoordinatorRow;
use crate::store::grants::Grant;

/// What the checks of one delivery found.
enum Delivery {
    /// The display holds no coordinator directive, or the directive does not
    /// pass: the synchronous store read is the answer (`mission` for a
    /// coordinator row).
    Plain,
    /// Every check passed: the coordinator directive is delivered.
    Pass(Box<CoordinatorRow>),
}

/// The answer of the two live policies.
enum Policy {
    Yes,
    /// A policy said `false`.
    No,
    /// A policy could not be decided.
    Unknown,
}

/// The caller policy and the target policy, one after the other, each with its
/// own 3 s bound.
async fn policies(link: &DeckLink, row: &CoordinatorRow, grant: &Grant) -> Policy {
    let (Some(binding), Some(target)) = (grant.binding.as_ref(), row.bound_station.as_ref()) else {
        return Policy::No;
    };
    let Ok(route) = resolve(link, &link.store, target) else {
        return Policy::Unknown;
    };
    let bound = Duration::from_millis(deadline_ms::CALLER_POLICY);
    let caller = caller_policy(
        link,
        &row.sender.caller_station,
        &row.sender.caller_session,
        binding,
        Instant::now() + bound,
    )
    .await;
    match caller {
        Ok(()) => {}
        Err(code::NOT_GRANTED) => return Policy::No,
        Err(_) => return Policy::Unknown,
    }
    let bound = Duration::from_millis(deadline_ms::TARGET_POLICY);
    let target = target_policy(
        link,
        &route,
        PolicyPurpose::Desktop,
        None,
        Instant::now() + bound,
    )
    .await;
    match target {
        Ok(()) => Policy::Yes,
        Err(code::NOT_GRANTED) => Policy::No,
        Err(_) => Policy::Unknown,
    }
}

/// The local pass of the store, with the clock of the deck read right before
/// it. Returns the grant as it is now.
async fn local_pass(link: &DeckLink, display_id: &str, generation: u64) -> Result<Option<Grant>> {
    let now = link.store.now_ms();
    link.store
        .display_coordinator_valid(display_id.to_owned(), generation, now)
        .await
}

/// The local pass, then the policies, then the local pass again. A pass that
/// fails invalidates the row it was made for.
async fn delivery(link: &DeckLink, display_id: &str) -> Result<Delivery> {
    let store = &link.store;
    let Some(row) = store.display_coordinator_row(display_id.to_owned()).await? else {
        return Ok(Delivery::Plain);
    };
    // A deck switch that is off gives `mission` for this response only and
    // keeps the row: the switch can come back on.
    if switches(link).is_err() {
        return Ok(Delivery::Plain);
    }
    let Some(grant) = local_pass(link, display_id, row.generation).await? else {
        store
            .display_coordinator_invalidate(display_id.to_owned(), row.generation)
            .await?;
        return Ok(Delivery::Plain);
    };
    match policies(link, &row, &grant).await {
        Policy::Yes => {}
        Policy::No => {
            store
                .display_coordinator_invalidate(display_id.to_owned(), row.generation)
                .await?;
            return Ok(Delivery::Plain);
        }
        Policy::Unknown => return Ok(Delivery::Plain),
    }
    if switches(link).is_err() {
        return Ok(Delivery::Plain);
    }
    if local_pass(link, display_id, row.generation)
        .await?
        .is_none()
    {
        // A changed generation is a newer directive: the guard keeps it.
        store
            .display_coordinator_invalidate(display_id.to_owned(), row.generation)
            .await?;
        return Ok(Delivery::Plain);
    }
    Ok(Delivery::Pass(Box::new(row)))
}

/// The directive that a display reads now and its generation: the coordinator
/// directive only after every live check of this delivery, else what the
/// synchronous read gives (`mission` for a coordinator row). `None` when the
/// display does not exist or is revoked.
///
/// # Errors
/// A database error.
pub(crate) async fn effective_directive(
    link: &DeckLink,
    display_id: &str,
) -> Result<Option<DirectiveView>> {
    match delivery(link, display_id).await? {
        Delivery::Pass(row) => Ok(Some(DirectiveView {
            generation: row.generation,
            directive: row.directive,
        })),
        Delivery::Plain => link.store.display_directive(display_id.to_owned()).await,
    }
}

/// The effective scope of a display now: for a delivered coordinator
/// directive the ceiling narrowed by its filter, else what the synchronous
/// read gives. `None` when the display does not exist or is revoked.
///
/// # Errors
/// A database error.
pub(crate) async fn effective_scope(
    link: &DeckLink,
    display_id: &str,
) -> Result<Option<EffectiveScope>> {
    match delivery(link, display_id).await? {
        Delivery::Pass(row) => Ok(Some(EffectiveScope::narrowed(row.ceiling, &row.directive))),
        Delivery::Plain => link.store.display_scope(display_id.to_owned()).await,
    }
}
