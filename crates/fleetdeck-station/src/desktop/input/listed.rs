//! The registry of listed-window handles (R13 P7b §7): the opaque handles that
//! `window_list` issues for ordinary windows. It is in memory only.
//!
//! A handle has 128 random bits (32 lowercase hexadecimal characters). It is
//! bound to the deck fingerprint, the grant, the caller station and session,
//! the binding and its generation, the target identity (the instance
//! signature of the desktop), the event epoch, the window incarnation, the
//! native address and the class seen at the listing, and a monotonic expiry
//! of [`LISTED_HANDLE_TTL_MS`] after issuance (a new listing never extends
//! it). A handle confers input eligibility only: it never resolves in the
//! preview registry, and `window_close` never accepts it (§7.1).
//!
//! A handle is invalid after its expiry, a close of its window (the hook of
//! [`crate::desktop::events::Events::on_close`]), a change of the incarnation
//! of its address, a new epoch, another target identity or another binding
//! generation. An unknown, expired, foreign or invalid handle resolves to
//! `None`: the same answer for each. When the registry is full, an issue
//! fails with `busy` and nothing live is evicted.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use fleetdeck_proto::coordinator_desktop_text::{code as p6_code, limits};
use fleetdeck_proto::coordinator_desktop_wire::CoordinatorContext;

use crate::desktop::Fail;
use crate::desktop::coordinator::{new_window_id, p6_fail};
use crate::desktop::events::Events;

/// The longest life of a handle.
const TTL: Duration = Duration::from_millis(limits::LISTED_HANDLE_TTL_MS);

/// Who a listed handle belongs to (R13 P7b §7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Binding {
    pub deck_fp: String,
    pub grant_id: String,
    pub caller_station: String,
    pub caller_session: String,
    pub binding_id: String,
    pub generation: u32,
}

impl Binding {
    /// The binding of the coordinator `ctx` on the link of `deck_fp`.
    pub(crate) fn of(deck_fp: &str, ctx: &CoordinatorContext) -> Self {
        Self {
            deck_fp: deck_fp.to_owned(),
            grant_id: ctx.grant_id.clone(),
            caller_station: ctx.caller_station.to_string(),
            caller_session: ctx.caller_session.to_string(),
            binding_id: ctx.binding_id.clone(),
            generation: ctx.generation,
        }
    }
}

/// The window that a handle names, as resolved now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Resolved {
    pub address: String,
    pub class: String,
    pub incarnation: u64,
    pub epoch: u64,
}

/// What a resolution compares the handle with: the target identity, the
/// event stream and the monotonic now.
pub(crate) struct Seen<'a> {
    pub signature: &'a str,
    pub events: &'a Events,
    pub now: Instant,
}

/// One window of a listing that needs a handle.
#[derive(Debug, Clone)]
pub(crate) struct Item {
    pub address: String,
    pub class: String,
    pub incarnation: u64,
}

/// The handles of one listing, in the order of its items.
#[derive(Debug, Default)]
pub(crate) struct Issued {
    /// The handle and its remaining life in milliseconds.
    pub handles: Vec<(String, u64)>,
    /// The handles that this listing created (a reused handle is not here).
    pub fresh: Vec<String>,
}

#[derive(Debug, Clone)]
struct Entry {
    binding: Binding,
    signature: String,
    epoch: u64,
    incarnation: u64,
    address: String,
    class: String,
    expires: Instant,
}

/// The registry. Cloning it shares the entries.
#[derive(Debug, Clone, Default)]
pub struct Listed {
    entries: Arc<Mutex<HashMap<String, Entry>>>,
}

impl Listed {
    fn lock(&self) -> MutexGuard<'_, HashMap<String, Entry>> {
        self.entries.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The handles for `items` of one binding, all or none: an unexpired
    /// handle of the same binding, target, epoch, incarnation, address and
    /// class is reused with its remaining life; every other item gets a new
    /// handle that lives [`LISTED_HANDLE_TTL_MS`]. Entries that can never
    /// resolve again (expired, another epoch or another target) are removed
    /// first; no live entry is ever evicted.
    ///
    /// # Errors
    /// `busy` when the new handles would pass the limit of the binding and
    /// target or of the station; `failed` when the random source fails. The
    /// registry is unchanged.
    pub(crate) fn issue(
        &self,
        binding: &Binding,
        signature: &str,
        epoch: u64,
        items: &[Item],
        now: Instant,
    ) -> Result<Issued, Fail> {
        let mut entries = self.lock();
        entries.retain(|_, e| e.expires > now && e.epoch == epoch && e.signature == signature);
        let mut issued = Issued::default();
        let mut pending: Vec<(String, Entry)> = Vec::new();
        for item in items {
            let reuse = entries.iter().find(|(_, e)| {
                e.binding == *binding
                    && e.incarnation == item.incarnation
                    && e.address == item.address
                    && e.class == item.class
            });
            if let Some((id, e)) = reuse {
                let left = e.expires.saturating_duration_since(now);
                issued.handles.push((id.clone(), millis(left)));
                continue;
            }
            let mut id = new_window_id()?;
            while entries.contains_key(&id) || pending.iter().any(|(p, _)| *p == id) {
                id = new_window_id()?;
            }
            pending.push((
                id.clone(),
                Entry {
                    binding: binding.clone(),
                    signature: signature.to_owned(),
                    epoch,
                    incarnation: item.incarnation,
                    address: item.address.clone(),
                    class: item.class.clone(),
                    expires: now + TTL,
                },
            ));
            issued.handles.push((id.clone(), millis(TTL)));
            issued.fresh.push(id);
        }
        let own = entries.values().filter(|e| e.binding == *binding).count();
        let per_binding = limits::LISTED_HANDLES_PER_BINDING as usize;
        let per_station = limits::LISTED_HANDLES_PER_STATION as usize;
        if own + pending.len() > per_binding || entries.len() + pending.len() > per_station {
            return Err(p6_fail(p6_code::BUSY));
        }
        entries.extend(pending);
        Ok(issued)
    }

    /// Removes the handles `ids` (a listing that failed after its issue).
    pub(crate) fn revoke(&self, ids: &[String]) {
        let mut entries = self.lock();
        for id in ids {
            entries.remove(id);
        }
    }

    /// Removes the handles of the window at `address` (the close hook).
    pub fn forget_address(&self, address: &str) {
        self.lock().retain(|_, e| e.address != address);
    }

    /// The window of `window_id` for `binding`, or `None`: an unknown, expired,
    /// foreign or invalid handle gives the same answer.
    pub(crate) fn resolve(
        &self,
        window_id: &str,
        binding: &Binding,
        seen: &Seen<'_>,
    ) -> Option<Resolved> {
        let epoch = seen.events.epoch();
        let entries = self.lock();
        let e = entries.get(window_id)?;
        let valid = e.binding == *binding
            && e.signature == seen.signature
            && e.epoch == epoch
            && e.expires > seen.now
            && seen.events.incarnation(&e.address) == Some(e.incarnation);
        valid.then(|| Resolved {
            address: e.address.clone(),
            class: e.class.clone(),
            incarnation: e.incarnation,
            epoch: e.epoch,
        })
    }

    /// How many handles the registry holds (tests).
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.lock().len()
    }
}

fn millis(d: Duration) -> u64 {
    u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
#[path = "listed_tests.rs"]
mod tests;
