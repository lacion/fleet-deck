//! The stations of a batch start (FG-5, FG-6; decisions B8, B16, D4, D5).
//! Without `home = "auto"` the batch has one target: the chosen home. With
//! `auto` one placement ([`crate::routing_place::place`]) gives a station for
//! each start of the request, in the request order.

use std::collections::BTreeMap;

use axum::http::StatusCode;
use fleetdeck_link::control::Control;
use fleetdeck_proto::StationId;
use fleetdeck_proto::integrations::{Project, StartSessions};

use super::Board;
use super::guard::ApiError;
use super::placement_routes::{no_station, ready_homes, views, with_instance};
use super::projects_routes::station_of_id;
use crate::routing_place::{PlaceNeeds, place};
use crate::store::StationRecord;

/// The station of a start with the path of its home on that station.
#[derive(Clone)]
pub(super) struct Target {
    pub rec: StationRecord,
    pub control: Control,
    /// `None`: the home still clones, or failed.
    pub path: Option<String>,
}

fn not_a_home() -> ApiError {
    ApiError::new(
        StatusCode::BAD_REQUEST,
        "not_a_home",
        "This folder is not a home of the project.",
    )
}

/// The target of start `index` (request order). A batch without `auto` has
/// one target for every start.
pub(super) fn target_for(targets: &[Target], index: usize) -> &Target {
    &targets[index.min(targets.len() - 1)]
}

/// The number of starts that the request asks for (a review is one start).
fn starts_of(req: &StartSessions) -> u32 {
    if req.review_of_pr.is_some() {
        return 1;
    }
    u32::try_from(req.issues.len())
        .unwrap_or(u32::MAX)
        .saturating_mul(req.count_per_issue)
}

/// The targets of the batch; never empty.
///
/// # Errors
/// 400 `not_a_home`; 404 `unknown_station`; 409 `station_unreachable` /
/// `no_station`.
pub(super) async fn targets(
    board: &Board,
    project: &Project,
    req: &StartSessions,
) -> Result<Vec<Target>, ApiError> {
    if req.home.as_deref() != Some("auto") {
        let home = match req.home.as_deref() {
            Some(station) => project
                .homes
                .iter()
                .find(|h| h.station_id.as_str() == station),
            None => project.homes.iter().find(|h| h.position == 0),
        }
        .ok_or_else(not_a_home)?;
        let (rec, control) = station_of_id(board, home.station_id.as_str()).await?;
        return Ok(vec![Target {
            rec,
            control,
            path: home.path.clone(),
        }]);
    }
    // One placement for the whole batch (FG-6, decision D4).
    let homes = ready_homes(project);
    let mut stations = views(board).await?;
    with_instance(&mut stations, &req.provider_instance);
    let sessions = board.store.sessions().await.map_err(ApiError::internal)?;
    let needs = PlaceNeeds {
        requires: &req.requires,
        prefer: &req.prefer,
        provider: None,
        homes: Some(&homes),
    };
    let placed = place(&stations, &sessions, &needs, starts_of(req), req.spread)
        .map_err(|e| no_station(&e))?;
    let mut by_station: BTreeMap<StationId, Target> = BTreeMap::new();
    let mut out = Vec::with_capacity(placed.len());
    for id in placed {
        if !by_station.contains_key(&id) {
            let (rec, control) = station_of_id(board, id.as_str()).await?;
            let path = project
                .homes
                .iter()
                .find(|h| h.station_id == id)
                .and_then(|h| h.path.clone());
            by_station.insert(id.clone(), Target { rec, control, path });
        }
        if let Some(t) = by_station.get(&id) {
            out.push(t.clone());
        }
    }
    Ok(out)
}
