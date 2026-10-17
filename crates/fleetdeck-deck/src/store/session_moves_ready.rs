//! What a move needs before it starts (design §12.2; FG-9, T4.4b Q14): the
//! conditions that the deck checks, in the order of §12.2, with the texts of
//! the design. The source station checks them again in `MoveOut`.

use std::collections::BTreeSet;

use fleetdeck_proto::board::DeckSession;
use fleetdeck_proto::event::{ProviderKind, SessionState};
use fleetdeck_proto::fleet_git::{GitHomeState, PushState};
use fleetdeck_proto::integrations::Project;
use fleetdeck_proto::{CardStatus, SessionSummary, StationId};
use rusqlite::{Connection, OptionalExtension};
use serde::Serialize;

use super::land_ready::summary_of;
use super::project_homes::{HomeRow, home_on, homes_of, not_ready_text};
use super::project_sessions::{Assignments, project_of};
use super::session_moves_steps::open_row_of;
use super::{LinkFeature, Result, slug_reservations};

/// A refusal of a move: the code and the text for the board.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MoveRefusal {
    pub code: &'static str,
    pub message: String,
}

impl MoveRefusal {
    #[must_use]
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

fn not_ready<T>(message: impl Into<String>) -> Result<std::result::Result<T, MoveRefusal>> {
    Ok(Err(MoveRefusal::new("move_not_ready", message)))
}

/// What the check reads outside the database: the link state of the two
/// stations for `fleet_git.move.v1`, and the provider instances of the
/// target (`None`: the deck has no `station.info` of it).
#[derive(Debug, Clone)]
pub struct MoveEnv {
    pub src: LinkFeature,
    pub dst: LinkFeature,
    pub dst_instances: Option<BTreeSet<String>>,
}

/// A move that passed the checks.
pub(super) struct Ready {
    /// `native` or `branch_only`.
    pub kind: &'static str,
}

/// The text of `home_unverified` (design §3.8).
const UNVERIFIED: &str =
    "This folder is not a verified home of the project. Check it on the Projects page.";

fn assignments(tx: &Connection) -> Result<Assignments> {
    let mut stmt = tx.prepare("SELECT station_id, session_id, project_id FROM project_sessions")?;
    let rows = stmt
        .query_map([], |r| Ok(((r.get(0)?, r.get(1)?), r.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

/// The project of the session (DL-176), or `None`.
pub(super) fn project_for(
    tx: &Connection,
    station: &str,
    summary: &SessionSummary,
) -> Result<Option<Project>> {
    let projects = super::projects::of_station(tx, station)?;
    let card = DeckSession {
        station_id: StationId::from(station.to_owned()),
        summary: summary.clone(),
        status: CardStatus::Idle,
        unread: None,
        project_id: None,
        review_rev: None,
    };
    Ok(project_of(&projects, &assignments(tx)?, &card)
        .and_then(|id| projects.into_iter().find(|p| p.project_id == id)))
}

fn station_known(tx: &Connection, id: &str) -> Result<bool> {
    Ok(tx
        .query_row("SELECT 1 FROM stations WHERE station_id = ?1", [id], |_| {
            Ok(())
        })
        .optional()?
        .is_some())
}

/// The kind that the station will most likely export: `native` for a session
/// of a known provider (T4.4b Q6). The deck names no provider (layering
/// rule); the export answer of the station decides.
fn kind_of(summary: &SessionSummary) -> &'static str {
    match summary.provider {
        Some(ProviderKind::Unknown) | None => "branch_only",
        Some(_) => "native",
    }
}

fn pushed(summary: &SessionSummary) -> bool {
    summary.push.as_ref().is_some_and(|p| {
        p.state == PushState::Pushed
            && p.oid.is_some()
            && p.desired_oid
                .as_ref()
                .is_none_or(|d| Some(d) == p.oid.as_ref())
    })
}

/// The conditions of §12.2 in order. The refusal of the first one that fails
/// is the inner `Err`.
///
/// # Errors
/// A database or stored-JSON error.
pub(super) fn check(
    tx: &Connection,
    env: &MoveEnv,
    (src, session): (&str, &str),
    dst: &str,
) -> Result<std::result::Result<Ready, MoveRefusal>> {
    let Some(summary) = summary_of(tx, src, session)? else {
        return Ok(Err(MoveRefusal::new(
            "unknown_session",
            "The station does not know this session.",
        )));
    };
    if !station_known(tx, dst)? {
        return Ok(Err(MoveRefusal::new("unknown_station", "Unknown station.")));
    }
    // 1. No running turn and no open request.
    if matches!(
        summary.state,
        SessionState::Starting | SessionState::Stopping
    ) || summary.current_run.is_some()
        || summary.current_turn.is_some()
    {
        return not_ready("A turn is running.");
    }
    if !summary.open_requests.is_empty() {
        return not_ready("An approval is open.");
    }
    // 2. The branch is pushed (a session of a synced project only).
    let project = project_for(tx, src, &summary)?.filter(|p| p.git.sync);
    let Some(project) = project.filter(|_| summary.push.is_some()) else {
        return not_ready("This session is not in a synced project.");
    };
    if !pushed(&summary) {
        return not_ready("The branch is not pushed.");
    }
    // 3. The target.
    let dst_name = slug_reservations::station_name(tx, dst)?;
    if dst == src {
        return not_ready("The session is already on this station.");
    }
    for (feature, name) in [(env.src, src), (env.dst, dst)] {
        match feature {
            LinkFeature::Negotiated => {}
            LinkFeature::Without => {
                return Ok(Err(MoveRefusal::new(
                    "fleet_git_unsupported",
                    "This station does not support Fleet Git. Update it first.",
                )));
            }
            LinkFeature::Down => {
                let name = slug_reservations::station_name(tx, name)?;
                return not_ready(format!("{name} is offline."));
            }
        }
    }
    let Some(home) = home_on(tx, &project.project_id, dst)? else {
        return not_ready(format!("{dst_name} has no home of this project."));
    };
    if let Some(refusal) = home_refusal(&home, &project.name) {
        return Ok(Err(refusal));
    }
    let instance = summary
        .provider_instance
        .as_ref()
        .map(ToString::to_string)
        .unwrap_or_default();
    if !env
        .dst_instances
        .as_ref()
        .is_some_and(|set| set.contains(&instance))
    {
        return not_ready(format!("{dst_name} has no provider named {instance}."));
    }
    // 4. No other move, no land hold.
    if summary.moved_to.is_some() || open_row_of(tx, src, session)?.is_some() {
        return not_ready("The session is already moving.");
    }
    let landing: i64 = tx.query_row(
        "SELECT COUNT(*) FROM land_queue
         WHERE station_id = ?1 AND session_id = ?2 AND state IN ('queued', 'running')",
        [src, session],
        |r| r.get(0),
    )?;
    if landing > 0 {
        return not_ready("The session is being landed.");
    }
    // 5. A session that arrived by a move has confirmed its conversation.
    if summary
        .move_in
        .as_ref()
        .is_some_and(|m| m.state != fleetdeck_proto::fleet_git::MoveInState::Confirmed)
    {
        return not_ready(
            "The session has not confirmed its conversation yet. Send it one prompt first.",
        );
    }
    Ok(Ok(Ready {
        kind: kind_of(&summary),
    }))
}

/// The refusal of a home that cannot receive a session, or `None`.
fn home_refusal(home: &HomeRow, project_name: &str) -> Option<MoveRefusal> {
    if let Some(text) = not_ready_text(&home.wire(), project_name) {
        return Some(MoveRefusal::new("home_not_ready", text));
    }
    if home.state != GitHomeState::Ready || !home.sync_owner {
        return Some(MoveRefusal::new("home_unverified", UNVERIFIED));
    }
    None
}

/// One station of the dialog of a move (T4.4b Q18).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MoveTarget {
    pub station_id: String,
    pub name: String,
    /// `native` or `branch_only`.
    pub kind: String,
    pub ready: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failed: Option<MoveFailed>,
}

/// The first failed condition of a target.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MoveFailed {
    pub code: String,
    pub text: String,
}

/// The stations with a home of the project of the session, each with the
/// result of [`check`]. `envs` holds the link state of every station.
pub(super) fn targets(
    tx: &Connection,
    (src, session): (&str, &str),
    envs: &[(String, MoveEnv)],
) -> Result<Vec<MoveTarget>> {
    let Some(summary) = summary_of(tx, src, session)? else {
        return Ok(Vec::new());
    };
    let Some(project) = project_for(tx, src, &summary)? else {
        return Ok(Vec::new());
    };
    let homes: BTreeSet<String> = homes_of(tx, &project.project_id)?
        .into_iter()
        .map(|h| h.station_id)
        .collect();
    // Each entry of `envs` is a station as a target: `dst` is its link state.
    let source = envs.iter().find(|(id, _)| id == src).map(|(_, e)| e.dst);
    let mut out = Vec::new();
    for (id, env) in envs {
        if id == src || !homes.contains(id) {
            continue;
        }
        let env = MoveEnv {
            src: source.unwrap_or(LinkFeature::Down),
            ..env.clone()
        };
        let failed = check(tx, &env, (src, session), id)?
            .err()
            .map(|r| MoveFailed {
                code: r.code.to_owned(),
                text: r.message,
            });
        out.push(MoveTarget {
            station_id: id.clone(),
            name: slug_reservations::station_name(tx, id)?,
            kind: kind_of(&summary).to_owned(),
            ready: failed.is_none(),
            failed,
        });
    }
    Ok(out)
}
