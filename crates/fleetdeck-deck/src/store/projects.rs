//! The projects of the board (R3, DL-164): a repository on a station with
//! its forge and tracker bindings and the prompts for sessions that start
//! from a ticket or a review. The deck stores bindings only, never a token.

use fleetdeck_proto::fleet_git::GitSettings;
use fleetdeck_proto::integrations::{ForgeBinding, Project, ProjectInput, TrackerBinding};
use rusqlite::{Connection, OptionalExtension, Row, params};

use super::{DeckError, DeckStore, Result, call, new_id};

const COLUMNS: &str = "project_id, name, station_id, path, forge, trackers, prompt_template, review_prompt, created_at, git, git_revision, repo, remote_url, remote_ssh_user, remote_dest";

/// The columns that a new project sets; the Git columns keep their defaults.
const INSERT_COLUMNS: &str = "project_id, name, station_id, path, forge, trackers, prompt_template, review_prompt, created_at";

struct Raw {
    project_id: String,
    name: String,
    station_id: String,
    path: String,
    forge: Option<String>,
    trackers: String,
    prompt_template: Option<String>,
    review_prompt: Option<String>,
    created_at: i64,
    git: String,
    git_revision: i64,
    repo: Option<String>,
    remote_url: Option<String>,
    remote_ssh_user: Option<String>,
    remote_dest: Option<String>,
}

fn raw(r: &Row<'_>) -> rusqlite::Result<Raw> {
    Ok(Raw {
        project_id: r.get(0)?,
        name: r.get(1)?,
        station_id: r.get(2)?,
        path: r.get(3)?,
        forge: r.get(4)?,
        trackers: r.get(5)?,
        prompt_template: r.get(6)?,
        review_prompt: r.get(7)?,
        created_at: r.get(8)?,
        git: r.get(9)?,
        git_revision: r.get(10)?,
        repo: r.get(11)?,
        remote_url: r.get(12)?,
        remote_ssh_user: r.get(13)?,
        remote_dest: r.get(14)?,
    })
}

impl Raw {
    fn parse(self) -> Result<Project> {
        Ok(Project {
            project_id: self.project_id,
            name: self.name,
            station_id: self.station_id,
            path: self.path,
            forge: self
                .forge
                .map(|f| serde_json::from_str::<ForgeBinding>(&f))
                .transpose()?,
            trackers: serde_json::from_str::<Vec<TrackerBinding>>(&self.trackers)?,
            prompt_template: self.prompt_template,
            review_prompt: self.review_prompt,
            created_at: self.created_at,
            // A malformed stored value is a corrupt store, as for `forge`.
            git: serde_json::from_str::<GitSettings>(&self.git)?,
            git_revision: super::to_u64(self.git_revision)?,
            repo: self.repo,
            remote_url: self.remote_url,
            remote_ssh_user: self.remote_ssh_user,
            remote_dest: self.remote_dest,
            // Filled by `projects_git::derive` (FG-5, phase 1: one home).
            homes: Vec::new(),
            slugs: Vec::new(),
        })
    }
}

pub(super) fn find(conn: &Connection, id: &str) -> Result<Option<Project>> {
    let found = conn
        .query_row(
            &format!("SELECT {COLUMNS} FROM projects WHERE project_id = ?1"),
            [id],
            raw,
        )
        .optional()?
        .map(Raw::parse)
        .transpose()?;
    match found {
        Some(project) => {
            let mut one = [project];
            super::projects_git::derive(conn, &mut one)?;
            let [project] = one;
            Ok(Some(project))
        }
        None => Ok(None),
    }
}

/// The projects whose home is on `station`, oldest first.
pub(super) fn of_station(conn: &Connection, station: &str) -> Result<Vec<Project>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLUMNS} FROM projects WHERE station_id = ?1 ORDER BY created_at, project_id"
    ))?;
    let rows = stmt.query_map([station], raw)?;
    let mut projects = rows.map(|r| r?.parse()).collect::<Result<Vec<_>>>()?;
    super::projects_git::derive(conn, &mut projects)?;
    Ok(projects)
}

/// The text of an optional prompt: trimmed, `None` when empty.
fn prompt(text: &Option<String>) -> Option<String> {
    text.as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_owned)
}

impl DeckStore {
    /// Every project, oldest first.
    ///
    /// # Errors
    /// A database or stored-JSON error.
    pub async fn projects(&self) -> Result<Vec<Project>> {
        call(&self.inner.db, |conn| {
            let mut stmt = conn.prepare(&format!(
                "SELECT {COLUMNS} FROM projects ORDER BY created_at, project_id"
            ))?;
            let rows = stmt.query_map([], raw)?;
            let mut projects = rows.map(|r| r?.parse()).collect::<Result<Vec<_>>>()?;
            super::projects_git::derive(conn, &mut projects)?;
            Ok(projects)
        })
        .await
    }

    /// One project.
    ///
    /// # Errors
    /// A database or stored-JSON error.
    pub async fn project(&self, id: String) -> Result<Option<Project>> {
        call(&self.inner.db, move |conn| find(conn, &id)).await
    }

    /// Stores a new project of `station_id` (the station that `input.station`
    /// named, resolved by the caller). The caller checked `input`.
    ///
    /// # Errors
    /// A database error.
    pub async fn create_project(&self, station_id: String, input: ProjectInput) -> Result<Project> {
        let id: String = new_id(self.inner.ids.as_ref());
        let now = self.now_ms();
        let out = call(&self.inner.db, move |conn| {
            conn.execute(
                &format!(
                    "INSERT INTO projects ({INSERT_COLUMNS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)"
                ),
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
            find(conn, &id)?.ok_or_else(|| DeckError::Corrupt("a new project is missing".into()))
        })
        .await;
        // A card's automatic project depends on the projects (DL-176).
        self.board_changed();
        out
    }

    /// Replaces every field of a project but its id and creation time.
    /// `None` when no project has `id`.
    ///
    /// # Errors
    /// A database error.
    pub async fn update_project(
        &self,
        id: String,
        station_id: String,
        input: ProjectInput,
    ) -> Result<Option<Project>> {
        let out = call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            let old: Option<(String, String)> = tx
                .query_row(
                    "SELECT station_id, path FROM projects WHERE project_id = ?1",
                    [&id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            let Some((old_station, old_path)) = old else {
                return Ok(None);
            };
            // A new station or folder is a new home: the Git revision grows
            // and no stored check of the old one applies (FG-5).
            let moved = old_station != station_id || old_path != input.path;
            tx.execute(
                "UPDATE projects SET name = ?2, station_id = ?3, path = ?4, forge = ?5,
                     trackers = ?6, prompt_template = ?7, review_prompt = ?8,
                     git_revision = git_revision + ?9
                 WHERE project_id = ?1",
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
                    i64::from(moved),
                ],
            )?;
            if moved {
                super::git_snapshots::clear_checks(&tx, &id)?;
            }
            let found = find(&tx, &id)?;
            tx.commit()?;
            Ok(found)
        })
        .await;
        self.board_changed();
        out
    }

    /// Deletes a project; `false` when no project has `id`.
    ///
    /// # Errors
    /// A database error.
    pub async fn delete_project(&self, id: String) -> Result<bool> {
        let out = call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            let deleted = tx.execute("DELETE FROM projects WHERE project_id = ?1", [&id])? > 0;
            super::git_snapshots::clear_checks(&tx, &id)?;
            tx.commit()?;
            Ok(deleted)
        })
        .await;
        self.board_changed();
        out
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use fleetdeck_proto::integrations::{ForgeKind, TrackerKind};
    use fleetdeck_testkit::Det;

    use super::*;

    async fn store() -> DeckStore {
        let det = Det::new();
        DeckStore::open_in_memory(det.clock(), det.ids())
            .await
            .unwrap()
    }

    fn input(name: &str) -> ProjectInput {
        ProjectInput {
            name: format!("  {name} "),
            station: "laptop".into(),
            path: "/work/app".into(),
            forge: Some(ForgeBinding {
                kind: ForgeKind::Github,
                host: "github.com".into(),
                repo: "acme/app".into(),
            }),
            trackers: vec![TrackerBinding {
                tracker: "acme".into(),
                kind: TrackerKind::Jira,
                key: "PROJ".into(),
            }],
            prompt_template: Some("  Do it.  ".into()),
            review_prompt: Some("   ".into()),
        }
    }

    #[tokio::test]
    async fn a_project_round_trips_with_its_bindings() {
        let s = store().await;
        assert!(s.projects().await.unwrap().is_empty());
        let made = s.create_project("st-1".into(), input("App")).await.unwrap();
        assert_eq!(made.name, "App");
        assert_eq!(made.station_id, "st-1");
        assert_eq!(made.prompt_template.as_deref(), Some("Do it."));
        assert_eq!(made.review_prompt, None, "an empty prompt is no prompt");
        assert_eq!(made.trackers[0].key, "PROJ");
        assert_eq!(made.forge.as_ref().unwrap().repo, "acme/app");
        assert_eq!(
            s.project(made.project_id.clone()).await.unwrap(),
            Some(made.clone())
        );
        assert_eq!(s.projects().await.unwrap(), vec![made]);
    }

    #[tokio::test]
    async fn an_update_replaces_the_fields_and_keeps_the_id_and_time() {
        let s = store().await;
        let made = s.create_project("st-1".into(), input("App")).await.unwrap();
        let mut change = input("Renamed");
        change.forge = None;
        change.trackers.clear();
        let updated = s
            .update_project(made.project_id.clone(), "st-2".into(), change)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(updated.project_id, made.project_id);
        assert_eq!(updated.created_at, made.created_at);
        assert_eq!(
            (updated.name.as_str(), updated.station_id.as_str()),
            ("Renamed", "st-2")
        );
        assert!(updated.forge.is_none() && updated.trackers.is_empty());
        assert_eq!(
            s.update_project("nope".into(), "st".into(), input("x"))
                .await
                .unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn a_delete_removes_one_project() {
        let s = store().await;
        let a = s.create_project("st-1".into(), input("A")).await.unwrap();
        let b = s.create_project("st-1".into(), input("B")).await.unwrap();
        assert!(s.delete_project(a.project_id.clone()).await.unwrap());
        assert!(!s.delete_project(a.project_id).await.unwrap());
        assert_eq!(s.projects().await.unwrap(), vec![b]);
    }
}
