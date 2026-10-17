//! The writes of the homes of a project (design §8.4; FG-5 phase 2):
//! add, retry, remove and the change of the remote (rebind). Each write is
//! one transaction and ends with the copy of the position-0 home. A rule
//! that refuses gives a [`HomeRefusal`]; the routes map its code to a status.

use std::collections::BTreeMap;

use fleetdeck_core::fleet_git_digest::{ensure_home_digest, home_folder_name};
use fleetdeck_proto::fleet_git::{
    EnsureHomeParams, GitHomeState, LocalRemotes, RemoteIdentity, station_slug,
};
use fleetdeck_proto::version::feature;
use fleetdeck_proto::{CardStatus, StationId};
use rusqlite::{Connection, params};
use serde::Serialize;

use super::git_snapshots::clear_checks;
use super::home_attempts::{find as find_attempt, find_by_command, insert as insert_attempt};
use super::project_homes::{
    HomeRefusal, HomeRow, homes_of, next_generation, set_state, supersede_attempts,
};
use super::projects_git::{clone_url, env_bridge, remote_write};
use super::projects_legacy::{busy, set_copy};
use super::{DeckStore, LinkFeature, Result, call, new_id, slug_reservations, to_i64};

type Refusable<T> = std::result::Result<T, HomeRefusal>;

fn refuse<T>(code: &'static str, message: impl Into<String>) -> Refusable<T> {
    Err(HomeRefusal::new(code, message))
}

/// What an add or a retry answers (`202`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HomeAttemptStarted {
    pub generation: u64,
    pub attempt_id: String,
}

/// What the change of the remote answers (`200`, decision B9).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RemoteChanged {
    pub remote_url: String,
    pub remote_ssh_user: Option<String>,
    pub repo: String,
    pub remote_dest: String,
    pub git_revision: u64,
}

impl DeckStore {
    /// The `cwd` of the live sessions of each station: a deck session whose
    /// card status is not `Ended` and not `Lost` (decision B7a).
    ///
    /// # Errors
    /// A database or stored-JSON error.
    pub async fn live_cwds_by_station(&self) -> Result<BTreeMap<String, Vec<String>>> {
        let mut out: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for s in self.sessions().await? {
            if matches!(s.status, CardStatus::Ended | CardStatus::Lost) {
                continue;
            }
            if let Some(cwd) = s.summary.cwd {
                out.entry(s.station_id.0).or_default().push(cwd);
            }
        }
        Ok(out)
    }

    /// The `cwd` of the live sessions of one station.
    ///
    /// # Errors
    /// A database or stored-JSON error.
    pub async fn live_cwds(&self, station: &str) -> Result<Vec<String>> {
        Ok(self
            .live_cwds_by_station()
            .await?
            .remove(station)
            .unwrap_or_default())
    }

    /// Adds a home of `project` on `station` (design §8.4 "Add a home"). The
    /// same `command_id` for the same request returns the first answer. The
    /// checks, in order: `needs_remote`, `home_exists`, the station link,
    /// `fleet_git_unsupported`, `remote_local_refused`,
    /// `station_info_missing`, `slug_taken`.
    ///
    /// # Errors
    /// A database or stored-JSON error.
    pub async fn home_add(
        &self,
        project: String,
        station: String,
        command_id: String,
    ) -> Result<Refusable<HomeAttemptStarted>> {
        let sid = StationId::from(station.clone());
        let link = self.link_feature(&sid, feature::FLEET_GIT_HOMES);
        let info = self.station_info(&sid).map(|(info, _)| {
            (
                info.name.clone(),
                info.fleet_git.is_some_and(|g| g.allow_local_remotes),
            )
        });
        let attempt_id = format!("ha-{}", new_id::<String>(self.inner.ids.as_ref()));
        let now = self.now_ms();
        let out = call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            let Some(p) = super::projects::find(&tx, &project)? else {
                return Ok(Err(unknown_project(&project)));
            };
            if let Some(a) = find_by_command(&tx, &command_id)? {
                return Ok(if a.project_id == project && a.station_id == station && a.retry_of.is_none() {
                    Ok(HomeAttemptStarted {
                        generation: a.generation,
                        attempt_id: a.attempt_id,
                    })
                } else {
                    reused()
                });
            }
            let Some(url) = &p.remote_url else {
                return Ok(refuse(
                    "needs_remote",
                    "Set the remote URL of the project first.",
                ));
            };
            if homes_of(&tx, &project)?.iter().any(|h| h.station_id == station) {
                return Ok(refuse(
                    "home_exists",
                    "This station already has a home of the project.",
                ));
            }
            if link == LinkFeature::Down {
                return Ok(refuse("station_offline", "The station is not connected."));
            }
            if link == LinkFeature::Without {
                return Ok(refuse(
                    "fleet_git_unsupported",
                    "This station does not support Fleet Git. Update it first.",
                ));
            }
            let local_remote = p.remote_dest.as_deref().is_some_and(|d| d.starts_with("file://"));
            if local_remote
                && env_bridge() != LocalRemotes::Allow
                && !info.as_ref().is_some_and(|(_, allowed)| *allowed)
            {
                return Ok(refuse(
                    "remote_local_refused",
                    "This station does not allow a local remote.",
                ));
            }
            let Some((name, _)) = info else {
                return Ok(refuse(
                    "station_info_missing",
                    "The deck does not know this station's name yet. Try again in a moment.",
                ));
            };
            let (Some(repo), Some(dest)) = (&p.repo, &p.remote_dest) else {
                return Ok(refuse(
                    "needs_remote",
                    "Set the remote URL of the project first.",
                ));
            };
            let slug = station_slug(&name);
            if let Some(other) = slug_reservations::reserve(&tx, repo, &slug, &station, now)? {
                let other = slug_reservations::station_name(&tx, &other)?;
                return Ok(refuse(
                    "slug_taken",
                    format!(
                        "The Git name {slug} is reserved for station {other} in {repo}. Rename this station."
                    ),
                ));
            }
            let generation = next_generation(&tx, &project, &station)?;
            let mut request = EnsureHomeParams {
                project_id: project.clone(),
                project_name: p.name.clone(),
                folder: home_folder_name(&p.name),
                url: clone_url(url, p.remote_ssh_user.as_deref()),
                remote: RemoteIdentity {
                    repo: repo.clone(),
                    dest: dest.clone(),
                },
                remote_name: p.git.push_remote.clone(),
                generation,
                attempt_id: attempt_id.clone(),
                request_digest: String::new(),
                retry_of: None,
            };
            request.request_digest = ensure_home_digest(&request);
            let position: i64 = tx.query_row(
                "SELECT COALESCE(MAX(position), -1) + 1 FROM project_homes WHERE project_id = ?1",
                [&project],
                |r| r.get(0),
            )?;
            tx.execute(
                "INSERT INTO project_homes (project_id, station_id, generation, position, state,
                     attempt_id, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, 'cloning', ?5, ?6, ?6)",
                params![project, station, to_i64(generation)?, position, attempt_id, now],
            )?;
            insert_attempt(&tx, &station, &request, &command_id, now)?;
            set_copy(&tx, &project)?;
            tx.commit()?;
            Ok(Ok(HomeAttemptStarted {
                generation,
                attempt_id,
            }))
        })
        .await;
        self.board_changed();
        out
    }

    /// Retries the failed home of `project` on `station` (design §8.4
    /// "Retry"): a new attempt with the same generation and `remote_name`,
    /// `retry_of` the failed attempt. The home is `cloning` again.
    ///
    /// # Errors
    /// A database or stored-JSON error.
    pub async fn home_retry(
        &self,
        project: String,
        station: String,
        command_id: String,
    ) -> Result<Refusable<HomeAttemptStarted>> {
        let attempt_id = format!("ha-{}", new_id::<String>(self.inner.ids.as_ref()));
        let now = self.now_ms();
        let out = call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            let Some(p) = super::projects::find(&tx, &project)? else {
                return Ok(Err(unknown_project(&project)));
            };
            let Some(home) = super::project_homes::home_on(&tx, &project, &station)? else {
                return Ok(Err(unknown_home()));
            };
            if let Some(a) = find_by_command(&tx, &command_id)? {
                return Ok(
                    if a.project_id == project && a.station_id == station && a.retry_of.is_some() {
                        Ok(HomeAttemptStarted {
                            generation: a.generation,
                            attempt_id: a.attempt_id,
                        })
                    } else {
                        reused()
                    },
                );
            }
            if home.state != GitHomeState::Failed {
                return Ok(refuse(
                    "home_not_failed",
                    "Only a failed home can be retried.",
                ));
            }
            let (Some(url), Some(repo), Some(dest)) = (&p.remote_url, &p.repo, &p.remote_dest)
            else {
                return Ok(refuse(
                    "needs_remote",
                    "Set the remote URL of the project first.",
                ));
            };
            let failed = home.attempt_id.clone();
            let remote_name = match failed
                .as_deref()
                .map(|id| find_attempt(&tx, id))
                .transpose()?
            {
                Some(Some(old)) => old.request.remote_name,
                _ => p.git.push_remote.clone(),
            };
            let mut request = EnsureHomeParams {
                project_id: project.clone(),
                project_name: p.name.clone(),
                folder: home_folder_name(&p.name),
                url: clone_url(url, p.remote_ssh_user.as_deref()),
                remote: RemoteIdentity {
                    repo: repo.clone(),
                    dest: dest.clone(),
                },
                remote_name,
                generation: home.generation,
                attempt_id: attempt_id.clone(),
                request_digest: String::new(),
                retry_of: failed,
            };
            request.request_digest = ensure_home_digest(&request);
            insert_attempt(&tx, &station, &request, &command_id, now)?;
            tx.execute(
                "UPDATE project_homes SET attempt_id = ?3, state = 'cloning', code = NULL,
                     message = NULL, updated_at = ?4
                 WHERE project_id = ?1 AND station_id = ?2",
                params![project, station, attempt_id, now],
            )?;
            set_copy(&tx, &project)?;
            tx.commit()?;
            Ok(Ok(HomeAttemptStarted {
                generation: home.generation,
                attempt_id,
            }))
        })
        .await;
        self.board_changed();
        out
    }

    /// Removes the home of `project` on `station` (design §8.4 "Remove a
    /// home"). Refusals, in order: `last_home`, `home_busy`, `no_ready_home`.
    /// No file is deleted.
    ///
    /// # Errors
    /// A database or stored-JSON error.
    pub async fn home_remove(&self, project: String, station: String) -> Result<Refusable<()>> {
        let live = self.live_cwds(&station).await?;
        let now = self.now_ms();
        let out = call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            if super::projects::find(&tx, &project)?.is_none() {
                return Ok(Err(unknown_project(&project)));
            }
            let homes = homes_of(&tx, &project)?;
            let Some(home) = homes.iter().find(|h| h.station_id == station) else {
                return Ok(Err(unknown_home()));
            };
            if homes.len() == 1 {
                return Ok(refuse("last_home", "A project needs one home."));
            }
            if let Some(reason) = busy(&tx, home, &live)? {
                return Ok(refuse("home_busy", reason));
            }
            let promote = homes
                .iter()
                .filter(|h| h.station_id != station && h.state == GitHomeState::Ready)
                .min_by_key(|h| h.position);
            if home.position == 0 && promote.is_none() {
                return Ok(refuse(
                    "no_ready_home",
                    "Another home must be ready before you remove the first home.",
                ));
            }
            supersede_attempts(&tx, &project, &station, now)?;
            tx.execute(
                "DELETE FROM project_homes WHERE project_id = ?1 AND station_id = ?2",
                [&project, &station],
            )?;
            if let (0, Some(next)) = (home.position, promote) {
                tx.execute(
                    "UPDATE project_homes SET position = 0, updated_at = ?3
                     WHERE project_id = ?1 AND station_id = ?2",
                    params![project, next.station_id, now],
                )?;
            }
            set_copy(&tx, &project)?;
            tx.commit()?;
            Ok(Ok(()))
        })
        .await;
        self.board_changed();
        out
    }

    /// Changes the remote URL of `project` (design §8.4 "Change the remote",
    /// decision B9). The same `dest` updates the URL parts only; no previous
    /// URL sets it; another `dest` needs `rebind` and idle homes, and sets
    /// the homes of a sync project `unverified` with `rebind_pending`.
    ///
    /// # Errors
    /// A database or stored-JSON error.
    pub async fn remote_set(
        &self,
        project: String,
        remote_url: String,
        rebind: bool,
    ) -> Result<Refusable<RemoteChanged>> {
        let local = self.local_remotes_of(project.clone()).await?;
        let live = self.live_cwds_by_station().await?;
        let now = self.now_ms();
        let out = call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            let Some(p) = super::projects::find(&tx, &project)? else {
                return Ok(Err(unknown_project(&project)));
            };
            let write = match remote_write(&remote_url, local) {
                Ok(w) => w,
                Err(e) => {
                    return Ok(Err(HomeRefusal::new("invalid_git_settings", e.message).field("remote_url")));
                }
            };
            let homes = homes_of(&tx, &project)?;
            let changes_dest = p
                .remote_dest
                .as_deref()
                .is_some_and(|d| d != write.identity.dest);
            if changes_dest {
                if !rebind {
                    return Ok(refuse(
                        "rebind_needed",
                        format!(
                            "This URL names another repository than {}. Confirm the rebind; every home must match the new remote before sync runs again.",
                            p.repo.as_deref().unwrap_or_default()
                        ),
                    ));
                }
                for home in &homes {
                    let cwds = live.get(&home.station_id).map_or(&[][..], Vec::as_slice);
                    if let Some(reason) = busy(&tx, home, cwds)? {
                        return Ok(refuse("home_busy", reason));
                    }
                }
            }
            let revision = p.git_revision.saturating_add(1);
            tx.execute(
                "UPDATE projects SET remote_url = ?2, remote_ssh_user = ?3, repo = ?4,
                     remote_dest = ?5, git_revision = ?6
                 WHERE project_id = ?1",
                params![
                    project,
                    write.url,
                    write.user,
                    write.identity.repo,
                    write.identity.dest,
                    to_i64(revision)?
                ],
            )?;
            if changes_dest {
                clear_checks(&tx, &project)?;
                if p.git.sync {
                    rebind_homes(&tx, &project, &homes, now)?;
                }
            }
            tx.commit()?;
            Ok(Ok(RemoteChanged {
                remote_url: write.url,
                remote_ssh_user: write.user,
                repo: write.identity.repo,
                remote_dest: write.identity.dest,
                git_revision: revision,
            }))
        })
        .await;
        self.board_changed();
        out
    }
}

/// Every home with a path of a sync project waits for the check of the new
/// remote (decision B4b).
fn rebind_homes(conn: &Connection, project: &str, homes: &[HomeRow], now: i64) -> Result<()> {
    for home in homes.iter().filter(|h| h.path.is_some()) {
        set_state(
            conn,
            (project, &home.station_id),
            GitHomeState::Unverified,
            Some("rebind_pending"),
            Some("Waiting for the station to check the new remote."),
            now,
        )?;
    }
    Ok(())
}

fn unknown_project(_id: &str) -> HomeRefusal {
    HomeRefusal::new("unknown_project", "Unknown project.")
}

fn unknown_home() -> HomeRefusal {
    HomeRefusal::new("unknown_home", "This station has no home of the project.")
}

fn reused<T>() -> Refusable<T> {
    refuse(
        "command_id_reused",
        "This command id was used for another request.",
    )
}
