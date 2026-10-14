//! The project of a session (DL-176). The owner can assign a session to a
//! project, say that it has none, or go back to the automatic match. The
//! automatic match uses the folder of the session, the main folder of its
//! worktree, and the forge binding of the project against the origin of the
//! repository.

use std::collections::BTreeMap;

use fleetdeck_proto::board::DeckSession;
use fleetdeck_proto::integrations::Project;
use fleetdeck_proto::{SessionId, StationId};
use rusqlite::{Connection, params};

use super::{DeckStore, Principal, Result, call};

/// The manual choices, by (station id, session id): `Some(project)` is an
/// assignment and `None` is "no project".
pub type Assignments = BTreeMap<(String, String), Option<String>>;

/// The suffix that the station adds to the name of a session worktree
/// (`<repo>--fd-<callsign>`).
const WORKTREE_MARK: &str = "--fd-";

/// `path` without trailing slashes (the root stays `/`).
fn trimmed(path: &str) -> &str {
    let t = path.trim_end_matches('/');
    if t.is_empty() && path.starts_with('/') {
        "/"
    } else {
        t
    }
}

/// Is `path` the folder `dir` or a folder below it?
fn within(dir: &str, path: &str) -> bool {
    let (dir, path) = (trimmed(dir), trimmed(path));
    if dir == "/" {
        return path.starts_with('/');
    }
    path == dir || path.strip_prefix(dir).is_some_and(|r| r.starts_with('/'))
}

/// The main checkout of a session worktree: `path` with the suffix
/// `--fd-<callsign>` removed from its last component. `None` when the last
/// component does not end with the suffix of this callsign.
fn main_path(path: &str, callsign: &str) -> Option<String> {
    let path = trimmed(path);
    let (parent, name) = path.rsplit_once('/')?;
    let base = name.strip_suffix(&format!("{WORKTREE_MARK}{callsign}"))?;
    if base.is_empty() {
        return None;
    }
    Some(format!("{parent}/{base}"))
}

/// The origin that a forge binding gives: `<host>/<repo>` in lower case.
fn forge_origin(project: &Project) -> Option<String> {
    let forge = project.forge.as_ref()?;
    Some(format!("{}/{}", forge.host, forge.repo).to_lowercase())
}

/// The project of a card.
///
/// - A manual row decides: a project, or none.
/// - Else the longest `path` of a project of the same station that is the
///   `cwd`, a parent of it, the main checkout of the session worktree, or a
///   parent of that. The main checkout is the last component of
///   `summary.worktree` (or of `cwd`) without `--fd-<callsign>` of the
///   session.
/// - Else the project of the same station whose forge binding equals
///   `summary.repo.origin`. Several such projects give none.
#[must_use]
pub fn project_of(
    projects: &[Project],
    assignments: &Assignments,
    card: &DeckSession,
) -> Option<String> {
    let summary = &card.summary;
    let station = card.station_id.as_str();
    let key = (station.to_owned(), summary.session_id.as_str().to_owned());
    if let Some(manual) = assignments.get(&key) {
        return manual.clone();
    }
    let mine = || projects.iter().filter(|p| p.station_id == station);
    let mut candidates: Vec<String> = summary.cwd.iter().cloned().collect();
    if let Some(callsign) = summary.callsign.as_deref() {
        let source = summary.worktree.as_deref().or(summary.cwd.as_deref());
        candidates.extend(source.and_then(|p| main_path(p, callsign)));
    }
    let by_path = mine()
        .filter(|p| candidates.iter().any(|c| within(&p.path, c)))
        .reduce(|best, p| {
            if trimmed(&p.path).len() > trimmed(&best.path).len() {
                p
            } else {
                best
            }
        });
    if let Some(project) = by_path {
        return Some(project.project_id.clone());
    }
    let origin = summary.repo.as_ref()?.origin.as_deref()?.to_lowercase();
    let mut same = mine().filter(|p| forge_origin(p).as_deref() == Some(origin.as_str()));
    match (same.next(), same.next()) {
        (Some(only), None) => Some(only.project_id.clone()),
        _ => None,
    }
}

/// Stores the manual choice of a session inside the caller's transaction (a
/// coordinator's `move_to_project` commits it with the receipt).
pub(super) fn set_project_tx(
    conn: &Connection,
    station: &StationId,
    session: &SessionId,
    project: Option<&str>,
    now: i64,
) -> Result<()> {
    conn.execute(
        "INSERT INTO project_sessions (station_id, session_id, project_id, assigned_at)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT (station_id, session_id) DO UPDATE SET
             project_id = excluded.project_id, assigned_at = excluded.assigned_at",
        params![station.as_str(), session.as_str(), project, now],
    )?;
    Ok(())
}

/// Removes the manual choice of a session inside the caller's transaction;
/// `false` when there was none.
pub(super) fn reset_project_tx(
    conn: &Connection,
    station: &StationId,
    session: &SessionId,
) -> Result<bool> {
    Ok(conn.execute(
        "DELETE FROM project_sessions WHERE station_id = ?1 AND session_id = ?2",
        params![station.as_str(), session.as_str()],
    )? > 0)
}

impl DeckStore {
    /// Every manual choice.
    ///
    /// # Errors
    /// A database error.
    pub async fn project_assignments(&self) -> Result<Assignments> {
        call(&self.inner.db, |conn| {
            let mut stmt =
                conn.prepare("SELECT station_id, session_id, project_id FROM project_sessions")?;
            let rows = stmt
                .query_map([], |r| Ok(((r.get(0)?, r.get(1)?), r.get(2)?)))?
                .collect::<rusqlite::Result<_>>()?;
            Ok(rows)
        })
        .await
    }

    /// Stores the manual choice of a session: a project, or `None` for "no
    /// project". The caller checked that the project and the session exist
    /// and that they share a station.
    ///
    /// # Errors
    /// A database error.
    pub async fn set_session_project(
        &self,
        station: String,
        session: String,
        project: Option<String>,
    ) -> Result<()> {
        let now = self.now_ms();
        call(&self.inner.db, move |conn| {
            set_project_tx(
                conn,
                &StationId(station),
                &SessionId(session),
                project.as_deref(),
                now,
            )
        })
        .await?;
        self.board_changed();
        Ok(())
    }

    /// Removes the manual choice of a session, so that the automatic match
    /// applies again. `false` when there was none.
    ///
    /// # Errors
    /// A database error.
    pub async fn reset_session_project(&self, station: String, session: String) -> Result<bool> {
        let removed = call(&self.inner.db, move |conn| {
            reset_project_tx(conn, &StationId(station), &SessionId(session))
        })
        .await?;
        if removed {
            self.board_changed();
        }
        Ok(removed)
    }

    /// Sets `project_id` on each card. A member gets none: the id of a
    /// project is not for a member.
    ///
    /// # Errors
    /// A database or stored-JSON error.
    pub(super) async fn fill_projects(
        &self,
        principal: &Principal,
        cards: &mut [DeckSession],
    ) -> Result<()> {
        if matches!(principal, Principal::Member(..)) {
            return Ok(());
        }
        let projects = self.projects().await?;
        let assignments = self.project_assignments().await?;
        for card in cards {
            card.project_id = project_of(&projects, &assignments, card);
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "project_sessions_tests.rs"]
mod tests;
