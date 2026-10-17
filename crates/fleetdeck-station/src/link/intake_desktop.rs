//! `desktop.info` and `desktop.act` (R13 §6.3, feature `desktop.v1`). Error
//! codes `unsupported`, `forbidden`, `invalid_params`, `not_found`,
//! `conflict` and `failed`. The deck serves both to the owner login only; the
//! station checks `allow_desktop` again at intake and before every program
//! run. `desktop.act` also needs the authority of the deck (feature
//! `desktop_authority.v1`, [`crate::desktop::authority`]).

use fleetdeck_link::control::Control;
use fleetdeck_proto::desktop::{DesktopAction, DesktopInfo, DesktopInfoQuery};
use fleetdeck_proto::desktop_authority::split_params;
use fleetdeck_proto::link::{Outcome, WireError};

use super::StationLink;
use super::ok;
use crate::desktop::{self, Fail, authority};

fn failed(e: Fail) -> Outcome {
    Outcome::Err(WireError::new(e.code, e.message))
}

/// The live `allow_desktop` of the station policy.
fn gate(ctx: &StationLink) -> impl Fn() -> bool + Send + Sync + use<> {
    let manager = std::sync::Arc::clone(&ctx.manager);
    move || manager.policy().allow_desktop()
}

/// `desktop.info`.
pub(super) async fn info(ctx: &StationLink, params: serde_json::Value) -> Outcome {
    let query: DesktopInfoQuery = match serde_json::from_value(params) {
        Ok(q) => q,
        Err(e) => {
            return failed(Fail::new(
                desktop::code::INVALID_PARAMS,
                format!("params: {e}"),
            ));
        }
    };
    let allowed = gate(ctx);
    if query.fresh {
        // The station reads everything again and ignores its 10 s cache.
        return match desktop::service().info_fresh(&allowed).await {
            Ok(info) => ok(info),
            Err(e) => failed(e),
        };
    }
    match desktop::service().info(&allowed).await {
        Ok(info) => ok(info),
        Err(e) => failed(e),
    }
}

/// `desktop.act`: the flat action with the `authority` key of the deck.
pub(super) async fn act(
    ctx: &StationLink,
    control: &Control,
    params: serde_json::Value,
) -> Outcome {
    let invalid = |e: String| failed(Fail::new(desktop::code::INVALID_PARAMS, e));
    let (flat, given) = match split_params(params) {
        Ok(v) => v,
        Err(e) => return invalid(e),
    };
    let action: DesktopAction = match serde_json::from_value(flat) {
        Ok(a) => a,
        Err(e) => return invalid(format!("params: {e}")),
    };
    // The authority check comes before anything reads the desktop. It consumes
    // the `action_id`; the start check runs before the first program.
    let station_node = ctx.node.identity.fingerprint();
    let admitted = match authority::admit(control, &station_node, given, &action) {
        Ok(admitted) => admitted,
        Err(e) => return failed(e),
    };
    let allowed = gate(ctx);
    match desktop::service().act(&allowed, action, admitted).await {
        Ok(result) => ok(result),
        Err(e) => failed(e),
    }
}

/// The `desktop` field of `station.info`: present when a desktop is found,
/// also with `allow_desktop` off.
pub(super) async fn station_info(ctx: &StationLink) -> Option<DesktopInfo> {
    let allowed = gate(ctx);
    match desktop::service().info(&allowed).await {
        Ok(info) => Some(info),
        Err(e) => {
            tracing::debug!(code = e.code, "station.info has no desktop");
            None
        }
    }
}
