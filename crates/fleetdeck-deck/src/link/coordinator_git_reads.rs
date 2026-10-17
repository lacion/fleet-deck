//! The station side of the Fleet Git queries of a coordinator (design §13.2;
//! FG-10): `project_branches` and `project_fetch`. The store named one read of
//! the radar home ([`StationRead`]). This module sends it, maps the answer with
//! the deck's stations, sessions and radar rows (the same map as the board
//! route), and puts the Git text into the untrusted envelope (ADR-008 §4.3).
//! Ids, names, counts and states stay outside the envelope.

use std::time::Duration;

use fleetdeck_link::control::Control;
use fleetdeck_proto::fleet_git::{FetchAnswer, RemoteBranches};
use fleetdeck_proto::untrusted::fence;
use fleetdeck_proto::{Reject, RejectCode};
use serde_json::{Value, json};

use super::DeckLink;
use super::coordinator_reads::{ask_within, parse, refusal};
use crate::store::{ReadShape, StationRead};

/// The wait for `project.branches` (decision Q7).
pub const BRANCHES_WAIT: Duration = Duration::from_secs(20);
/// The wait for `project.fetch`: two fetches of 30 s (decision Q7).
pub const FETCH_WAIT: Duration = Duration::from_secs(70);

fn store_error(e: &crate::store::DeckError) -> Reject {
    tracing::warn!(error = %e, "coordinator git read");
    refusal(
        RejectCode::InvalidState,
        "internal: the deck could not read its store",
    )
}

/// Serves one `project_branches` or `project_fetch` read.
pub(super) async fn serve(
    ctx: &DeckLink,
    control: &Control,
    read: StationRead,
) -> Result<Value, Reject> {
    let StationRead {
        method,
        params,
        shape,
        ..
    } = read;
    match shape {
        ReadShape::ProjectBranches { project_id } => {
            let answer: RemoteBranches =
                parse(ask_within(control, BRANCHES_WAIT, method, params).await?)?;
            let project = ctx
                .store
                .project(project_id.clone())
                .await
                .map_err(|e| store_error(&e))?
                .ok_or_else(|| refusal(RejectCode::UnknownRequest, "no such project"))?;
            let mut view = ctx
                .store
                .branch_view(&project, answer)
                .await
                .map_err(|e| store_error(&e))?;
            for b in &mut view.branches {
                b.subject = fence(&format!("git:{project_id}/{}", b.name), &b.subject);
                // A title is a text of a worker: it is not sent.
                if let Some(s) = &mut b.session {
                    s.title = None;
                }
            }
            serde_json::to_value(&view)
                .map_err(|e| refusal(RejectCode::InvalidState, format!("internal: {e}")))
        }
        ReadShape::ProjectFetch { project_id } => {
            let answer: FetchAnswer =
                parse(ask_within(control, FETCH_WAIT, method, params).await?)?;
            let mut out = json!({ "project_id": project_id });
            if let Some(at) = answer.fetched_at {
                out["fetched_at"] = json!(at);
            }
            if let Some(code) = answer.code {
                out["code"] = json!(code);
            }
            if let Some(text) = answer.text {
                out["text"] = json!(fence(&format!("git:fetch/{project_id}"), &text));
            }
            Ok(out)
        }
        _ => Err(refusal(RejectCode::Unsupported, "not a Fleet Git read")),
    }
}
