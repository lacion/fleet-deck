//! The write adapter of projects and homes (design §8.2, decision 28).
//! Every writer of projects and homes goes through here, in one transaction.
//! The homes are authoritative: `projects.station_id` and `projects.path`
//! are a compatibility copy of the position-0 home, so an old board reads a
//! project as before (§3.9 item 6).
//!
//! - [`create_project`], [`update_project`], [`delete_project`]: the project
//!   writers.
//! - [`set_copy`]: every home write ends with it.
//! - [`repair`]: at deck start, a copy that differs from its home is made
//!   again from the home and logged.

use fleetdeck_proto::fleet_git::GitHomeState;
use fleetdeck_proto::integrations::{Project, ProjectInput};
use rusqlite::{Connection, OptionalExtension, params};

use super::project_homes::{
    HomeRefusal, HomeRow, homes_of, next_generation, set_refused, state_name, supersede_attempts,
    take_ownership,
};
use super::projects::{find, prompt};
use super::{DeckError, Result};

/// The copy of the position-0 home: `projects.station_id` and
/// `projects.path` (design §8.2). A home without a path cannot be the copy.
pub(super) fn set_copy(conn: &Connection, project: &str) -> Result<()> {
    conn.execute(
        "UPDATE projects SET station_id = h.station_id, path = h.path
         FROM project_homes h
         WHERE projects.project_id = ?1 AND h.project_id = ?1 AND h.position = 0
           AND h.path IS NOT NULL",
        [project],
    )?;
    Ok(())
}

/// Inserts the home at position 0 of a project.
fn insert_first_home(
    conn: &Connection,
    project: &str,
    station: &str,
    (generation, state): (u64, GitHomeState),
    path: &str,
    now: i64,
) -> Result<()> {
    conn.execute(
        "INSERT INTO project_homes (project_id, station_id, generation, position, state, path,
             canonical_path, created_at, updated_at)
         VALUES (?1, ?2, ?3, 0, ?4, ?5, ?5, ?6, ?6)",
        params![
            project,
            station,
            i64::try_from(generation).unwrap_or(i64::MAX),
            state_name(state),
            path,
            now
        ],
    )?;
    Ok(())
}

/// Stores a new project of `station_id`: the project, the generation row and
/// the position-0 home (generation 1, `ready`, `path = input.path`), then the
/// copy.
pub(super) fn create_project(
    conn: &mut Connection,
    id: &str,
    station_id: &str,
    input: &ProjectInput,
    now: i64,
) -> Result<Project> {
    let tx = conn.transaction()?;
    tx.execute(
        "INSERT INTO projects (project_id, name, station_id, path, forge, trackers,
             prompt_template, review_prompt, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            id,
            input.name.trim(),
            station_id,
            input.path,
            input
                .forge
                .as_ref()
                .map(serde_json::to_string)
                .transpose()?,
            serde_json::to_string(&input.trackers)?,
            prompt(&input.prompt_template),
            prompt(&input.review_prompt),
            now,
        ],
    )?;
    let generation = next_generation(&tx, id, station_id)?;
    insert_first_home(
        &tx,
        id,
        station_id,
        (generation, GitHomeState::Ready),
        &input.path,
        now,
    )?;
    set_copy(&tx, id)?;
    let project =
        find(&tx, id)?.ok_or_else(|| DeckError::Corrupt("a new project is missing".into()))?;
    tx.commit()?;
    Ok(project)
}

/// The busy test of a home (design §8.4, decision B7): a clone that runs, or
/// a live session in the home. `live` holds the `cwd` of the live sessions of
/// the home's station. The land and move checks come with FG-8 and FG-9.
pub(super) fn busy(conn: &Connection, home: &HomeRow, live: &[String]) -> Result<Option<String>> {
    let clone: bool = conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM home_attempts
             WHERE project_id = ?1 AND station_id = ?2 AND state IN ('sent', 'cloning'))",
        [&home.project_id, &home.station_id],
        |r| r.get(0),
    )?;
    if clone {
        return Ok(Some("A clone runs for this home.".to_owned()));
    }
    // (FG-8/FG-9 add the land and move checks)
    let inside = |cwd: &String| {
        [home.path.as_deref(), home.canonical_path.as_deref()]
            .into_iter()
            .flatten()
            .any(|dir| super::project_homes::path_within(dir, cwd))
    };
    if live.iter().any(inside) {
        return Ok(Some("A session runs in this home.".to_owned()));
    }
    Ok(None)
}

fn refuse(code: &'static str, message: impl Into<String>) -> DeckError {
    DeckError::HomeRefused(HomeRefusal::new(code, message))
}

/// Replaces every field of a project but its id and creation time. With
/// another station or folder the project must have exactly one home and no
/// busy operation (`use_homes`, `home_busy`); the home is replaced by a new
/// generation (design §8.2). `live` holds the `cwd` of the live sessions of
/// the old home's station. `None` when no project has `id`.
pub(super) fn update_project(
    conn: &mut Connection,
    id: &str,
    station_id: &str,
    input: &ProjectInput,
    live: &[String],
    now: i64,
) -> Result<Option<Project>> {
    let tx = conn.transaction()?;
    let old: Option<(String, String)> = tx
        .query_row(
            "SELECT station_id, path FROM projects WHERE project_id = ?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((old_station, old_path)) = old else {
        return Ok(None);
    };
    // A new station or folder is a new home: the Git revision grows and no
    // stored check of the old one applies (FG-5).
    let moved = old_station != station_id || old_path != input.path;
    if moved {
        replace_home(&tx, id, station_id, input, live, now)?;
    }
    tx.execute(
        "UPDATE projects SET name = ?2, forge = ?3, trackers = ?4, prompt_template = ?5,
             review_prompt = ?6, git_revision = git_revision + ?7
         WHERE project_id = ?1",
        params![
            id,
            input.name.trim(),
            input
                .forge
                .as_ref()
                .map(serde_json::to_string)
                .transpose()?,
            serde_json::to_string(&input.trackers)?,
            prompt(&input.prompt_template),
            prompt(&input.review_prompt),
            i64::from(moved),
        ],
    )?;
    if moved {
        super::git_snapshots::clear_checks(&tx, id)?;
    }
    set_copy(&tx, id)?;
    let found = find(&tx, id)?;
    tx.commit()?;
    Ok(found)
}

/// The single home of the project becomes a home on `station` at
/// `input.path` with a new generation: `ready` with sync off, `unverified`
/// with sync on (design §8.2, decision B4b).
fn replace_home(
    conn: &Connection,
    id: &str,
    station: &str,
    input: &ProjectInput,
    live: &[String],
    now: i64,
) -> Result<()> {
    let homes = homes_of(conn, id)?;
    let [home] = homes.as_slice() else {
        return Err(refuse(
            "use_homes",
            "This project has more than one home. Change the homes in the Homes section.",
        ));
    };
    if let Some(reason) = busy(conn, home, live)? {
        return Err(refuse("home_busy", reason));
    }
    supersede_attempts(conn, id, &home.station_id, now)?;
    conn.execute("DELETE FROM project_homes WHERE project_id = ?1", [id])?;
    let sync: bool = conn.query_row(
        "SELECT COALESCE(json_extract(git, '$.sync') = 1, 0) FROM projects WHERE project_id = ?1",
        [id],
        |r| r.get(0),
    )?;
    let generation = next_generation(conn, id, station)?;
    let state = if sync {
        GitHomeState::Unverified
    } else {
        GitHomeState::Ready
    };
    insert_first_home(conn, id, station, (generation, state), &input.path, now)?;
    if sync {
        let found = take_ownership(conn, id, station, &input.path, now)?;
        if let Some((code, text)) = found.refusal() {
            set_refused(conn, (id, station), code, &text, now)?;
        }
    }
    Ok(())
}

/// Deletes a project: the cascade deletes its homes; the generations go with
/// it, the reserved Git names stay (design §8.2).
pub(super) fn delete_project(conn: &mut Connection, id: &str) -> Result<bool> {
    let tx = conn.transaction()?;
    let deleted = tx.execute("DELETE FROM projects WHERE project_id = ?1", [id])? > 0;
    tx.execute("DELETE FROM home_generations WHERE project_id = ?1", [id])?;
    super::git_snapshots::clear_checks(&tx, id)?;
    tx.commit()?;
    Ok(deleted)
}

/// At deck start: the copy of each project is compared with its position-0
/// home, and a difference is made again from the home and logged
/// (`projects_legacy_repaired`). Returns the repaired project ids. A project
/// without any home is logged (`projects_legacy_no_home`) and left alone.
pub(super) fn repair(conn: &Connection) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT p.project_id FROM projects p
         JOIN project_homes h ON h.project_id = p.project_id AND h.position = 0
         WHERE h.path IS NOT NULL AND (p.station_id <> h.station_id OR p.path <> h.path)
         ORDER BY p.project_id",
    )?;
    let ids = stmt
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for id in &ids {
        set_copy(conn, id)?;
        tracing::warn!(project = %id, "projects_legacy_repaired");
    }
    let mut stmt = conn.prepare(
        "SELECT project_id FROM projects
         WHERE NOT EXISTS (SELECT 1 FROM project_homes h WHERE h.project_id = projects.project_id)",
    )?;
    let lost = stmt
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for id in lost {
        tracing::warn!(project = %id, "projects_legacy_no_home");
    }
    Ok(ids)
}

#[cfg(test)]
#[path = "projects_legacy_tests.rs"]
mod tests;
