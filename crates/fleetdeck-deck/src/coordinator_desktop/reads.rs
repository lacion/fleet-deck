//! The protected read of the coordinator desktop tools (R13 P6 §5.7, decision
//! O12-9 item 13 and Q4): the deck registers a read permit, sends
//! `desktop.coordinator_read` on the link of the authorization, and removes
//! the permit when the read ends, fails or is cancelled. The target asks
//! `desktop.authority_check` (phase `read`, [`super::check`]) before it
//! returns data.

use std::time::{Duration, Instant};

use fleetdeck_proto::coordinator_desktop_text::code;
use fleetdeck_proto::coordinator_desktop_wire::{CoordinatorRead, ProtectedRead};
use fleetdeck_proto::link::Outcome;
use serde_json::Value;

use super::authorize::Authorized;
use super::known_code;
use super::permits::PermitGuard;

/// Reads `read` from the target of `auth` (a fresh read, one request). The
/// whole read, from the permit to the answer, takes at most `deadline_ms`.
///
/// # Errors
/// `busy` when no slot is free; `desktop_read_timeout` after the deadline; a
/// code of §11 that the station answered; else `desktop_failed`.
pub(crate) async fn coordinator_read(
    link: &crate::link::DeckLink,
    auth: &Authorized,
    read: ProtectedRead,
    deadline_ms: u64,
) -> Result<Value, &'static str> {
    let wait = Duration::from_millis(deadline_ms);
    let route = auth.route();
    let permits = link.coordinator_desktop.permits();
    let registered = permits.register(
        auth.ctx().clone(),
        (
            auth.target().clone(),
            route.station_node().to_owned(),
            route.epoch().to_owned(),
        ),
        auth.class(),
        read.clone(),
        Instant::now() + wait,
    )?;
    // The permit goes away on every path out of this function.
    let _guard = PermitGuard {
        permits,
        read_id: registered.read_id.clone(),
    };
    let params = serde_json::to_value(CoordinatorRead {
        read_id: registered.read_id,
        read,
        read_hash: registered.read_hash,
        // R13 P7b §7: task K1 sends the context with `InputWindows`.
        context: None,
    })
    .map_err(|_| code::DESKTOP_FAILED)?;
    let outcome = tokio::time::timeout(
        wait,
        route
            .control()
            .request("desktop.coordinator_read", None, params),
    )
    .await
    .map_err(|_| code::DESKTOP_READ_TIMEOUT)?;
    match outcome {
        Ok(Outcome::Ok(value)) => Ok(value),
        // A code of §11 passes through; a wire message never does.
        Ok(Outcome::Err(e)) => Err(known_code(&e.code).unwrap_or(code::DESKTOP_FAILED)),
        Err(_) => Err(code::DESKTOP_FAILED),
    }
}
