//! The target of an input operation (R13 P7 §7.3 step 4; R13 P7b §7, §10.1).
//!
//! A legacy action resolves its `window_id` in the preview registry of the
//! caller. An `InputAny*` action resolves it first in the registry of listed
//! handles for the binding of the caller, then in the caller's own previews;
//! neither gives `desktop_window_not_found`. A listed window has no preview
//! provenance: its record has the kind `Listed`, the barrier reads the wall
//! records and the names only, and the absence of board evidence is not
//! proof (a deck-origin mark on a preview record at its address is evidence).
//! While the operation runs, the handle must stay valid, and the
//! window incarnation and the event epoch must not change.

use super::Env;
use super::exec::{Op, not_found};
use super::listed::{Binding, Resolved, Seen};
use crate::desktop::authority::Start;
use crate::desktop::coordinator::refusal;
use crate::desktop::events::{Kind, PreviewOwner, Record};
use crate::desktop::{Ctx, Desktop, Fail};

/// A resolved listed handle.
pub(super) struct ListedTarget {
    pub(super) binding: Binding,
    pub(super) resolved: Resolved,
}

/// What step 4 found.
pub(super) struct Found {
    pub(super) record: Record,
    pub(super) listed: Option<ListedTarget>,
}

/// The binding of the caller of `start`.
///
/// # Errors
/// `forbidden` (the refusal of an authority) without a coordinator context.
pub(crate) fn binding_of(start: &Start) -> Result<Binding, Fail> {
    match (start.coordinator(), start.link_extras()) {
        (Some(ctx), Some(extras)) => Ok(Binding::of(&extras.deck_fp, ctx)),
        _ => Err(refusal()),
    }
}

/// Step 4: the record of the target.
pub(super) fn resolve(
    desktop: &Desktop,
    ctx: &Ctx,
    start: &Start,
    window_id: &str,
    owner: &PreviewOwner,
    any: bool,
    env: &Env,
) -> Result<Found, Fail> {
    if any {
        let binding = binding_of(start)?;
        let seen = Seen {
            signature: &ctx.found.signature,
            events: &desktop.events,
            now: (env.clock)(),
        };
        if let Some(resolved) = desktop.listed().resolve(window_id, &binding, &seen) {
            let record = Record {
                signature: ctx.found.signature.clone(),
                address: resolved.address.clone(),
                class: resolved.class.clone(),
                kind: Kind::Listed,
                display_id: None,
                monitor: None,
                opener: None,
                launch_id: String::new(),
            };
            return Ok(Found {
                record,
                listed: Some(ListedTarget { binding, resolved }),
            });
        }
    }
    let (found_owner, record) = desktop.events.preview(window_id).ok_or_else(not_found)?;
    if found_owner != *owner || record.signature != ctx.found.signature {
        return Err(not_found());
    }
    Ok(Found {
        record,
        listed: None,
    })
}

impl Op<'_> {
    /// The target is still the one that step 4 found (never a similar
    /// window): the same owner and record for a preview; for a listed
    /// window the handle still resolves to the same window incarnation in the
    /// same epoch, before its expiry.
    pub(super) fn same_record(&self) -> Result<(), Fail> {
        match &self.listed {
            None => match self.desktop.events.preview(&self.window_id) {
                Some((o, r)) if o == self.owner && r == self.record => Ok(()),
                _ => Err(not_found()),
            },
            Some(target) => {
                let seen = Seen {
                    signature: &self.ctx.found.signature,
                    events: &self.desktop.events,
                    now: (self.env.clock)(),
                };
                match self
                    .desktop
                    .listed()
                    .resolve(&self.window_id, &target.binding, &seen)
                {
                    Some(now) if now == target.resolved => Ok(()),
                    _ => Err(not_found()),
                }
            }
        }
    }

    /// The launch URL of the preview named a known deck origin. A listed
    /// window has no preview provenance of its own; it is marked when any
    /// preview record at its address carries a deck-origin mark (R13 P7b §8),
    /// read at each barrier check.
    pub(super) fn marked(&self) -> bool {
        if self.listed.is_none() {
            return self.desktop.board_marks.is_marked(&self.window_id);
        }
        self.desktop
            .events
            .preview_ids_at(&self.record.address)
            .iter()
            .any(|id| self.desktop.is_board_origin(id))
    }

    /// The record that the barrier reads: a listed window is a wall window
    /// when the station has a wall record at its address (R13 P7b §8).
    pub(super) fn barrier_record(&self) -> Record {
        let mut record = self.record.clone();
        let wall = self
            .desktop
            .events
            .record(&record.address)
            .is_some_and(|r| r.kind == Kind::Wall);
        if self.listed.is_some() && wall {
            record.kind = Kind::Wall;
        }
        record
    }
}
