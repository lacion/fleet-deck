//! What a land needs before it is queued and before it is admitted (design
//! §11.1, §11.3; decisions Q6 and Q7): the checks from the read model, and
//! the pinned [`LandPlan`].

use std::collections::BTreeSet;

use fleetdeck_proto::board::DeckSession;
use fleetdeck_proto::event::SessionState;
use fleetdeck_proto::fleet_git::{
    GitHomeState, LandMode, LandPlan, LocalRemotes, PushTarget, RemoteIdentity, remote_identity,
};
use fleetdeck_proto::integrations::Project;
use fleetdeck_proto::{CardStatus, SessionSummary, StationId};
use rusqlite::{Connection, OptionalExtension};

use super::project_homes::{HomeRow, home_on, homes_of};
use super::project_sessions::{Assignments, project_of};
use super::{LinkFeature, Result};

/// A refusal of a land: the code and the text for the board.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LandRefusal {
    pub code: &'static str,
    pub message: String,
}

fn refuse<T>(code: &'static str, message: impl Into<String>) -> Result<Result2<T>> {
    Ok(Err(LandRefusal {
        code,
        message: message.into(),
    }))
}

type Result2<T> = std::result::Result<T, LandRefusal>;

/// What the check reads outside the database.
#[derive(Debug, Clone)]
pub(super) struct Env {
    /// The state of the link of the station for `fleet_git.land.v1`.
    pub feature: LinkFeature,
    /// The stations whose `station.info` allows local remotes (B10).
    pub local_ok: BTreeSet<String>,
}

/// A land that passed the checks.
pub(super) struct Ready {
    pub project: Project,
    pub plan: LandPlan,
}

pub(super) fn summary_of(
    tx: &Connection,
    station: &str,
    session: &str,
) -> Result<Option<SessionSummary>> {
    let text: Option<String> = tx
        .query_row(
            "SELECT summary FROM sessions WHERE station_id = ?1 AND session_id = ?2",
            [station, session],
            |r| r.get(0),
        )
        .optional()?;
    Ok(text.map(|t| serde_json::from_str(&t)).transpose()?)
}

fn assignments(tx: &Connection) -> Result<Assignments> {
    let mut stmt = tx.prepare("SELECT station_id, session_id, project_id FROM project_sessions")?;
    let rows = stmt
        .query_map([], |r| Ok(((r.get(0)?, r.get(1)?), r.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

/// Ended, or idle with no run, turn or open request (design §11.1).
fn can_land(s: &SessionSummary) -> bool {
    match s.state {
        SessionState::Exited | SessionState::Lost => true,
        SessionState::Running => {
            s.current_run.is_none() && s.current_turn.is_none() && s.open_requests.is_empty()
        }
        SessionState::Starting | SessionState::Stopping => false,
    }
}

/// The Fleet Git branch of a session: its worktree and a branch under
/// `fd/<slug>/` of the station (Q6 approximation of `created_branch = 1`).
fn fleet_branch(
    tx: &Connection,
    summary: &SessionSummary,
    repo: &str,
    station: &str,
) -> Result<Option<(String, String)>> {
    let (Some(branch), Some(_)) = (&summary.branch, &summary.worktree) else {
        return Ok(None);
    };
    let mut stmt =
        tx.prepare("SELECT slug FROM slug_reservations WHERE repo = ?1 AND station_id = ?2")?;
    let slugs: Vec<String> = stmt
        .query_map([repo, station], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let under = |name: &str| slugs.iter().any(|s| name.starts_with(&format!("fd/{s}/")));
    let publish = summary
        .push
        .as_ref()
        .map_or_else(|| branch.clone(), |p| p.ref_name.clone());
    Ok((under(branch) || under(&publish)).then(|| (branch.clone(), publish)))
}

/// The checks of §11.1 in the order of decision Q6, and the plan.
///
/// # Errors
/// A database or stored-JSON error. A refusal is the inner `Err`.
pub(super) fn check(
    tx: &Connection,
    env: &Env,
    station: &str,
    session: &str,
) -> Result<Result2<Ready>> {
    let Some(summary) = summary_of(tx, station, session)? else {
        return refuse("unknown_session", "The station does not know this session.");
    };
    let projects = super::projects::of_station(tx, station)?;
    let card = DeckSession {
        station_id: StationId::from(station.to_owned()),
        summary: summary.clone(),
        status: CardStatus::Idle,
        unread: None,
        project_id: None,
        review_rev: None,
    };
    let Some(project) = project_of(&projects, &assignments(tx)?, &card)
        .and_then(|id| projects.into_iter().find(|p| p.project_id == id))
    else {
        return refuse("land_not_ready", "This session is not in a project.");
    };
    if !project.git.sync {
        return refuse("land_not_ready", "This session is not in a synced project.");
    }
    if !can_land(&summary) {
        return refuse(
            "land_not_ready",
            "The session is working. Land it when it is idle or ended.",
        );
    }
    let (Some(repo), Some(dest)) = (project.repo.clone(), project.remote_dest.clone()) else {
        return refuse("home_unverified", UNVERIFIED);
    };
    let Some((branch_ref, publish_ref)) = fleet_branch(tx, &summary, &repo, station)? else {
        return refuse("land_not_ready", "This session has no Fleet Git branch.");
    };
    if env.feature == LinkFeature::Without {
        return refuse(
            "fleet_git_unsupported",
            "This station does not support Fleet Git. Update it first.",
        );
    }
    let home = home_on(tx, &project.project_id, station)?;
    let Some(home) = home.filter(|h: &HomeRow| h.state == GitHomeState::Ready && h.sync_owner)
    else {
        return refuse("home_unverified", UNVERIFIED);
    };
    plan(
        tx,
        env,
        project,
        &home,
        (branch_ref, publish_ref),
        (&summary, session),
        (repo, dest),
    )
}

/// The text of `home_unverified` (T1.3).
const UNVERIFIED: &str =
    "This folder is not a verified home of the project. Check it on the Projects page.";

/// The mode rule, the forge-branch rule and the plan (design §11.1, §11.3).
fn plan(
    tx: &Connection,
    env: &Env,
    project: Project,
    home: &HomeRow,
    (branch_ref, publish_ref): (String, String),
    (summary, session): (&SessionSummary, &str),
    (repo, dest): (String, String),
) -> Result<Result2<Ready>> {
    let settings = project.git.clone();
    let mode = settings.land.unwrap_or(if project.forge.is_some() {
        LandMode::Pr
    } else {
        LandMode::Merge
    });
    let stations: Vec<String> = homes_of(tx, &project.project_id)?
        .into_iter()
        .map(|h| h.station_id)
        .collect();
    let local = if !stations.is_empty() && stations.iter().all(|s| env.local_ok.contains(s)) {
        LocalRemotes::Allow
    } else {
        LocalRemotes::Deny
    };
    let (target, target_dest, target_repo) = match &settings.wip_remote {
        Some(url) => match remote_identity(url, local) {
            Ok(id) => (url.clone(), id.dest, id.repo),
            Err(_) => return refuse("land_not_ready", "The WIP remote is not valid."),
        },
        None => (settings.push_remote.clone(), dest.clone(), repo.clone()),
    };
    if mode == LandMode::Pr {
        let Some(forge) = &project.forge else {
            return refuse(
                "land_not_ready",
                "Pull request mode needs a forge. Bind the project to a forge or choose Merge.",
            );
        };
        let want = format!("{}/{}", forge.host.to_lowercase(), forge.repo);
        if target_repo != want {
            return refuse(
                "land_pr_needs_forge_branch",
                format!(
                    "Pull request mode needs the branch on {want}, but pushes go to {target_repo}. Choose Merge in the project settings, or clear the WIP remote."
                ),
            );
        }
    }
    let title = summary
        .title
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map_or_else(|| branch_ref.clone(), str::to_owned);
    let body = summary
        .digest
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map_or_else(
            || {
                format!(
                    "Landed by Fleet Deck from session {}.",
                    summary.callsign.as_deref().unwrap_or(session)
                )
            },
            str::to_owned,
        );
    let plan = LandPlan {
        project_id: project.project_id.clone(),
        settings_revision: project.git_revision,
        home_generation: home.generation,
        mode,
        fetch_remote: settings.push_remote.clone(),
        remote: RemoteIdentity { repo, dest },
        default_branch: settings.default_branch.clone(),
        branch_ref,
        publish_ref,
        branch_target: PushTarget {
            target,
            dest: target_dest,
        },
        forge: project.forge.clone(),
        check_cmd: settings.check_cmd.clone(),
        check_env: settings.worktree_env.clone(),
        pr_title: title,
        pr_body: body,
    };
    Ok(Ok(Ready { project, plan }))
}
