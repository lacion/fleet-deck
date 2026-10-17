//! `fleet_git.artifact_authorize` (station to deck, FG-12, design §15.5,
//! decision D9): a station asks whether it may use the artifact share of a
//! project for a session. The deck checks the question, finds the station
//! that exports the share's machine, asks it for the verdict
//! (`fleet_git.share_verdict`, 5 s), and answers an `ArtifactAuthorization`
//! that expires 10 minutes after it was issued. A refusal or a doubt is a
//! link error; the station fails closed.

use std::time::Duration;

use fleetdeck_link::control::InboundRequest;
use fleetdeck_proto::StationId;
use fleetdeck_proto::fleet_git::{
    ArtifactAuthorization, ArtifactAuthorizeParams, ExposureState, SharePathState, ShareVerdict,
    ShareVerdictAnswer, ShareVerdictParams, code,
};
use fleetdeck_proto::link::{Outcome, WireError};
use fleetdeck_proto::version::feature;

use super::DeckLink;
use crate::store::LinkFeature;
use crate::store::project_sessions::project_of;

/// The deadline of `fleet_git.share_verdict` (design §15.5).
pub const VERDICT_DEADLINE: Duration = Duration::from_secs(5);

/// How long an authorization lasts (design §15.5): 10 minutes.
pub const AUTHORIZATION_MS: i64 = 600_000;

const EXPORTER_UNCHECKED: &str = "The station that exports the share cannot be checked now.";

fn refuse(code: &'static str, message: impl Into<String>) -> Outcome {
    Outcome::Err(WireError::new(code, message.into()))
}

/// The verdict that the deck gives for the answer of the exporting station
/// (decision D9e): an exposure is `exposed`; a path that is not `ok`, or an
/// exposure state that is not `clear`, is `unknown`; else `ok`.
#[must_use]
pub fn verdict_of(answer: &ShareVerdictAnswer) -> ShareVerdict {
    if answer.exposure_state == ExposureState::Exposed || !answer.exposed.is_empty() {
        ShareVerdict::Exposed
    } else if answer.path_state != SharePathState::Ok
        || answer.exposure_state != ExposureState::Clear
    {
        ShareVerdict::Unknown
    } else {
        ShareVerdict::Ok
    }
}

/// Answers `fleet_git.artifact_authorize` of `caller`.
pub(crate) async fn artifact_authorize(
    ctx: &DeckLink,
    caller: StationId,
    req: &InboundRequest,
) -> Outcome {
    let params = match serde_json::from_value::<ArtifactAuthorizeParams>(req.params.clone()) {
        Ok(p) => p,
        Err(e) => return refuse(code::INVALID_PARAMS, e.to_string()),
    };
    match decide(ctx, &caller, params).await {
        Ok(auth) => match serde_json::to_value(&auth) {
            Ok(v) => Outcome::Ok(v),
            Err(e) => refuse(code::INTERNAL, e.to_string()),
        },
        Err(outcome) => outcome,
    }
}

async fn decide(
    ctx: &DeckLink,
    caller: &StationId,
    params: ArtifactAuthorizeParams,
) -> Result<ArtifactAuthorization, Outcome> {
    let internal = |e: crate::store::DeckError| {
        tracing::warn!(error = %e, "artifact authorize");
        refuse(code::INTERNAL, "deck store error")
    };
    let project = ctx
        .store
        .project(params.project_id.clone())
        .await
        .map_err(internal)?
        .ok_or_else(|| refuse(code::INVALID_PARAMS, "Unknown project."))?;
    if project.git.artifact_share.as_deref() != Some(params.share.as_str()) {
        return Err(refuse(
            code::INVALID_PARAMS,
            "The share is not the artifact share of the project.",
        ));
    }
    // The session is a deck session of the caller, in this project.
    let sessions = ctx.store.sessions().await.map_err(internal)?;
    let assignments = ctx.store.project_assignments().await.map_err(internal)?;
    let projects = ctx.store.projects().await.map_err(internal)?;
    let belongs = sessions
        .iter()
        .find(|s| s.station_id == *caller && s.summary.session_id == params.session_id)
        .is_some_and(|card| {
            project_of(&projects, &assignments, card).as_deref()
                == Some(project.project_id.as_str())
        });
    if !belongs {
        return Err(refuse(
            code::INVALID_PARAMS,
            "The session does not belong to this station and project.",
        ));
    }

    let machine = params
        .share
        .split_once('/')
        .map_or(params.share.as_str(), |(m, _)| m)
        .to_lowercase();
    let share_name = params
        .share
        .split_once('/')
        .map_or("", |(_, n)| n)
        .to_owned();
    let mut exporters = Vec::new();
    for st in ctx.store.stations().await.map_err(internal)? {
        let reported = ctx
            .store
            .station_info(&st.station_id)
            .and_then(|(info, _)| info.taildrive)
            .and_then(|t| t.machine);
        if reported.map(|m| m.to_lowercase()).as_deref() == Some(machine.as_str()) {
            exporters.push(st.station_id);
        }
    }
    let now = ctx.store.now_ms();
    let issued =
        |exporting_station, revision, checked_at, verdict, exposed| ArtifactAuthorization {
            share: params.share.clone(),
            exporting_station,
            inventory_revision: revision,
            verdict,
            exposed,
            checked_at,
            issued_at: now,
            expires_at: now + AUTHORIZATION_MS,
        };
    let exporter = match exporters.as_slice() {
        [] => {
            return Ok(issued(
                None,
                None,
                None,
                ShareVerdict::NotAStation,
                Vec::new(),
            ));
        }
        [one] => one.clone(),
        _ => {
            return Err(refuse(
                code::SHARE_UNVERIFIED,
                format!("More than one station reports the machine {machine}."),
            ));
        }
    };
    let unchecked = || refuse(code::SHARE_UNVERIFIED, EXPORTER_UNCHECKED);
    if ctx.store.link_feature(&exporter, feature::FLEET_GIT_HOMES) != LinkFeature::Negotiated {
        return Err(unchecked());
    }
    let control = ctx.control_for(&exporter).ok_or_else(unchecked)?;
    let body = serde_json::to_value(ShareVerdictParams { share_name }).map_err(|e| {
        tracing::warn!(error = %e, "share verdict params");
        unchecked()
    })?;
    let answer = match tokio::time::timeout(
        VERDICT_DEADLINE,
        control.request("fleet_git.share_verdict", None, body),
    )
    .await
    {
        Ok(Ok(Outcome::Ok(v))) => serde_json::from_value::<ShareVerdictAnswer>(v).map_err(|e| {
            tracing::warn!(station = %exporter, error = %e, "share verdict does not parse");
            unchecked()
        })?,
        Ok(Ok(Outcome::Err(e))) => {
            tracing::warn!(station = %exporter, code = %e.code, "share verdict refused");
            return Err(unchecked());
        }
        Ok(Err(e)) => {
            tracing::debug!(station = %exporter, error = %e, "share verdict: link ended");
            return Err(unchecked());
        }
        Err(_) => {
            tracing::debug!(station = %exporter, "share verdict timed out");
            return Err(unchecked());
        }
    };
    Ok(issued(
        Some(exporter),
        Some(answer.inventory_revision),
        Some(answer.checked_at),
        verdict_of(&answer),
        answer.exposed,
    ))
}
