//! "Merge into project" (Fleet Sync §2): the homes of the project `from` move
//! to the project `target`, then `from` ends. One call, one transaction.

use fleetdeck_proto::fleet_git::{GitHomeState, same_dest, station_slug};
use fleetdeck_proto::integrations::Project;
use rusqlite::{Connection, params};

use super::git_snapshots::clear_checks;
use super::project_homes::{HomeRefusal, homes_of, next_generation, supersede_attempts};
use super::projects::find;
use super::projects_git::derive_remote;
use super::projects_legacy::set_copy;
use super::{DeckStore, Result, call, slug_reservations, to_i64};

type Refusable<T> = std::result::Result<T, HomeRefusal>;

fn refuse<T>(code: &'static str, message: impl Into<String>) -> Refusable<T> {
    Err(HomeRefusal::new(code, message))
}

fn unknown_project() -> HomeRefusal {
    HomeRefusal::new("unknown_project", "Unknown project.")
}

fn table_exists(conn: &Connection, name: &str) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
        [name],
        |r| r.get(0),
    )?)
}

impl DeckStore {
    /// Merges the project `from` into `target` (Fleet Sync §2). Answers the
    /// target after the merge and the number of homes that moved. The checks,
    /// in order: `unknown_project`, `invalid_body`, `needs_remote`,
    /// `not_same_repo`, `merge_sync_on`, `home_exists`, `home_busy` (a home),
    /// `home_busy` (a land), `slug_taken`.
    ///
    /// # Errors
    /// A database or stored-JSON error.
    pub async fn project_merge(
        &self,
        target: String,
        from: String,
    ) -> Result<Refusable<(Project, usize)>> {
        let clone_ssh = self.knobs().clone_ssh;
        let now = self.now_ms();
        let out = call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            let (Some(_), Some(_)) = (find(&tx, &target)?, find(&tx, &from)?) else {
                return Ok(Err(unknown_project()));
            };
            if target == from {
                return Ok(refuse("invalid_body", "Choose another project."));
            }
            derive_remote(&tx, &target, clone_ssh)?;
            derive_remote(&tx, &from, clone_ssh)?;
            let (Some(t), Some(f)) = (find(&tx, &target)?, find(&tx, &from)?) else {
                return Ok(Err(unknown_project()));
            };
            let (Some(t_dest), Some(f_dest)) = (&t.remote_dest, &f.remote_dest) else {
                return Ok(refuse(
                    "needs_remote",
                    "Set the forge or the remote URL of both projects first.",
                ));
            };
            let t_repo = t.repo.clone().unwrap_or_default();
            if !same_dest(t_dest, f_dest) {
                return Ok(refuse(
                    "not_same_repo",
                    format!(
                        "{} uses {}, not {t_repo}.",
                        f.name,
                        f.repo.as_deref().unwrap_or_default()
                    ),
                ));
            }
            if f.git.sync {
                return Ok(refuse(
                    "merge_sync_on",
                    format!("Turn sync off in {} first.", f.name),
                ));
            }
            if f.git_revision > 0 {
                return Ok(refuse(
                    "merge_sync_used",
                    format!(
                        "{} has Fleet Git settings. Merge works only for a project that never changed them.",
                        f.name
                    ),
                ));
            }
            let t_homes = homes_of(&tx, &target)?;
            let f_homes = homes_of(&tx, &from)?;
            if let Some(h) = f_homes
                .iter()
                .find(|h| t_homes.iter().any(|o| o.station_id == h.station_id))
            {
                let station = slug_reservations::station_name(&tx, &h.station_id)?;
                return Ok(refuse(
                    "home_exists",
                    format!(
                        "Both projects have a home on {station}. Remove one of the homes first."
                    ),
                ));
            }
            if f_homes.iter().any(|h| {
                !matches!(h.state, GitHomeState::Ready | GitHomeState::Unverified)
                    || h.path.is_none()
            }) {
                return Ok(refuse(
                    "home_busy",
                    format!("A home of {} is not ready.", f.name),
                ));
            }
            let land: bool = tx.query_row(
                "SELECT EXISTS (SELECT 1 FROM land_queue
                     WHERE project_id = ?1 AND state IN ('queued', 'running'))",
                [&from],
                |r| r.get(0),
            )?;
            if land {
                return Ok(refuse(
                    "home_busy",
                    format!("A land of {} is not finished.", f.name),
                ));
            }
            for h in &f_homes {
                let name = slug_reservations::station_name(&tx, &h.station_id)?;
                let slug = station_slug(&name);
                if let Some(other) =
                    slug_reservations::reserve(&tx, &t_repo, &slug, &h.station_id, now)?
                {
                    let other = slug_reservations::station_name(&tx, &other)?;
                    return Ok(refuse(
                        "slug_taken",
                        format!(
                            "The Git name {slug} is reserved for station {other} in {t_repo}. Rename this station."
                        ),
                    ));
                }
            }
            // The changes (§2.3).
            for h in &f_homes {
                let generation = next_generation(&tx, &target, &h.station_id)?;
                let position: i64 = tx.query_row(
                    "SELECT COALESCE(MAX(position), -1) + 1 FROM project_homes
                     WHERE project_id = ?1",
                    [&target],
                    |r| r.get(0),
                )?;
                let (state, code, message) = if t.git.sync {
                    (
                        "unverified",
                        Some("rebind_pending"),
                        Some("Waiting for the station to check the new remote."),
                    )
                } else {
                    ("ready", None, None)
                };
                tx.execute(
                    "INSERT INTO project_homes (project_id, station_id, generation, position,
                         state, path, canonical_path, attempt_id, sync_owner, code, message,
                         created_at, updated_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL, 0, ?8, ?9, ?10, ?10)",
                    params![
                        target,
                        h.station_id,
                        to_i64(generation)?,
                        position,
                        state,
                        h.path,
                        h.canonical_path,
                        code,
                        message,
                        now
                    ],
                )?;
            }
            tx.execute(
                "UPDATE project_sessions SET project_id = ?1 WHERE project_id = ?2",
                [&target, &from],
            )?;
            tx.execute(
                "UPDATE schedules SET project_id = ?1 WHERE project_id = ?2",
                [&target, &from],
            )?;
            tx.execute("DELETE FROM radar_pairs WHERE project_id = ?1", [&from])?;
            for h in &f_homes {
                supersede_attempts(&tx, &from, &h.station_id, now)?;
            }
            // T2 adds the table `secret_files`.
            if table_exists(&tx, "secret_files")? {
                tx.execute("DELETE FROM secret_files WHERE project_id = ?1", [&from])?;
            }
            // The same statements as `projects_legacy::delete_project`.
            tx.execute("DELETE FROM projects WHERE project_id = ?1", [&from])?;
            tx.execute("DELETE FROM home_generations WHERE project_id = ?1", [&from])?;
            clear_checks(&tx, &from)?;
            set_copy(&tx, &target)?;
            let Some(project) = find(&tx, &target)? else {
                return Ok(Err(unknown_project()));
            };
            tx.commit()?;
            Ok(Ok((project, f_homes.len())))
        })
        .await;
        self.board_changed();
        out
    }
}

#[cfg(test)]
#[path = "project_merge_tests.rs"]
mod tests;
