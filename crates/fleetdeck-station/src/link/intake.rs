//! Requests of a deck link (ADR-004 §9.3, §9.5, §9.6). The control channel
//! already checked the method, the roles, the feature, the scope (from the
//! grant cache) and the `command_id`. Here:
//!
//! - a mutating request is parsed into its typed envelope; a malformed
//!   envelope, a session command without `target_incarnation` (required
//!   over a link) and a refused rotation get a rejected receipt that is not
//!   stored;
//! - the decision transaction re-checks the caller's pin and grant
//!   (`Caller::peer`, G5) and applies the G3 receipt rules;
//! - the response to a mutating request is the `Receipt`.

use fleetdeck_core::identity::PeerKey;
use fleetdeck_link::control::{Control, InboundRequest};
use fleetdeck_link::methods::code;
use fleetdeck_link::revoke::{RotateError, check_rotation, finish_rotation};
use fleetdeck_proto::command::{CommandResult, ReceiptState};
use fleetdeck_proto::link::{Outcome, ReceiptQuery, StationInfo, WireError};
use fleetdeck_proto::mutation::Mutation;
use fleetdeck_proto::{Receipt, Reject, RejectCode};
use serde::Serialize;

use super::StationLink;
use crate::store::{Caller, DecideError, StoreError, peers};

/// The error code of a request that this build does not serve yet.
pub const NOT_IMPLEMENTED: &str = "not_implemented";

fn ok(value: impl Serialize) -> Outcome {
    match serde_json::to_value(value) {
        Ok(v) => Outcome::Ok(v),
        Err(e) => internal(e),
    }
}

fn internal(e: impl std::fmt::Display) -> Outcome {
    Outcome::Err(WireError::new("internal", e.to_string()))
}

fn bad_params(e: impl std::fmt::Display) -> Outcome {
    Outcome::Err(WireError::new(code::PROTOCOL_ERROR, format!("params: {e}")))
}

/// Answers a request that is not `events.replay` or `session.snapshot`
/// (the stream handles those).
pub(super) async fn handle(
    ctx: &StationLink,
    control: &Control,
    peer: &PeerKey,
    req: InboundRequest,
) {
    let id = req.id;
    if let Some(outcome) = answer(ctx, control, peer, req).await
        && let Err(e) = control.respond(id, outcome).await
    {
        tracing::debug!(error = %e, "response");
    }
}

/// The outcome of a request; `None` when it was already answered.
async fn answer(
    ctx: &StationLink,
    control: &Control,
    peer: &PeerKey,
    req: InboundRequest,
) -> Option<Outcome> {
    if req.spec.mutating {
        return mutate(ctx, control, peer, req).await;
    }
    Some(read(ctx, req).await)
}

async fn read(ctx: &StationLink, req: InboundRequest) -> Outcome {
    let store = ctx.manager.store();
    match req.method.as_str() {
        "station.info" => ok(StationInfo {
            name: ctx.name.clone(),
            incarnation: store.incarnation().clone(),
            head: store.head(),
            live_sessions: ctx.manager.live_sessions() as u64,
            version: Some(env!("CARGO_PKG_VERSION").to_owned()),
            max_sessions: Some(ctx.manager.policy().max_sessions()),
            providers: ctx.manager.providers().await,
        }),
        "session.list" => match store.snapshot().await {
            Ok(s) => ok(s.sessions),
            Err(e) => internal(e),
        },
        "command.receipt" => {
            let query: ReceiptQuery = match serde_json::from_value(req.params) {
                Ok(q) => q,
                Err(e) => return bad_params(e),
            };
            match store.receipt(query.command_id).await {
                Ok(r) => ok(r),
                Err(e) => internal(e),
            }
        }
        // The workspace service and its data streams are Phase 6.
        other => Outcome::Err(WireError::new(
            NOT_IMPLEMENTED,
            format!("`{other}` is not served by this station build"),
        )),
    }
}

fn not_stored(command_id: fleetdeck_proto::CommandId, reject: Reject, now: i64) -> Outcome {
    ok(Receipt::rejected(command_id, reject, now))
}

async fn mutate(
    ctx: &StationLink,
    control: &Control,
    peer: &PeerKey,
    req: InboundRequest,
) -> Option<Outcome> {
    let id = req.id;
    let store = ctx.manager.store();
    let now = store.now_ms();
    // `command_id` is present: the control channel refuses a mutating
    // request without one.
    let Some(command_id) = req.command_id.clone() else {
        return Some(Outcome::Err(WireError::new(
            code::MISSING_COMMAND_ID,
            "no command_id",
        )));
    };
    let mutation = match Mutation::parse(&req.method, req.command_id.as_ref(), req.params) {
        Ok(m) => m,
        Err(reject) => return Some(not_stored(command_id, reject, now)),
    };
    let caller = Caller::peer(peer.fingerprint(), req.spec.scope);
    let decided = match mutation {
        Mutation::Session(envelope) => {
            if envelope.target_incarnation.is_none() {
                let reject = Reject::new(
                    RejectCode::InvalidCommand,
                    "target_incarnation is required over a link",
                );
                return Some(not_stored(command_id, reject, now));
            }
            ctx.manager.submit(envelope, caller).await
        }
        Mutation::RotateKey(envelope) => {
            return rotate(ctx, control, id, peer, envelope, caller).await;
        }
        // DL-047: the station policy is read-only in this build.
        m @ Mutation::Configure(_) => {
            store
                .decide_mutation(m, caller, |_| {
                    Err(DecideError::Reject(Reject::new(
                        RejectCode::Unsupported,
                        "station.configure is not supported by this station build",
                    )))
                })
                .await
        }
        // The roles of the method table never deliver it to a station.
        Mutation::Coordinator(_) => {
            let reject = Reject::new(
                RejectCode::InvalidCommand,
                "a station does not hold coordinator calls",
            );
            return Some(not_stored(command_id, reject, now));
        }
    };
    Some(match decided {
        Ok(d) => ok(d.receipt),
        Err(e) => internal(e),
    })
}

fn rotate_reject(e: &RotateError) -> Reject {
    let code = match e {
        RotateError::NotLinkPeer | RotateError::NotPaired => RejectCode::Forbidden,
        RotateError::Store(_) => RejectCode::Unknown,
        _ => RejectCode::InvalidCommand,
    };
    Reject::new(code, e.to_string())
}

/// `link.rotate_key`: the checks of `fleetdeck-link`, then the key change
/// in the decision transaction (one transaction with the receipt), then the
/// link-layer part (pins, grant cache, close of the old key's link).
async fn rotate(
    ctx: &StationLink,
    control: &Control,
    id: u64,
    peer: &PeerKey,
    envelope: fleetdeck_proto::mutation::KeyRotationEnvelope,
    caller: Caller,
) -> Option<Outcome> {
    let store = ctx.manager.store();
    let now = store.now_ms();
    let command_id = envelope.command_id.clone();
    let (new_key, role) = match check_rotation(&ctx.node, peer, &envelope.rotation, &command_id) {
        Ok(checked) => checked,
        Err(e) => return Some(not_stored(command_id, rotate_reject(&e), now)),
    };
    let old = *peer;
    let decided = store
        .decide_mutation(
            Mutation::RotateKey(envelope),
            caller,
            move |d| match peers::rotate_in(d.connection(), &old, &new_key, d.now()) {
                Ok(()) => Ok(CommandResult::default()),
                Err(StoreError::UnknownDeck(k)) => Err(DecideError::Reject(Reject::new(
                    RejectCode::Forbidden,
                    format!("key {k} is not paired"),
                ))),
                Err(e) => Err(DecideError::Store(e)),
            },
        )
        .await;
    match decided {
        Ok(d) => {
            if d.receipt.state == ReceiptState::Accepted && !d.duplicate {
                ctx.peers.rotated_in_store(&old, &new_key);
                // The receipt goes out before the old key's link closes.
                let reply = ok(d.receipt);
                if let Err(e) = control.respond(id, reply).await {
                    tracing::debug!(error = %e, "rotation receipt");
                }
                finish_rotation(&ctx.node, &old, new_key, role);
                return None;
            }
            Some(ok(d.receipt))
        }
        Err(e) => Some(internal(e)),
    }
}
