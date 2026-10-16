//! `desktop.info` and `desktop.act` (R13 §6.3, feature `desktop.v1`). Error
//! codes `unsupported`, `forbidden`, `invalid_params`, `not_found`,
//! `conflict` and `failed`. The deck serves both to the owner login only; the
//! station checks `allow_desktop` again at intake and before every program
//! run.

use fleetdeck_proto::desktop::{DesktopAction, DesktopInfo};
use fleetdeck_proto::link::{Outcome, WireError};

use super::StationLink;
use super::ok;
use crate::desktop::{self, Fail};

fn failed(e: Fail) -> Outcome {
    Outcome::Err(WireError::new(e.code, e.message))
}

/// The live `allow_desktop` of the station policy.
fn gate(ctx: &StationLink) -> impl Fn() -> bool + Send + Sync + use<> {
    let manager = std::sync::Arc::clone(&ctx.manager);
    move || manager.policy().allow_desktop()
}

/// `desktop.info`.
pub(super) async fn info(ctx: &StationLink) -> Outcome {
    let allowed = gate(ctx);
    match desktop::service().info(&allowed).await {
        Ok(info) => ok(info),
        Err(e) => failed(e),
    }
}

/// `desktop.act`.
pub(super) async fn act(ctx: &StationLink, params: serde_json::Value) -> Outcome {
    let action: DesktopAction = match serde_json::from_value(params) {
        Ok(a) => a,
        Err(e) => {
            return failed(Fail::new(
                desktop::code::INVALID_PARAMS,
                format!("params: {e}"),
            ));
        }
    };
    let allowed = gate(ctx);
    match desktop::service().act(&allowed, action).await {
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
