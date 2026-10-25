//! Requests of a station on its link (M2 P3-7b; G7, ADR-004 §9.3).
//!
//! - `coordinator.call` is decided by the deck's coordinator receipt store
//!   ([`crate::store::coordinator`]); a call that does not parse gets a
//!   rejected receipt that is not stored.
//! - `coordinator.query` is answered by [`crate::store::coordinator_query`]
//!   (M3 6-5, ADR-006 §6.6): the result, or `{"reject": ...}`; no receipt.
//!   An ADR-008 read of a station is served by [`super::coordinator_reads`].
//! - `fleet.view` (MCP P3, `fleet_view.v1`) is answered by
//!   [`crate::store::fleet_view`] for every paired station (scope `admin`);
//!   a refusal is a link error with the code and text of design `mcp-p3.md`
//!   §2.2.
//! - Every other method (`link.rotate_key` of a station) answers
//!   `not_implemented` in this build (DL-050, DL-053).
//!
//! The link layer checks the method's role, feature and grant before a
//! request arrives here; the decision transaction checks the grant again.

use std::time::Duration;

use fleetdeck_link::control::{Control, InboundRequest};
use fleetdeck_link::methods::code;
use fleetdeck_proto::link::{Outcome, WireError};
use fleetdeck_proto::mutation::{
    AuthorityAnswer, AuthorityCheck, CoordinatorOp, CoordinatorQueryEnvelope, Mutation,
};
use fleetdeck_proto::{CommandId, Receipt, StationId};

use super::{DeckLink, NOT_IMPLEMENTED};
use crate::store::coordinator::AuthorityLocal;
use crate::store::fleet_view::FleetViewError;
use crate::store::grant_scopes::NOT_COVERED;
use crate::store::{LinkV2, QueryAnswer};

/// The whole round trip of `coordinator.authority_check` (ADR-008 section
/// 6.2).
pub const AUTHORITY_CHECK_DEADLINE: Duration = Duration::from_secs(5);

/// How long a request on a returning link waits for the return check of its
/// station (B26 section 3.3a). It is below the station's wait limits.
pub const SETTLE_WAIT: Duration = Duration::from_secs(8);

/// The text of the refusal of a request on a link that has not settled.
pub const NOT_SETTLED: &str =
    "The deck has not finished the check of this station after its return. Try again in a minute.";

/// Answers one request of `station` on link `token`.
pub(crate) async fn answer(
    ctx: &DeckLink,
    control: &Control,
    station: StationId,
    token: u64,
    req: InboundRequest,
) {
    if !ctx
        .store
        .wait_link_settled(&station, token, SETTLE_WAIT)
        .await
    {
        let refused = Outcome::Err(WireError::new(code::TIMEOUT, NOT_SETTLED));
        if let Err(e) = control.respond(req.id, refused).await {
            tracing::debug!(error = %e, "response");
        }
        return;
    }
    let outcome = match req.method.as_str() {
        "coordinator.call" => coordinator_call(ctx, station, &req).await,
        "coordinator.query" => coordinator_query(ctx, station, &req).await,
        "coordinator.authority_check" => authority_check(ctx, station, &req).await,
        // R13 P6 (`desktop_mcp.v1`).
        "coordinator.desktop_call" => {
            crate::coordinator_desktop::desktop_call(ctx, station, &req).await
        }
        "coordinator.desktop_query" => {
            crate::coordinator_desktop::desktop_query(ctx, station, &req).await
        }
        "desktop.authority_check" => {
            crate::coordinator_desktop::authority_check(ctx, control, station, &req).await
        }
        // R13 P5b (`desktop_recording.v1`).
        "desktop.capture_lease" => {
            crate::desktop_recording::lease(ctx, control, station, &req).await
        }
        "fleet_git.artifact_authorize" => {
            super::fleet_git_artifacts::artifact_authorize(ctx, station, &req).await
        }
        // R13 P4 (`desktop_shell.v1`): `display.directive` (task E) and
        // `station.palette` (task F1).
        "display.directive" => {
            super::display_directive::display_directive(ctx, &station, req.params.clone()).await
        }
        "station.palette" => {
            super::palette::station_palette(ctx, &station, req.params.clone()).await
        }
        // MCP P3 (`fleet_view.v1`).
        "fleet.view" => fleet_view(ctx, station, &req).await,
        method => Outcome::Err(WireError::new(
            NOT_IMPLEMENTED,
            format!("the deck does not serve `{method}` in this build"),
        )),
    };
    if let Err(e) = control.respond(req.id, outcome).await {
        tracing::debug!(error = %e, "response");
    }
}

/// `fleet.view`: the refusals of design §2.2 are link errors; a store error
/// is a `protocol_error`.
async fn fleet_view(ctx: &DeckLink, station: StationId, req: &InboundRequest) -> Outcome {
    match ctx.store.fleet_view(&station, req.params.clone()).await {
        Ok(page) => match serde_json::to_value(page) {
            Ok(v) => Outcome::Ok(v),
            Err(e) => Outcome::Err(WireError::new(code::PROTOCOL_ERROR, e.to_string())),
        },
        Err(FleetViewError::Refused(refused, text)) => Outcome::Err(WireError::new(refused, text)),
        Err(FleetViewError::Store(e)) => {
            tracing::warn!(error = %e, "fleet view");
            Outcome::Err(WireError::new(code::PROTOCOL_ERROR, "deck store error"))
        }
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
    let lands = matches!(envelope.call.op, CoordinatorOp::Land { .. });
    let moves = matches!(envelope.call.op, CoordinatorOp::Move { .. });
    match ctx.store.decide_coordinator(station, envelope).await {
        Ok(r) => {
            // A queued coordinator land wakes admission (FG-10).
            if lands && r.reject.is_none() {
                ctx.land_kick.kick();
            }
            // A started coordinator move wakes the move task (FG-9).
            if moves && r.reject.is_none() {
                ctx.move_kick.kick();
            }
            receipt(&r)
        }
        Err(e) => {
            tracing::warn!(error = %e, "coordinator receipt");
            Outcome::Err(WireError::new(code::PROTOCOL_ERROR, "deck store error"))
        }
    }
}

async fn coordinator_query(ctx: &DeckLink, station: StationId, req: &InboundRequest) -> Outcome {
    let envelope = match serde_json::from_value::<CoordinatorQueryEnvelope>(req.params.clone()) {
        Ok(envelope) => envelope,
        Err(e) => return Outcome::Err(WireError::new(code::PROTOCOL_ERROR, e.to_string())),
    };
    match ctx.store.coordinator_query(station, envelope).await {
        Ok(Ok(QueryAnswer::Value(v))) => Outcome::Ok(v),
        Ok(Ok(QueryAnswer::Station(read))) => super::coordinator_reads::run(ctx, read).await,
        Ok(Err(reject)) => Outcome::Ok(serde_json::json!({ "reject": reject })),
        Err(e) => {
            tracing::warn!(error = %e, "coordinator query");
            Outcome::Err(WireError::new(code::PROTOCOL_ERROR, "deck store error"))
        }
    }
}

fn invalid(reason: &str) -> AuthorityAnswer {
    AuthorityAnswer {
        valid: false,
        reason: Some(reason.to_owned()),
    }
}

/// `coordinator.authority_check` (ADR-008 section 6.2): the target station
/// asks right before the provider write whether the authority of a command
/// still holds. The deck answers from its own state and from the live policy
/// of the caller station (`coordinator.policy_now`), and fails closed.
async fn authority_check(ctx: &DeckLink, station: StationId, req: &InboundRequest) -> Outcome {
    // The exact rule of every `coordinate` admission (grant stations design
    // §2.3a): a grant in effect names the asking station; nothing else runs.
    match ctx.store.station_covered_now(&station).await {
        Ok(true) => {}
        Ok(false) => {
            return Outcome::Err(WireError::new(code::FORBIDDEN, NOT_COVERED));
        }
        Err(e) => {
            tracing::warn!(error = %e, "authority check");
            return Outcome::Err(WireError::new(code::PROTOCOL_ERROR, "deck store error"));
        }
    }
    let check = match serde_json::from_value::<AuthorityCheck>(req.params.clone()) {
        Ok(check) => check,
        Err(e) => return Outcome::Err(WireError::new(code::PROTOCOL_ERROR, e.to_string())),
    };
    let answer = tokio::time::timeout(
        AUTHORITY_CHECK_DEADLINE,
        authority_answer(ctx, &station, check),
    )
    .await;
    match answer {
        Ok(Ok(outcome)) => outcome,
        Ok(Err(e)) => {
            tracing::warn!(error = %e, "authority check");
            Outcome::Err(WireError::new(code::PROTOCOL_ERROR, "deck store error"))
        }
        Err(_) => Outcome::Ok(
            serde_json::to_value(invalid("caller_unreachable")).unwrap_or(serde_json::Value::Null),
        ),
    }
}

/// The checks that the deck makes alone (ADR-008 section 6.2): the grant, the
/// generation, the deck setting, the command, the launch mode, and the links
/// of the caller station and of the target station. The first reason that
/// fails, else the facts for the target rule.
async fn local_pass(
    ctx: &DeckLink,
    station: &StationId,
    check: &AuthorityCheck,
) -> Result<Result<AuthorityLocal, &'static str>, crate::store::DeckError> {
    let local = match ctx
        .store
        .authority_local(station.clone(), check.clone())
        .await?
    {
        Ok(local) => local,
        Err(reason) => return Ok(Err(reason)),
    };
    let caller = ctx.store.link_v2(&check.caller_station);
    if caller == LinkV2::Down {
        return Ok(Err("caller_unreachable"));
    }
    if caller == LinkV2::Without || ctx.store.link_v2(station) != LinkV2::Negotiated {
        return Ok(Err("no_v2"));
    }
    Ok(Ok(local))
}

/// The outcome of an authority check that is not valid. A pass that finds no
/// grant in effect for the asking station is the wire error `forbidden`
/// (grant stations design §2.3a); every other reason is an answer.
fn refused(reason: &'static str) -> Outcome {
    if reason == NOT_COVERED {
        return Outcome::Err(WireError::new(code::FORBIDDEN, NOT_COVERED));
    }
    answered(&invalid(reason))
}

fn answered(answer: &AuthorityAnswer) -> Outcome {
    match serde_json::to_value(answer) {
        Ok(v) => Outcome::Ok(v),
        Err(e) => Outcome::Err(WireError::new(code::PROTOCOL_ERROR, e.to_string())),
    }
}

async fn authority_answer(
    ctx: &DeckLink,
    station: &StationId,
    check: AuthorityCheck,
) -> Result<Outcome, crate::store::DeckError> {
    // The first pass saves the `policy_now` round trip for an authority that
    // is invalid already.
    if let Err(reason) = local_pass(ctx, station, &check).await? {
        return Ok(refused(reason));
    }
    let Some(policy) = ctx
        .policy_now(&check.caller_station, &check.caller_session)
        .await
    else {
        return Ok(refused("caller_unreachable"));
    };
    // `policy_now` can wait up to 3 s: the grant, the deck setting, the
    // coverage of the station and the links can change meanwhile, so the deck
    // checks them again (each pass reads the clock in its own transaction).
    let local = match local_pass(ctx, station, &check).await? {
        Ok(local) => local,
        Err(reason) => return Ok(refused(reason)),
    };
    // B26 §3.3a: the policy answer holds only for the links of the caller
    // that were settled when the deck asked. A link of the caller that
    // returned since then waits for (or ran) its return check, which can drop
    // the grant after the last pass read it. No await follows this check.
    if !ctx.store.settled_since(&check.caller_station, policy.epoch) {
        return Ok(refused("caller_unreachable"));
    }
    let policy = policy.answer;
    if !policy.bound {
        return Ok(refused("caller_unbound"));
    }
    if !policy.fleet_control {
        return Ok(refused("caller_policy_off"));
    }
    if local.target_excluded {
        return Ok(refused("target_excluded"));
    }
    Ok(answered(&AuthorityAnswer {
        valid: true,
        reason: None,
    }))
}

fn receipt(r: &Receipt) -> Outcome {
    match serde_json::to_value(r) {
        Ok(v) => Outcome::Ok(v),
        Err(e) => Outcome::Err(WireError::new(code::PROTOCOL_ERROR, e.to_string())),
    }
}
