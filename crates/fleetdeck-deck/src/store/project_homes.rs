//! The homes of a project (design §8.1, §8.2, §8.4, §3.8 rule 4; FG-5 phase
//! 2). `project_homes` is authoritative: `projects.station_id` and
//! `projects.path` are a copy of the position-0 home, written by
//! [`super::projects_legacy`].
//!
//! - [`fill_homes`]: the `homes` and `slugs` of a project list.
//! - Sync ownership: [`claim`] says whether a canonical path is free of
//!   another sync project (the same folder, a folder above it, a folder below
//!   it); [`take_ownership`] writes `sync_owner`.
//! - [`apply_check`]: what a `HomeCheck` of the station does to a home.
//! - [`HomeRefusal`]: a rule of the homes that refuses a write.

use std::path::Path;

use fleetdeck_proto::StationId;
use fleetdeck_proto::fleet_git::{
    GitHomeState, HomeCheck, HomeCheckState, ProjectHome, SlugReservation,
};
use fleetdeck_proto::integrations::Project;
use rusqlite::{Connection, OptionalExtension, Row, params};

use super::projects_git::clone_url;
use super::{DeckError, Result, slug_reservations, to_i64, to_u64};

/// A rule of the homes that refuses a write: the code and the text for the
/// board. The routes map the code to a status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HomeRefusal {
    pub code: &'static str,
    pub message: String,
    /// The setting field of an `invalid_git_settings` refusal.
    pub field: Option<&'static str>,
}

impl HomeRefusal {
    pub(super) fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            field: None,
        }
    }

    pub(super) fn field(mut self, field: &'static str) -> Self {
        self.field = Some(field);
        self
    }
}

/// The columns of a home row, in the order of [`HomeRow::read`].
pub(super) const HOME_COLUMNS: &str = "project_id, station_id, generation, position, state, path, canonical_path, reserved_path, attempt_id, sync_owner, code, message";

/// One row of `project_homes`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct HomeRow {
    pub project_id: String,
    pub station_id: String,
    pub generation: u64,
    pub position: u32,
    pub state: GitHomeState,
    pub path: Option<String>,
    pub canonical_path: Option<String>,
    pub reserved_path: Option<String>,
    pub attempt_id: Option<String>,
    pub sync_owner: bool,
    pub code: Option<String>,
    pub message: Option<String>,
}

/// The stored name of a state.
pub(super) fn state_name(state: GitHomeState) -> &'static str {
    match state {
        GitHomeState::Cloning => "cloning",
        GitHomeState::Ready => "ready",
        GitHomeState::Unverified => "unverified",
        GitHomeState::Failed => "failed",
        GitHomeState::Unknown => "unknown",
    }
}

fn parse_state(name: &str) -> Result<GitHomeState> {
    Ok(match name {
        "cloning" => GitHomeState::Cloning,
        "ready" => GitHomeState::Ready,
        "unverified" => GitHomeState::Unverified,
        "failed" => GitHomeState::Failed,
        other => return Err(DeckError::Corrupt(format!("home state `{other}`"))),
    })
}

impl HomeRow {
    pub(super) fn read(r: &Row<'_>) -> Result<Self> {
        Ok(Self {
            project_id: r.get(0)?,
            station_id: r.get(1)?,
            generation: to_u64(r.get(2)?)?,
            position: u32::try_from(r.get::<_, i64>(3)?)
                .map_err(|_| DeckError::Corrupt("home position".into()))?,
            state: parse_state(&r.get::<_, String>(4)?)?,
            path: r.get(5)?,
            canonical_path: r.get(6)?,
            reserved_path: r.get(7)?,
            attempt_id: r.get(8)?,
            sync_owner: r.get::<_, i64>(9)? == 1,
            code: r.get(10)?,
            message: r.get(11)?,
        })
    }

    /// The folder that holds sessions: the verified canonical path, else the
    /// path.
    pub(super) fn folder(&self) -> Option<&str> {
        self.canonical_path.as_deref().or(self.path.as_deref())
    }

    pub(super) fn wire(&self) -> ProjectHome {
        ProjectHome {
            station_id: StationId::from(self.station_id.clone()),
            generation: self.generation,
            position: self.position,
            state: self.state,
            path: self.path.clone(),
            reserved_path: self.reserved_path.clone(),
            attempt_id: self.attempt_id.clone(),
            sync_owner: self.sync_owner,
            code: self.code.clone(),
            message: self.message.clone(),
        }
    }
}

/// Every home of `project`, by position.
pub(super) fn homes_of(conn: &Connection, project: &str) -> Result<Vec<HomeRow>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {HOME_COLUMNS} FROM project_homes WHERE project_id = ?1 ORDER BY position"
    ))?;
    let rows = stmt.query_map([project], |r| Ok(HomeRow::read(r)))?;
    rows.map(|r| r?).collect()
}

/// The home of `project` on `station`.
pub(super) fn home_on(conn: &Connection, project: &str, station: &str) -> Result<Option<HomeRow>> {
    conn.query_row(
        &format!(
            "SELECT {HOME_COLUMNS} FROM project_homes WHERE project_id = ?1 AND station_id = ?2"
        ),
        params![project, station],
        |r| Ok(HomeRow::read(r)),
    )
    .optional()?
    .transpose()
}

/// The homes on `station` of every project, with each project's id.
pub(super) fn homes_on_station(conn: &Connection, station: &str) -> Result<Vec<HomeRow>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {HOME_COLUMNS} FROM project_homes WHERE station_id = ?1 ORDER BY project_id"
    ))?;
    let rows = stmt.query_map([station], |r| Ok(HomeRow::read(r)))?;
    rows.map(|r| r?).collect()
}

/// Fills `homes` and `slugs` of `projects` (design §8.1).
pub(super) fn fill_homes(conn: &Connection, projects: &mut [Project]) -> Result<()> {
    for p in projects.iter_mut() {
        p.homes = homes_of(conn, &p.project_id)?
            .iter()
            .map(HomeRow::wire)
            .collect();
        p.slugs = match &p.repo {
            Some(repo) => {
                let mut stmt = conn.prepare(
                    "SELECT slug, station_id FROM slug_reservations WHERE repo = ?1 ORDER BY slug",
                )?;
                stmt.query_map([repo], |r| {
                    Ok(SlugReservation {
                        slug: r.get(0)?,
                        station_id: StationId::from(r.get::<_, String>(1)?),
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?
            }
            None => Vec::new(),
        };
    }
    Ok(())
}

/// Is the folder `path` the folder `dir` or inside it (by components)?
#[must_use]
pub fn path_within(dir: &str, path: &str) -> bool {
    Path::new(path).starts_with(dir)
}

/// What a claim of a canonical path found (design §3.8 rule 4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Claim {
    Free,
    /// Another sync project owns this very folder.
    Taken(String),
    /// The folder is inside the home of another sync project.
    Inside(String),
    /// The folder contains the home of another sync project.
    Contains(String),
}

impl Claim {
    /// The code and the text of a refused claim; `None` for a free folder.
    pub(super) fn refusal(&self) -> Option<(&'static str, String)> {
        match self {
            Self::Free => None,
            Self::Taken(name) => Some((
                "home_owned",
                format!("This folder is already the home of project {name} with sync on."),
            )),
            Self::Inside(name) => Some((
                "home_nested",
                format!("This folder is inside the home of project {name}, which has sync on."),
            )),
            Self::Contains(name) => Some((
                "home_nested",
                format!("This folder contains the home of project {name}, which has sync on."),
            )),
        }
    }
}

/// Looks at the sync-owned homes of other projects on `station`, oldest
/// project first, and says what `path` meets. Nothing is written.
pub(super) fn claim(conn: &Connection, project: &str, station: &str, path: &str) -> Result<Claim> {
    let mut stmt = conn.prepare(
        "SELECT p.name, h.canonical_path FROM project_homes h
         JOIN projects p ON p.project_id = h.project_id
         WHERE h.station_id = ?1 AND h.sync_owner = 1 AND h.project_id <> ?2
           AND h.canonical_path IS NOT NULL
         ORDER BY p.created_at, p.project_id",
    )?;
    let others = stmt
        .query_map(params![station, project], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if let Some((name, _)) = others
        .iter()
        .find(|(_, other)| Path::new(other) == Path::new(path))
    {
        return Ok(Claim::Taken(name.clone()));
    }
    if let Some((name, _)) = others.iter().find(|(_, other)| path_within(other, path)) {
        return Ok(Claim::Inside(name.clone()));
    }
    if let Some((name, _)) = others.iter().find(|(_, other)| path_within(path, other)) {
        return Ok(Claim::Contains(name.clone()));
    }
    Ok(Claim::Free)
}

/// Claims `path` for the home of `project` on `station`: when free, the home
/// gets `sync_owner = 1` and `canonical_path = path`. A claim that fails
/// leaves the home as it is.
pub(super) fn take_ownership(
    conn: &Connection,
    project: &str,
    station: &str,
    path: &str,
    now: i64,
) -> Result<Claim> {
    let found = claim(conn, project, station, path)?;
    if found == Claim::Free {
        conn.execute(
            "UPDATE project_homes SET sync_owner = 1, canonical_path = ?3, updated_at = ?4
             WHERE project_id = ?1 AND station_id = ?2",
            params![project, station, path, now],
        )?;
    }
    Ok(found)
}

/// Writes the state of a home with its code and text.
pub(super) fn set_state(
    conn: &Connection,
    (project, station): (&str, &str),
    state: GitHomeState,
    code: Option<&str>,
    message: Option<&str>,
    now: i64,
) -> Result<()> {
    conn.execute(
        "UPDATE project_homes SET state = ?3, code = ?4, message = ?5, updated_at = ?6
         WHERE project_id = ?1 AND station_id = ?2",
        params![project, station, state_name(state), code, message, now],
    )?;
    Ok(())
}

/// A home that cannot be sync owner: `unverified` with the claim's code and
/// text, without ownership.
pub(super) fn set_refused(
    conn: &Connection,
    ids: (&str, &str),
    code: &str,
    message: &str,
    now: i64,
) -> Result<()> {
    conn.execute(
        "UPDATE project_homes SET sync_owner = 0 WHERE project_id = ?1 AND station_id = ?2",
        [ids.0, ids.1],
    )?;
    set_state(
        conn,
        ids,
        GitHomeState::Unverified,
        Some(code),
        Some(message),
        now,
    )
}

/// The wire name of a check state.
fn check_name(state: HomeCheckState) -> String {
    serde_json::to_value(state)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_else(|| "unknown".to_owned())
}

/// The project facts that the texts of a home need.
struct Facts {
    sync: bool,
    repo: Option<String>,
    push_remote: String,
    clone_url: Option<String>,
}

fn facts_of(conn: &Connection, project: &str) -> Result<Option<Facts>> {
    let Some(p) = super::projects::find(conn, project)? else {
        return Ok(None);
    };
    Ok(Some(Facts {
        sync: p.git.sync,
        repo: p.repo,
        push_remote: p.git.push_remote,
        clone_url: p
            .remote_url
            .map(|u| clone_url(&u, p.remote_ssh_user.as_deref())),
    }))
}

/// The text of an unverified home for `code` (design §8.7, §5.7; the
/// decisions B4c). `None` for no code.
fn code_text(
    code: Option<&str>,
    path: &str,
    station: &str,
    seen: Option<&str>,
    facts: &Facts,
) -> Option<String> {
    let code = code?;
    let repo = facts.repo.as_deref().unwrap_or_default();
    Some(match code {
        "mismatch" => match seen {
            Some(seen) => format!("The home {path} on {station} points to {seen}, not {repo}."),
            None => format!(
                "The home {path} on {station} does not point to {repo}. Fix its remote, then try again."
            ),
        },
        "no_remote" => format!(
            "The home {path} has no remote {0}. Add it: git remote add {0} {1}.",
            facts.push_remote,
            facts.clone_url.as_deref().unwrap_or_default()
        ),
        "remote_ambiguous" => format!(
            "The remote {} of {path} has more than one URL. Fleet Deck pushes only to one verified repository; remove the extra url or pushurl.",
            facts.push_remote
        ),
        "missing" => format!("The home {path} on {station} does not exist."),
        "not_a_repo" => format!("The home {path} on {station} is not a Git repository."),
        "refused" => format!("The station refused the home {path}."),
        "rebind_pending" => "Waiting for the station to check the new remote.".to_owned(),
        _ => format!("The station could not verify the home {path}."),
    })
}

/// Applies one check of `station` to its home (decisions B4b, B4c, B13). It
/// changes the homes of sync projects only, and only a home in state `ready`
/// or `unverified` with the generation of the check; it never touches a
/// `cloning` or `failed` home.
pub(super) fn apply_check(
    conn: &Connection,
    station: &str,
    check: &HomeCheck,
    now: i64,
) -> Result<()> {
    let Some(facts) = facts_of(conn, &check.project_id)? else {
        return Ok(());
    };
    if !facts.sync {
        return Ok(());
    }
    let Some(home) = home_on(conn, &check.project_id, station)? else {
        return Ok(());
    };
    if home.generation != check.generation
        || !matches!(home.state, GitHomeState::Ready | GitHomeState::Unverified)
    {
        return Ok(());
    }
    let ids = (home.project_id.as_str(), station);
    if check.state == HomeCheckState::Verified {
        let folder = check
            .canonical_path
            .clone()
            .or_else(|| home.folder().map(str::to_owned))
            .unwrap_or_default();
        return become_ready(conn, ids, &folder, now);
    }
    let code = check
        .code
        .clone()
        .unwrap_or_else(|| check_name(check.state));
    let name = slug_reservations::station_name(conn, station)?;
    let path = home.path.as_deref().unwrap_or_default();
    let seen = check.seen.as_ref().map(|s| s.repo.as_str());
    let text = code_text(Some(&code), path, &name, seen, &facts);
    set_state(
        conn,
        ids,
        GitHomeState::Unverified,
        Some(&code),
        text.as_deref(),
        now,
    )
}

/// A home of a sync project becomes `ready` with the folder `folder`, when no
/// other sync project owns it; else it is `unverified` with `home_owned` or
/// `home_nested` (decision B4b). The home takes sync ownership.
pub(super) fn become_ready(
    conn: &Connection,
    ids: (&str, &str),
    folder: &str,
    now: i64,
) -> Result<()> {
    // The old owner flag goes first: the unique index looks at the folder.
    conn.execute(
        "UPDATE project_homes SET sync_owner = 0 WHERE project_id = ?1 AND station_id = ?2",
        [ids.0, ids.1],
    )?;
    match take_ownership(conn, ids.0, ids.1, folder, now)? {
        Claim::Free => set_state(conn, ids, GitHomeState::Ready, None, None, now),
        other => {
            let (code, text) = other.refusal().unwrap_or(("home_owned", String::new()));
            conn.execute(
                "UPDATE project_homes SET canonical_path = ?3 WHERE project_id = ?1 AND station_id = ?2",
                params![ids.0, ids.1, folder],
            )?;
            set_refused(conn, ids, code, &text, now)
        }
    }
}

/// The largest generation ever used for `project` on `station` (0 for a new
/// pair).
pub(super) fn last_generation(conn: &Connection, project: &str, station: &str) -> Result<u64> {
    let last: Option<i64> = conn
        .query_row(
            "SELECT last_generation FROM home_generations WHERE project_id = ?1 AND station_id = ?2",
            [project, station],
            |r| r.get(0),
        )
        .optional()?;
    last.map_or(Ok(0), to_u64)
}

/// The next generation for the pair; the row is written (upserted).
pub(super) fn next_generation(conn: &Connection, project: &str, station: &str) -> Result<u64> {
    let next = last_generation(conn, project, station)? + 1;
    conn.execute(
        "INSERT INTO home_generations (project_id, station_id, last_generation)
         VALUES (?1, ?2, ?3)
         ON CONFLICT (project_id, station_id) DO UPDATE SET last_generation = excluded.last_generation",
        params![project, station, to_i64(next)?],
    )?;
    Ok(next)
}

/// The refusal text for a home that cannot start a session because it is
/// still cloning or failed (design §8.5, `home_not_ready`); `None` for any
/// other state.
#[must_use]
pub fn not_ready_text(home: &ProjectHome, project_name: &str) -> Option<String> {
    let place = home
        .reserved_path
        .as_deref()
        .or(home.path.as_deref())
        .unwrap_or_default();
    match home.state {
        GitHomeState::Cloning => Some(format!(
            "The home {place} of {project_name} is still cloning."
        )),
        GitHomeState::Failed => Some(format!(
            "The home {place} of {project_name} failed: {}. Retry or remove it.",
            home.message
                .as_deref()
                .unwrap_or("the clone did not finish")
        )),
        _ => None,
    }
}

/// Marks the attempts of the home `(project, station, generation)` that are
/// not over as `superseded`: the home is removed or replaced.
pub(super) fn supersede_attempts(
    conn: &Connection,
    project: &str,
    station: &str,
    now: i64,
) -> Result<()> {
    conn.execute(
        "UPDATE home_attempts SET state = 'superseded', next_send_at = NULL,
             ended_at = COALESCE(ended_at, ?3)
         WHERE project_id = ?1 AND station_id = ?2 AND state <> 'superseded'",
        params![project, station, now],
    )?;
    Ok(())
}

#[cfg(test)]
#[path = "project_homes_tests.rs"]
mod tests;
