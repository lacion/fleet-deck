//! The `window_list` protected read (R13 P7b §6, §6.1, §7): the closed
//! projection of the ordinary windows that an `any_window` grant may give
//! input to, and the opaque handles that name them.
//!
//! The order:
//!
//! 1. the live gates of an `InputAny*` action (no click gate) and the capture
//!    gate; a read without a coordinator context is refused;
//! 2. the revision of the window registry, one snapshot of the clients, the
//!    monitors and the lock state, the epoch again: a changed epoch fails (no
//!    sampling loop);
//! 3. an unlocked screen, then the eligible windows (mapped, not hidden, on a
//!    workspace that a monitor shows now, positive bounded size, not a board
//!    or wall window, every overlapped monitor permitted);
//! 4. the limits of 64 windows and 64 monitors (no truncation);
//! 5. the authority check of the read, then a fresh bounded lock check, the
//!    live gates again and the revision: a changed revision fails. Only then
//!    the handles are issued or reused (the revision check and the issue have
//!    no await between them) and the answer is built.
//!
//! Titles and classes are read only inside the eligibility check. They go
//! into no answer, record, log or audit. A window that fails a rule is left
//! out, with no count and no reason.

use std::time::{Duration, Instant};

use fleetdeck_proto::capture::{code as capture_code, text as capture_text};
use fleetdeck_proto::coordinator_desktop_text::{code as p6_code, deadline_ms, limits};
use fleetdeck_proto::coordinator_desktop_wire::{AuthorityPhase, CoordinatorRead};
use fleetdeck_proto::coordinator_input::{InputMonitor, InputRect, InputWindow, InputWindowList};
use serde_json::Value;

use super::ACTIVE_LIMIT;
use super::barrier::{self, Names};
use super::listed::{Binding, Item};
use super::snapshot::{self, Client, Snapshot, TakeError};
use crate::desktop::capture::{Allowed, capture_gate};
use crate::desktop::coordinator::{DispatchCheck, Live, check_live_window_list, p6_fail};
use crate::desktop::events::Kind;
use crate::desktop::run::Runner;
use crate::desktop::{Ctx, Desktop, Fail, info};

/// How long a listing waits for the event socket before its first epoch read
/// (R13 P7b task S2a).
pub const CONNECT_WAIT: Duration = Duration::from_millis(1_000);

/// What a listing needs of the station.
pub struct ListRead<'a> {
    desktop: &'a Desktop,
    deck_fp: &'a str,
    station: &'a str,
    live: &'a (dyn Fn() -> Live + Send + Sync),
}

impl<'a> ListRead<'a> {
    /// A listing on `desktop` for the deck `deck_fp`; `station` is the name
    /// that the answer carries and `live` reads the live settings.
    #[must_use]
    pub fn new(
        desktop: &'a Desktop,
        deck_fp: &'a str,
        station: &'a str,
        live: &'a (dyn Fn() -> Live + Send + Sync),
    ) -> Self {
        Self {
            desktop,
            deck_fp,
            station,
            live,
        }
    }
}

fn failed() -> Fail {
    p6_fail(p6_code::DESKTOP_FAILED)
}

fn too_large() -> Fail {
    p6_fail(p6_code::DESKTOP_RESULT_TOO_LARGE)
}

/// A window that passed every rule, before its handle.
struct Candidate {
    address: String,
    class: String,
    rect: InputRect,
    monitors: Vec<String>,
    focused: bool,
}

/// What the inspection read: the target, the epoch, the revision of the
/// window registry before the snapshot, and the snapshot.
struct Inspected {
    ctx: Ctx,
    epoch: u64,
    revision: u64,
    snap: Snapshot,
}

fn unreadable(e: TakeError) -> Fail {
    match e {
        TakeError::Run(fail) => fail,
        TakeError::Malformed => failed(),
    }
}

/// Steps 2: the epoch, one snapshot, the epoch again.
async fn inspect(
    desktop: &Desktop,
    gate: &(dyn Fn() -> bool + Send + Sync),
) -> Result<Inspected, Fail> {
    let ctx = desktop.context().await?;
    if !desktop.events.wait_connected(CONNECT_WAIT).await {
        return Err(failed());
    }
    let epoch = desktop.events.epoch();
    let revision = desktop.events.revision();
    let runner = Runner {
        env: &ctx.env,
        gate: Some(gate),
        start: None,
    };
    let snap = snapshot::take(&runner, false).await.map_err(unreadable)?;
    if desktop.events.epoch() != epoch {
        return Err(failed());
    }
    Ok(Inspected {
        ctx,
        epoch,
        revision,
        snap,
    })
}

/// The lock state again, after the authority check (R13 P7b §6.1): one read
/// within [`ACTIVE_LIMIT`].
///
/// # Errors
/// `screen_locked`; `desktop_failed` when the state is unknown or the read
/// fails or passes its limit.
async fn still_unlocked(
    seen: &Inspected,
    gate: &(dyn Fn() -> bool + Send + Sync),
) -> Result<(), Fail> {
    let runner = Runner {
        env: &seen.ctx.env,
        gate: Some(gate),
        start: None,
    };
    match tokio::time::timeout(ACTIVE_LIMIT, info::locked(&runner)).await {
        Ok(Some(false)) => Ok(()),
        Ok(Some(true)) => Err(Fail::new(
            capture_code::SCREEN_LOCKED,
            capture_text::SCREEN_LOCKED,
        )),
        Ok(None) | Err(_) => Err(failed()),
    }
}

fn permitted(allowed: &[String], name: &str) -> bool {
    allowed.is_empty() || allowed.iter().any(|a| a == name)
}

/// The rectangle of `client` when its geometry fits the answer (`i32` x and y,
/// `u32` width and height, both positive).
fn rect_of(client: &Client) -> Option<InputRect> {
    if client.rect.w <= 0 || client.rect.h <= 0 {
        return None;
    }
    Some(InputRect {
        x: i32::try_from(client.rect.x).ok()?,
        y: i32::try_from(client.rect.y).ok()?,
        width: u32::try_from(client.rect.w).ok()?,
        height: u32::try_from(client.rect.h).ok()?,
    })
}

/// Step 3: whether `client` is a board or wall window by any barrier rule.
fn is_board(desktop: &Desktop, client: &Client, deck_hosts: &[String]) -> bool {
    let wall = desktop
        .events
        .record(&client.address)
        .is_some_and(|r| r.kind == Kind::Wall);
    let names = Names {
        class: &client.class,
        initial_class: &client.initial_class,
        title: &client.title,
        initial_title: &client.initial_title,
    };
    let kind = if wall { Kind::Wall } else { Kind::Listed };
    barrier::is_board(kind, &names, deck_hosts)
        || desktop
            .events
            .preview_ids_at(&client.address)
            .iter()
            .any(|id| desktop.is_board_origin(id))
}

/// Steps 3 and 4: the permitted monitors and the eligible windows.
fn eligible(
    desktop: &Desktop,
    snap: &Snapshot,
    allowed: &[String],
    deck_hosts: &[String],
) -> Result<(Vec<InputMonitor>, Vec<Candidate>), Fail> {
    if snap.monitors.len() > limits::WINDOW_LIST_MAX_MONITORS as usize {
        return Err(too_large());
    }
    // A monitor whose geometry does not fit the answer is not listed, and
    // a window that overlaps it is left out (as an unpermitted monitor).
    let monitors: Vec<InputMonitor> = snap
        .monitors
        .iter()
        .filter(|m| !m.disabled && permitted(allowed, &m.name))
        .filter_map(|m| {
            Some(InputMonitor {
                name: m.name.clone(),
                x: i32::try_from(m.rect.x).ok()?,
                y: i32::try_from(m.rect.y).ok()?,
                width: u32::try_from(m.rect.w).ok()?,
                height: u32::try_from(m.rect.h).ok()?,
            })
        })
        .collect();
    let mut windows = Vec::new();
    for client in &snap.clients {
        if !snap.eligible_place(client) {
            continue;
        }
        let Some(rect) = rect_of(client) else {
            continue;
        };
        let covered = snap.covered(client);
        let listed = |name: &str| monitors.iter().any(|o| o.name == name);
        if covered.is_empty() || covered.iter().any(|m| !listed(&m.name)) {
            continue;
        }
        if is_board(desktop, client, deck_hosts) {
            continue;
        }
        windows.push(Candidate {
            address: client.address.clone(),
            class: client.class.clone(),
            rect,
            monitors: covered.iter().map(|m| m.name.clone()).collect(),
            focused: client.focused,
        });
    }
    if windows.len() > limits::WINDOW_LIST_MAX_WINDOWS as usize {
        return Err(too_large());
    }
    Ok((monitors, windows))
}

/// The monitors of the answer against the live `capture_monitors`, read
/// after the authority check.
fn monitors_still_permitted(live: &Live, monitors: &[InputMonitor]) -> Result<(), Fail> {
    for monitor in monitors {
        capture_gate(
            &Allowed {
                desktop: true,
                capture: true,
                monitors: live.monitors.clone(),
            },
            Some(&monitor.name),
        )?;
    }
    Ok(())
}

/// One `window_list` read (R13 P7b §6.1): the data of the answer, only after
/// every check passed.
///
/// # Errors
/// The gates (`input_any_disabled`, the capture gate), `screen_locked`,
/// `desktop_failed` (no coordinator context, an unreadable or changing
/// desktop, an invalid answer), `desktop_result_too_large`, the failure of
/// the authority check, and `busy` when the registry is full.
pub async fn read(
    source: &ListRead<'_>,
    deck_hosts: &[String],
    check: &dyn DispatchCheck,
    req: &CoordinatorRead,
) -> Result<Value, Fail> {
    let live = source.live;
    let desktop = source.desktop;
    let context = req.context.as_ref().ok_or_else(failed)?;
    check_live_window_list(&live())?;
    let gate = || live().desktop;
    let limit = Duration::from_millis(deadline_ms::INFO_STATION);
    let seen = tokio::time::timeout(limit, inspect(desktop, &gate))
        .await
        .map_err(|_| p6_fail(p6_code::DESKTOP_READ_TIMEOUT))??;
    if seen.snap.locked != Some(false) {
        return Err(Fail::new(
            capture_code::SCREEN_LOCKED,
            capture_text::SCREEN_LOCKED,
        ));
    }
    let (monitors, windows) = eligible(desktop, &seen.snap, &live().monitors, deck_hosts)?;
    check.check(AuthorityPhase::Read, Some(&req.read)).await?;
    still_unlocked(&seen, &gate).await?;
    let now_live = live();
    check_live_window_list(&now_live)?;
    monitors_still_permitted(&now_live, &monitors)?;
    // The event stream did not change during the checks (no sampling loop:
    // an epoch change is a revision change), and no window opened or closed.
    // The revision check and the reading of
    // the incarnations are one step, and no await follows it until the
    // handles are issued.
    let addresses: Vec<&str> = windows.iter().map(|w| w.address.as_str()).collect();
    let incarnations = desktop
        .events
        .observe_all_at(seen.revision, &addresses)
        .ok_or_else(failed)?;
    let binding = Binding::of(source.deck_fp, context);
    let items: Vec<Item> = windows
        .iter()
        .zip(incarnations)
        .map(|(w, incarnation)| Item {
            address: w.address.clone(),
            class: w.class.clone(),
            incarnation,
        })
        .collect();
    let issued = desktop.listed().issue(
        &binding,
        &seen.ctx.found.signature,
        seen.epoch,
        &items,
        Instant::now(),
    )?;
    let list = InputWindowList {
        station: source.station.to_owned(),
        monitors,
        windows: windows
            .into_iter()
            .zip(&issued.handles)
            .map(|(w, (window_id, left))| InputWindow {
                window_id: window_id.clone(),
                rect: w.rect,
                monitors: w.monitors,
                focused: w.focused,
                expires_in_ms: *left,
            })
            .collect(),
    };
    let value = list
        .validate()
        .map_err(|e| p6_fail(e.code()))
        .and_then(|()| serde_json::to_value(&list).map_err(|_| failed()));
    if value.is_err() {
        desktop.listed().revoke(&issued.fresh);
    }
    value
}

#[cfg(test)]
#[path = "list_tests.rs"]
mod tests;
