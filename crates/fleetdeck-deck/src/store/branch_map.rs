//! The branches of other stations (design §13.1; FG-10, T4.4a): the radar home
//! of a project and the map from the answer of `project.branches` to the
//! stations, the sessions and the radar state that the deck knows. The board
//! route and the coordinator query both use it.

use std::collections::HashMap;

use fleetdeck_proto::board::DeckSession;
use fleetdeck_proto::fleet_git::{GitHomeState, RemoteBranch, RemoteBranches, SlugReservation};
use fleetdeck_proto::integrations::Project;
use fleetdeck_proto::{SessionId, StationId};
use rusqlite::params;
use serde::Serialize;

use super::project_sessions::project_of;
use super::radar::effective_radar_home;
use super::{DeckStore, Result, call};

/// The text of a project that does not sync (decision Q6).
pub const SYNC_OFF: &str = "This project does not sync with Git.";
/// The text of a project without a verified home on the radar home (Q6).
pub const UNVERIFIED: &str =
    "This folder is not a verified home of the project. Check it on the Projects page.";

/// A project and the station that holds its radar home.
#[derive(Debug, Clone)]
pub struct BranchLookup {
    pub project: Project,
    /// The radar home station and its name; `None` when the project has no
    /// home there that is `ready` and owns the sync.
    pub radar: Option<(StationId, String)>,
}

/// A session of a branch row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SessionLink {
    pub station_id: StationId,
    pub session_id: SessionId,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub callsign: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

/// The conflict state of a branch (radar, FG-7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Conflict {
    None,
    WillConflict,
}

/// One row of `GET /api/projects/{id}/branches` (decision Q8).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BranchView {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub slug: Option<String>,
    pub callsign: String,
    pub head: String,
    pub ahead: u32,
    pub behind: u32,
    pub at: i64,
    pub subject: String,
    /// A `fd/<callsign>` branch of a session without sync.
    pub legacy: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub station_id: Option<StationId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub station_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session: Option<SessionLink>,
    pub conflict: Conflict,
    pub files: Vec<String>,
}

/// The answer of `GET /api/projects/{id}/branches` (decision Q8).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BranchesView {
    pub remote: String,
    pub default_branch: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_oid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fetched_at: Option<i64>,
    pub truncated: bool,
    pub branches: Vec<BranchView>,
}

/// One will-conflict row of `radar_pairs`: the two sessions and the files.
pub type ConflictRow = ((String, String), (String, String), Vec<String>);

/// What the map reads besides the station's answer.
pub struct MapInput<'a> {
    pub slugs: &'a [SlugReservation],
    /// Station names by station id.
    pub names: &'a HashMap<String, String>,
    /// The sessions of the project.
    pub sessions: &'a [DeckSession],
    pub conflicts: &'a [ConflictRow],
}

/// The one session of `station` (or of any station when `None`) whose
/// publication ref is `name`, else whose branch is `name`. No session when
/// none or more than one matches.
fn find_session<'a>(
    sessions: &'a [DeckSession],
    station: Option<&StationId>,
    name: &str,
) -> Option<&'a DeckSession> {
    let on = |c: &&DeckSession| station.is_none_or(|s| *s == c.station_id);
    // `Some(found)`: this rule decided (a unique session, or an ambiguity).
    let pick = |hits: Vec<&'a DeckSession>| match hits.as_slice() {
        [one] => Some(Some(*one)),
        [] => None,
        _ => Some(None),
    };
    if station.is_some() {
        let by_ref = sessions
            .iter()
            .filter(on)
            .filter(|c| c.summary.push.as_ref().is_some_and(|p| p.ref_name == name))
            .collect();
        if let Some(found) = pick(by_ref) {
            return found;
        }
    }
    let by_branch = sessions
        .iter()
        .filter(on)
        .filter(|c| c.summary.branch.as_deref() == Some(name))
        .collect();
    pick(by_branch).flatten()
}

fn row_of(branch: RemoteBranch, input: &MapInput<'_>) -> BranchView {
    let legacy = branch.slug.is_none();
    let station = branch.slug.as_ref().and_then(|slug| {
        input
            .slugs
            .iter()
            .find(|r| &r.slug == slug)
            .map(|r| r.station_id.clone())
    });
    let found = find_session(input.sessions, station.as_ref(), &branch.name)
        .filter(|_| legacy || station.is_some());
    let session = found.map(|c| SessionLink {
        station_id: c.station_id.clone(),
        session_id: c.summary.session_id.clone(),
        callsign: c.summary.callsign.clone(),
        title: c.summary.title.clone(),
    });
    let mut files: Vec<String> = Vec::new();
    let mut conflict = Conflict::None;
    if let Some(s) = &session {
        let me = (s.station_id.as_str(), s.session_id.as_str());
        for (a, b, f) in input.conflicts {
            let is = |side: &(String, String)| (side.0.as_str(), side.1.as_str()) == me;
            if is(a) || is(b) {
                conflict = Conflict::WillConflict;
                files.extend(f.iter().cloned());
            }
        }
        files.sort();
        files.dedup();
    }
    BranchView {
        station_name: station
            .as_ref()
            .and_then(|s| input.names.get(s.as_str()).cloned()),
        station_id: station,
        name: branch.name,
        slug: branch.slug,
        callsign: branch.callsign,
        head: branch.head,
        ahead: branch.ahead,
        behind: branch.behind,
        at: branch.at,
        subject: branch.subject,
        legacy,
        session,
        conflict,
        files,
    }
}

/// The rows of the answer of a station, mapped (design §13.1). The order is
/// the station's.
#[must_use]
pub fn map_branches(answer: RemoteBranches, input: &MapInput<'_>) -> BranchesView {
    BranchesView {
        remote: answer.remote,
        default_branch: answer.default_branch,
        default_oid: answer.default_oid,
        fetched_at: answer.fetched_at,
        truncated: answer.truncated,
        branches: answer
            .branches
            .into_iter()
            .map(|b| row_of(b, input))
            .collect(),
    }
}

impl DeckStore {
    /// The project and its radar home (design §10.1), or `None` for an
    /// unknown project.
    ///
    /// # Errors
    /// A database or stored-JSON error.
    pub async fn branch_lookup(&self, project_id: &str) -> Result<Option<BranchLookup>> {
        let Some(project) = self.project(project_id.to_owned()).await? else {
            return Ok(None);
        };
        let always_on = self.always_on_stations();
        let station = effective_radar_home(&project, &always_on).filter(|s| {
            project
                .homes
                .iter()
                .any(|h| h.station_id == *s && h.state == GitHomeState::Ready && h.sync_owner)
        });
        let Some(station) = station else {
            return Ok(Some(BranchLookup {
                project,
                radar: None,
            }));
        };
        let name = self
            .stations()
            .await?
            .into_iter()
            .find(|r| r.station_id == station)
            .map_or_else(|| station.to_string(), |r| r.name);
        Ok(Some(BranchLookup {
            project,
            radar: Some((station, name)),
        }))
    }

    /// Maps the answer of `project.branches` for `project` (decisions Q8,
    /// Q9).
    ///
    /// # Errors
    /// A database or stored-JSON error.
    pub async fn branch_view(
        &self,
        project: &Project,
        answer: RemoteBranches,
    ) -> Result<BranchesView> {
        let projects = self.projects().await?;
        let assignments = self.project_assignments().await?;
        let sessions: Vec<DeckSession> = self
            .sessions()
            .await?
            .into_iter()
            .filter(|c| {
                project_of(&projects, &assignments, c).as_deref() == Some(&project.project_id)
            })
            .collect();
        let names: HashMap<String, String> = self
            .stations()
            .await?
            .into_iter()
            .map(|s| (s.station_id.as_str().to_owned(), s.name))
            .collect();
        let id = project.project_id.clone();
        let conflicts: Vec<ConflictRow> = call(&self.inner.db, move |conn| {
            let mut stmt = conn.prepare(
                "SELECT a_station, a_session, b_station, b_session, files FROM radar_pairs
                 WHERE project_id = ?1 AND conflict = 1",
            )?;
            let rows = stmt
                .query_map(params![id], |r| {
                    Ok((
                        (r.get::<_, String>(0)?, r.get::<_, String>(1)?),
                        (r.get::<_, String>(2)?, r.get::<_, String>(3)?),
                        r.get::<_, String>(4)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows.into_iter()
                .map(|(a, b, files)| Ok((a, b, serde_json::from_str(&files)?)))
                .collect()
        })
        .await?;
        let input = MapInput {
            slugs: &project.slugs,
            names: &names,
            sessions: &sessions,
            conflicts: &conflicts,
        };
        Ok(map_branches(answer, &input))
    }
}

#[cfg(test)]
#[path = "branch_map_tests.rs"]
mod tests;
