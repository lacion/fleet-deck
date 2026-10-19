//! The `window_list` read (R13 P7b §5.2, §6, §6.1): a protected read of the
//! windows that an `any_window` grant may address. It follows `desktop_info`
//! ([`super::queries`]): the authorization, the query rate and the
//! `window_list` rate in one transaction, the two live policies, the protected
//! read ([`super::reads`]), and then the checks of the answer and the final
//! delivery pass.
//!
//! At most one list read is in flight for each target station on this deck: a
//! second read is refused at once with `busy` ([`ListSlots`], released by a
//! guard on every path). A list read uses no input, character or
//! desktop-mutation budget, sends no notification and does no board sign-out.
//! Its audit row names the query identity, the window count and the fixed
//! outcome only: never a handle, a rectangle or a monitor.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use fleetdeck_proto::coordinator_desktop::WindowListArguments;
use fleetdeck_proto::coordinator_desktop_text::{code, deadline_ms, limits};
use fleetdeck_proto::coordinator_desktop_wire::{PolicyPurpose, ProtectedRead};
use fleetdeck_proto::coordinator_input::InputWindowList;
use fleetdeck_proto::link::Outcome;
use fleetdeck_proto::mutation::CoordinatorBinding;
use fleetdeck_proto::{SessionId, StationId};
use serde_json::{Value, json};

use super::audit::{self, Facts};
use super::authorize::{Authorized, Tool, authorize, input_delivery_pass, store_error};
use super::policy::{caller_policy, target_policy};
use super::reads::coordinator_read;
use super::{reply_err, reply_ok};
use crate::link::DeckLink;
use crate::store::AuditResult;

/// The list reads in flight, for each target station of this deck (R13 P7b
/// §6.1). Nothing is stored: a restart starts with none.
#[derive(Debug, Default)]
pub(crate) struct ListSlots {
    stations: Mutex<HashMap<StationId, u32>>,
}

impl ListSlots {
    fn stations(&self) -> std::sync::MutexGuard<'_, HashMap<StationId, u32>> {
        self.stations.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The slot of one list read of `target`, or `None` when a read of that
    /// station is in flight.
    pub(crate) fn take(self: &Arc<Self>, target: &StationId) -> Option<ListSlot> {
        let mut stations = self.stations();
        let used = stations.get(target).copied().unwrap_or(0);
        if used >= limits::WINDOW_LIST_IN_FLIGHT_PER_STATION {
            return None;
        }
        stations.insert(target.clone(), used + 1);
        Some(ListSlot {
            slots: Arc::clone(self),
            target: target.clone(),
        })
    }

    /// The stations with a read in flight (tests).
    #[cfg(test)]
    fn in_flight(&self) -> usize {
        self.stations().len()
    }
}

/// The slot of one list read: released when it is dropped.
#[derive(Debug)]
pub(crate) struct ListSlot {
    slots: Arc<ListSlots>,
    target: StationId,
}

impl Drop for ListSlot {
    fn drop(&mut self) {
        let mut stations = self.slots.stations();
        if let Some(n) = stations.get_mut(&self.target) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                stations.remove(&self.target);
            }
        }
    }
}

/// Where a failed list read stopped: the audit row differs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stage {
    /// Before the station was asked for data, or at the final delivery pass
    /// (the list is not returned): a refusal.
    Refused,
    /// After the station was asked: a failed outcome.
    Failed,
}

/// The two policies, the protected read, the checks of the answer and the
/// final delivery pass.
async fn fetch(
    link: &DeckLink,
    auth: &Authorized,
    binding: &CoordinatorBinding,
    deadline: Instant,
) -> Result<InputWindowList, (&'static str, Stage)> {
    let ctx = auth.ctx();
    caller_policy(
        link,
        &ctx.caller_station,
        &ctx.caller_session,
        binding,
        deadline,
    )
    .await
    .map_err(|c| (c, Stage::Refused))?;
    // R13 P7b §6.1: the broader-scope purpose, with a null monitor.
    target_policy(link, auth.route(), PolicyPurpose::InputAny, None, deadline)
        .await
        .map_err(|c| (c, Stage::Refused))?;
    let left = deadline.saturating_duration_since(Instant::now());
    let ms = u64::try_from(
        left.min(Duration::from_millis(deadline_ms::INFO_DECK))
            .as_millis(),
    )
    .unwrap_or(0);
    let value = coordinator_read(link, auth, ProtectedRead::InputWindows, ms)
        .await
        .map_err(|c| (c, Stage::Failed))?;
    let list: InputWindowList =
        serde_json::from_value(value).map_err(|_| (code::DESKTOP_FAILED, Stage::Failed))?;
    if list.station != auth.target().as_str() {
        return Err((code::DESKTOP_FAILED, Stage::Failed));
    }
    list.validate().map_err(|e| (e.code(), Stage::Failed))?;
    // The final authority check before the list goes to the caller.
    input_delivery_pass(link, ctx, auth.target(), Tool::WindowList)
        .await
        .map_err(|c| (c, Stage::Refused))?;
    Ok(list)
}

/// The audit row of a list read: the query identity, the fixed stage and
/// outcome, and the window count of a success.
async fn audit_list(
    link: &DeckLink,
    auth: &Authorized,
    (stage, result, failure): (&'static str, AuditResult, Option<&str>),
    windows: Option<usize>,
) {
    let facts = Facts {
        action: "desktop.window_list",
        ctx: auth.ctx(),
        target: auth.target(),
        operation_id: None,
        window_id: None,
        monitor: None,
    };
    let extra: Vec<(&'static str, Value)> = windows
        .map(|n| vec![("windows", json!(n))])
        .unwrap_or_default();
    let row = facts.row_with(stage, result, failure, None, &extra);
    audit::write_or_log(&link.store, row).await;
}

/// `window_list` (R13 P7b §5.2) after the envelope checks.
pub(super) async fn list(
    link: &DeckLink,
    station: &StationId,
    (caller, binding): (SessionId, CoordinatorBinding),
    args: WindowListArguments,
    started: Instant,
) -> Outcome {
    let deadline = started + Duration::from_millis(deadline_ms::INFO_DECK);
    let auth = match authorize(
        link,
        station,
        &caller,
        &binding,
        Tool::WindowList,
        args.station.as_deref(),
    )
    .await
    {
        Ok(auth) => auth,
        Err(refusal) => return reply_err(refusal, None),
    };
    // The query rate and the list rate, in one transaction.
    let admitted = link
        .store
        .desktop_window_list_admit(auth.ctx(), link.store.now_ms())
        .await
        .map_err(|e| store_error(&e));
    let refusal = match admitted {
        Ok(Ok(())) => None,
        Ok(Err(refusal)) | Err(refusal) => Some(refusal),
    };
    if let Some(refusal) = refusal {
        let row = (audit::REFUSED, AuditResult::Refused, Some(refusal));
        audit_list(link, &auth, row, None).await;
        return reply_err(refusal, None);
    }
    // One read in flight for each target station on this deck.
    let Some(_slot) = link.coordinator_desktop.lists().take(auth.target()) else {
        let row = (audit::REFUSED, AuditResult::Refused, Some(code::BUSY));
        audit_list(link, &auth, row, None).await;
        return reply_err(code::BUSY, None);
    };
    match fetch(link, &auth, &binding, deadline).await {
        Ok(list) => match serde_json::to_value(&list) {
            Ok(value) => {
                let row = (audit::OUTCOME, AuditResult::Ok, None);
                audit_list(link, &auth, row, Some(list.windows.len())).await;
                reply_ok(value)
            }
            Err(_) => {
                let row = (
                    audit::OUTCOME,
                    AuditResult::Failed,
                    Some(code::DESKTOP_FAILED),
                );
                audit_list(link, &auth, row, None).await;
                reply_err(code::DESKTOP_FAILED, None)
            }
        },
        Err((failure, stage)) => {
            let (name, result) = match stage {
                Stage::Refused => (audit::REFUSED, AuditResult::Refused),
                Stage::Failed => (audit::OUTCOME, AuditResult::Failed),
            };
            audit_list(link, &auth, (name, result, Some(failure)), None).await;
            reply_err(failure, None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_read_is_in_flight_for_each_station_and_a_drop_releases_it() {
        let slots = Arc::new(ListSlots::default());
        let (a, b) = (StationId::from("alpha"), StationId::from("beta"));
        let one = slots.take(&a).expect("first");
        assert!(slots.take(&a).is_none(), "a second read of the station");
        let other = slots.take(&b).expect("another station");
        assert_eq!(slots.in_flight(), 2);
        drop(one);
        let again = slots.take(&a).expect("released");
        drop((other, again));
        assert_eq!(slots.in_flight(), 0, "no leftover entry");
    }
}
