//! The Fleet Git tools of a coordinator on the deck (design §13.2; FG-10,
//! T4.4a): the queries `project_branches` and `project_fetch` (class
//! `inspect`) and the op `land` (class `manage`).
//!
//! - [`land`] runs in the decision transaction. It queues the land with the
//!   stored authority of the grant, so the receipt and the `land_queue` row
//!   commit together (design §11.2).
//! - The two queries check the target rule (every home of the project is on
//!   a station of the grant) and name one read of the radar home station.
//!   The link layer serves it and fences the Git text (ADR-008 §4.3).

use std::collections::BTreeSet;

use fleetdeck_proto::fleet_git::BranchesParams;
use fleetdeck_proto::fleet_git::FetchParams;
use fleetdeck_proto::mutation::CoordinatorOp;
use fleetdeck_proto::version::feature;
use fleetdeck_proto::{Reject, RejectCode, SessionId, StationId};
use rusqlite::Connection;
use serde_json::json;

use super::branch_map::{SYNC_OFF, UNVERIFIED};
use super::coordinator::strict_target;
use super::coordinator_ops::{Context, Decision};
use super::coordinator_query::{QueryAnswer, ReadShape, StationRead};
use super::grants::Grant;
use super::land_queue::{LandRefusal, LandRequest, LandRequested, request_in_tx};
use super::land_ready::Env;
use super::{DeckError, DeckStore, LinkFeature, LinkV2, Result, new_id};

/// The result of one read query.
type Read = std::result::Result<QueryAnswer, Reject>;

/// The audit target of ops that `AuditCall::new` does not name itself.
pub(super) fn audit_target(op: &CoordinatorOp) -> Option<String> {
    match op {
        CoordinatorOp::Land {
            station,
            session_id,
        } => Some(format!("{station}/{session_id}")),
        _ => None,
    }
}

/// The new land id and command id of a `Land` op; none for another op, so the
/// id sequence of other calls does not change.
pub(super) fn land_ids(store: &DeckStore, op: &CoordinatorOp) -> Option<(String, String)> {
    matches!(op, CoordinatorOp::Land { .. }).then(|| {
        let id = || new_id::<String>(store.inner.ids.as_ref());
        (format!("land-{}", id()), format!("landcmd-{}", id()))
    })
}

/// The rejection for a refusal of the land request (decision Q2).
fn refused(r: LandRefusal) -> Decision {
    let code = match r.code {
        "unknown_session" => return Decision::Rejected(RejectCode::UnknownSession, r.message),
        "fleet_git_unsupported" => RejectCode::Unsupported,
        "command_id_reused" => RejectCode::CommandIdReused,
        _ => RejectCode::InvalidState,
    };
    Decision::Rejected(code, format!("{}: {}", r.code, r.message))
}

/// `land_session` (design §13.2): the target rules of ADR-008 §4.2, then the
/// land request with the authority of the grant.
pub(super) fn land(
    tx: &Connection,
    grant: &Grant,
    ctx: &Context,
    (station, session): (&StationId, &SessionId),
    (command_id, now): (&str, i64),
) -> Result<Decision> {
    if let Some(d) = strict_target(tx, grant, (station, session), (true, now))? {
        return Ok(d);
    }
    let Some((land_id, land_command)) = &ctx.land_ids else {
        return Err(DeckError::Corrupt("land ids".to_owned()));
    };
    let env = Env {
        feature: ctx
            .env
            .land
            .get(station.as_str())
            .copied()
            .unwrap_or(LinkFeature::Down),
        local_ok: ctx
            .infos
            .iter()
            .filter(|(_, i)| i.fleet_git.as_ref().is_some_and(|g| g.allow_local_remotes))
            .map(|(id, _)| id.as_str().to_owned())
            .collect::<BTreeSet<_>>(),
    };
    let req = LandRequest {
        requested_by: format!("coordinator:{}", grant.grant_id),
        request_command_id: command_id.to_owned(),
        station_id: station.as_str().to_owned(),
        session_id: session.as_str().to_owned(),
        authority: grant.authority(),
    };
    match request_in_tx(tx, &env, (land_id, land_command), &req, now)? {
        LandRequested::Queued { .. } => {
            ctx.flags
                .board
                .store(true, std::sync::atomic::Ordering::SeqCst);
            Ok(Decision::Accepted(Some(session.clone())))
        }
        LandRequested::Refused(r) => Ok(refused(r)),
    }
}

fn no(code: RejectCode, message: impl Into<String>) -> Result<Read> {
    Ok(Err(Reject::new(code, message)))
}

impl DeckStore {
    /// The station read of a Fleet Git query, after the target rule of
    /// design §13.2 and the refusals of decision Q11. `shape` and `method`
    /// are those of `project_branches` or `project_fetch`.
    async fn git_read(&self, grant: &Grant, project_id: &str, fetch: bool) -> Result<Read> {
        let Some(found) = self.branch_lookup(project_id).await? else {
            return no(RejectCode::UnknownRequest, "no such project");
        };
        let project = &found.project;
        if project
            .homes
            .iter()
            .any(|h| !grant.stations.contains(&h.station_id))
        {
            return no(
                RejectCode::Forbidden,
                "a home of the project is on a station outside the grant",
            );
        }
        if !project.git.sync {
            return no(RejectCode::InvalidState, format!("sync_off: {SYNC_OFF}"));
        }
        let Some((home, _)) = found.radar else {
            return no(
                RejectCode::InvalidState,
                format!("home_unverified: {UNVERIFIED}"),
            );
        };
        for station in [&grant.station_id, &home] {
            if self.link_v2(station) == LinkV2::Without {
                return no(
                    RejectCode::Unsupported,
                    format!("the link of station `{station}` did not negotiate coordinator.v2"),
                );
            }
        }
        if self.link_feature(&home, feature::FLEET_GIT_BRANCHES) == LinkFeature::Without {
            return no(
                RejectCode::Unsupported,
                format!(
                    "the link of station `{home}` did not negotiate {}",
                    feature::FLEET_GIT_BRANCHES
                ),
            );
        }
        let id = project_id.to_owned();
        let (method, params, shape) = if fetch {
            (
                "project.fetch",
                json!(FetchParams {
                    project_id: id.clone()
                }),
                ReadShape::ProjectFetch { project_id: id },
            )
        } else {
            (
                "project.branches",
                json!(BranchesParams {
                    project_id: id.clone()
                }),
                ReadShape::ProjectBranches { project_id: id },
            )
        };
        Ok(Ok(QueryAnswer::Station(StationRead {
            station: home,
            method,
            params,
            shape,
            source: format!("git:{project_id}"),
        })))
    }

    /// `project_branches` (class `inspect`).
    pub(super) async fn project_branches_read(
        &self,
        grant: &Grant,
        project_id: &str,
    ) -> Result<Read> {
        self.git_read(grant, project_id, false).await
    }

    /// `project_fetch` (class `inspect`).
    pub(super) async fn project_fetch_read(&self, grant: &Grant, project_id: &str) -> Result<Read> {
        self.git_read(grant, project_id, true).await
    }
}
