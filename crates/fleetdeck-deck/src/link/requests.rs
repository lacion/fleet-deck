//! Requests of a station on its link (M2 P3-7b; G7, ADR-004 §9.3).
//!
//! - `coordinator.call` is decided by the deck's coordinator receipt store
//!   ([`crate::store::coordinator`]); a call that does not parse gets a
//!   rejected receipt that is not stored.
//! - Every other method (`coordinator.query`, `link.rotate_key` of a
//!   station) answers `not_implemented` in this build (DL-050, DL-053).
//!
//! The link layer checks the method's role, feature and grant before a
//! request arrives here; the decision transaction checks the grant again.

use fleetdeck_link::control::{Control, InboundRequest};
use fleetdeck_link::methods::code;
use fleetdeck_proto::link::{Outcome, WireError};
use fleetdeck_proto::mutation::Mutation;
use fleetdeck_proto::{CommandId, Receipt, StationId};

use super::{DeckLink, NOT_IMPLEMENTED};

/// Answers one request of `station`.
pub(crate) async fn answer(
    ctx: &DeckLink,
    control: &Control,
    station: StationId,
    req: InboundRequest,
) {
    let outcome = match req.method.as_str() {
        "coordinator.call" => coordinator_call(ctx, station, &req).await,
        method => Outcome::Err(WireError::new(
            NOT_IMPLEMENTED,
            format!("the deck does not serve `{method}` in this build"),
        )),
    };
    if let Err(e) = control.respond(req.id, outcome).await {
        tracing::debug!(error = %e, "response");
    }
}

async fn coordinator_call(ctx: &DeckLink, station: StationId, req: &InboundRequest) -> Outcome {
    let now = ctx.store.now_ms();
    let envelope = match Mutation::parse(&req.method, req.command_id.as_ref(), req.params.clone()) {
        Ok(Mutation::Coordinator(envelope)) => envelope,
        Ok(_) => {
            return Outcome::Err(WireError::new(
                code::PROTOCOL_ERROR,
                "`coordinator.call` carried another mutation",
            ));
        }
        Err(reject) => {
            let id = req
                .command_id
                .clone()
                .unwrap_or_else(|| CommandId::from(String::new()));
            return receipt(&Receipt::rejected(id, reject, now));
        }
    };
    match ctx.store.decide_coordinator(station, envelope).await {
        Ok(r) => receipt(&r),
        Err(e) => {
            tracing::warn!(error = %e, "coordinator receipt");
            Outcome::Err(WireError::new(code::PROTOCOL_ERROR, "deck store error"))
        }
    }
}

fn receipt(r: &Receipt) -> Outcome {
    match serde_json::to_value(r) {
        Ok(v) => Outcome::Ok(v),
        Err(e) => Outcome::Err(WireError::new(code::PROTOCOL_ERROR, e.to_string())),
    }
}
