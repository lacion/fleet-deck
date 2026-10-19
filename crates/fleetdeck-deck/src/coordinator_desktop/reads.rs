//! The protected read of the coordinator desktop tools (R13 P6 §5.7, decision
//! O12-9 item 13 and Q4): the deck registers a read permit, sends
//! `desktop.coordinator_read` on the link of the authorization, and removes
//! the permit when the read ends, fails or is cancelled. The target asks
//! `desktop.authority_check` (phase `read`, [`super::check`]) before it
//! returns data.

use std::time::{Duration, Instant};

use fleetdeck_proto::capture;
use fleetdeck_proto::coordinator_desktop_text::code;
use fleetdeck_proto::coordinator_desktop_wire::{CoordinatorRead, ProtectedRead};
use fleetdeck_proto::link::Outcome;
use serde_json::Value;

use super::authorize::Authorized;
use super::known_code;
use super::permits::PermitGuard;

/// The station refusals of the `window_list` read that pass to the caller (R13
/// P7b §6.1), besides the codes of [`known_code`]; any other unknown code of
/// that read stays `desktop_failed`.
const LIST_CODES: [&str; 9] = [
    code::INPUT_ANY_DISABLED,
    code::INPUT_SCOPE_NOT_ALLOWED,
    code::INPUT_NOT_ALLOWED,
    capture::code::SCREEN_LOCKED,
    code::BUSY,
    code::DESKTOP_RESULT_TOO_LARGE,
    code::DESKTOP_WINDOW_NOT_FOUND,
    code::DESKTOP_CONTROL_OFF,
    code::DESKTOP_UNSUPPORTED,
];

/// The constant of a passed station refusal of the `window_list` read.
fn window_list_code(is_list: bool, candidate: &str) -> Option<&'static str> {
    if !is_list {
        return None;
    }
    LIST_CODES.iter().copied().find(|c| *c == candidate)
}

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
    // R13 P7b §7: the `window_list` read carries the coordinator context of
    // the authorization (the same context as `desktop.act`); every other read
    // carries none.
    let is_list = matches!(read, ProtectedRead::InputWindows);
    let context = is_list.then(|| auth.ctx().clone());
    let params = serde_json::to_value(CoordinatorRead {
        read_id: registered.read_id,
        read,
        read_hash: registered.read_hash,
        context,
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
        Ok(Outcome::Err(e)) => Err(known_code(&e.code)
            .or_else(|| window_list_code(is_list, &e.code))
            .unwrap_or(code::DESKTOP_FAILED)),
        Err(_) => Err(code::DESKTOP_FAILED),
    }
}
