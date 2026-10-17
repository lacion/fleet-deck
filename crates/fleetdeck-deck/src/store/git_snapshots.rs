//! The Fleet Git snapshot state of each station (design §3.3, §8.2 phase 1).
//!
//! `git_snapshots` keeps, for each station, the revision and the digest of the
//! newest snapshot that the deck prepared, the acknowledged revision and the
//! last list of `HomeCheck` answers. The snapshot itself is built from the
//! projects whose home is on the station; it is never stored.
//!
//! - The digest is the SHA-256 of the canonical JSON of the snapshot with
//!   `revision` set to 0 (so only a real change gives a new revision).
//! - The revision is `max(stored + 1, now in unix ms)`: it grows also after a
//!   restore of a deck backup.
//! - A trigger whose digest equals the acknowledged digest sends nothing.

use fleetdeck_proto::StationId;
use fleetdeck_proto::fleet_git::{
    GitHomeState, GitProjectView, GitSnapshot, HomeCheck, RemoteIdentity, SnapshotAck, StationHome,
};
use rusqlite::{Connection, OptionalExtension, params};

use super::project_homes::{apply_check, home_on};
use super::projects_git::clone_url;
use super::{DeckStore, Result, call, to_i64, to_u64};

/// The checks that `station` answered last.
pub(super) fn checks_of(conn: &Connection, station: &str) -> Result<Vec<HomeCheck>> {
    let json: Option<String> = conn
        .query_row(
            "SELECT checks FROM git_snapshots WHERE station_id = ?1",
            [station],
            |r| r.get(0),
        )
        .optional()?;
    json.map_or_else(
        || Ok(Vec::new()),
        |json| Ok(serde_json::from_str::<Vec<HomeCheck>>(&json)?),
    )
}

/// Drops the stored check of `project` from every station: its settings,
/// folder or station changed, so the home is unverified until the station
/// answers the next snapshot.
pub(super) fn clear_checks(conn: &Connection, project: &str) -> Result<()> {
    let mut stmt = conn.prepare("SELECT station_id, checks FROM git_snapshots")?;
    let rows = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (station, json) in rows {
        let mut checks: Vec<HomeCheck> = serde_json::from_str(&json)?;
        let before = checks.len();
        checks.retain(|c| c.project_id != project);
        if checks.len() != before {
            conn.execute(
                "UPDATE git_snapshots SET checks = ?2 WHERE station_id = ?1",
                params![station, serde_json::to_string(&checks)?],
            )?;
        }
    }
    Ok(())
}

/// The snapshot of `station` with `revision` 0 (design §3.3, decision B13):
/// `homes` lists every home of the station, sync on or off, in every state,
/// with its real generation (a home without a path sends its reserved path);
/// `projects` lists the sync projects whose home here is `ready` or
/// `unverified`.
fn build(conn: &Connection, station: &str) -> Result<GitSnapshot> {
    let mut snapshot = GitSnapshot {
        revision: 0,
        projects: Vec::new(),
        homes: Vec::new(),
    };
    for p in super::projects::of_station(conn, station)? {
        let Some(home) = home_on(conn, &p.project_id, station)? else {
            continue;
        };
        snapshot.homes.push(StationHome {
            project_id: p.project_id.clone(),
            generation: home.generation,
            path: home
                .path
                .clone()
                .or_else(|| home.reserved_path.clone())
                .unwrap_or_default(),
            sync: p.git.sync,
        });
        if !p.git.sync || !matches!(home.state, GitHomeState::Ready | GitHomeState::Unverified) {
            continue;
        }
        let (Some(repo), Some(dest), Some(url)) = (&p.repo, &p.remote_dest, &p.remote_url) else {
            // A sync project without its remote cannot be saved; a stored row
            // like it is left out rather than sent half-made.
            continue;
        };
        snapshot.projects.push(GitProjectView {
            project_id: p.project_id.clone(),
            project_name: p.name.clone(),
            settings_revision: p.git_revision,
            settings: p.git.clone(),
            remote: RemoteIdentity {
                repo: repo.clone(),
                dest: dest.clone(),
            },
            forge: p.forge.clone(),
            clone_url: clone_url(url, p.remote_ssh_user.as_deref()),
            home_generation: home.generation,
            home_path: home.folder().unwrap_or_default().to_owned(),
            radar_home: p
                .git
                .radar_home
                .as_ref()
                .is_some_and(|s| s.as_str() == station),
        });
    }
    Ok(snapshot)
}

/// The SHA-256 of the canonical JSON of `snapshot`, in hex.
fn digest_of(snapshot: &GitSnapshot) -> Result<String> {
    let bytes = serde_json::to_vec(snapshot)?;
    let hash = ring::digest::digest(&ring::digest::SHA256, &bytes);
    Ok(hash.as_ref().iter().map(|b| format!("{b:02x}")).collect())
}

impl DeckStore {
    /// The snapshot to send to `station`, with its revision, or `None` when
    /// the station acknowledged this very state already and `force` is
    /// false (a new link sends it again). A state that the
    /// deck prepared and did not get acknowledged keeps its revision (a
    /// resend is safe); a changed state gets `max(stored + 1, now in ms)`.
    ///
    /// # Errors
    /// A database or stored-JSON error.
    pub async fn git_snapshot_next(
        &self,
        station: &StationId,
        force: bool,
    ) -> Result<Option<GitSnapshot>> {
        let station = station.0.clone();
        let now = self.now_ms();
        call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            let mut snapshot = build(&tx, &station)?;
            let digest = digest_of(&snapshot)?;
            let row: Option<(i64, String, Option<i64>)> = tx
                .query_row(
                    "SELECT revision, digest, acked_revision FROM git_snapshots
                     WHERE station_id = ?1",
                    [&station],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .optional()?;
            let revision = match row {
                Some((revision, stored, acked)) if stored == digest => {
                    if acked == Some(revision) && !force {
                        return Ok(None);
                    }
                    to_u64(revision)?
                }
                Some((revision, ..)) => {
                    let next = to_u64(revision)?.saturating_add(1).max(to_u64(now)?);
                    tx.execute(
                        "UPDATE git_snapshots SET revision = ?2, digest = ?3, sent_at = NULL
                         WHERE station_id = ?1",
                        params![station, to_i64(next)?, digest],
                    )?;
                    next
                }
                None => {
                    let next = to_u64(now)?.max(1);
                    tx.execute(
                        "INSERT INTO git_snapshots (station_id, revision, digest) VALUES (?1, ?2, ?3)",
                        params![station, to_i64(next)?, digest],
                    )?;
                    next
                }
            };
            tx.commit()?;
            snapshot.revision = revision;
            Ok(Some(snapshot))
        })
        .await
    }

    /// Records the answer of `station` to `sent`. The deck keeps a check only
    /// when its project still has the settings revision and the station that
    /// the snapshot carried: a save or a move while the call was on its way
    /// leaves the home unverified until the next answer.
    ///
    /// # Errors
    /// A database or stored-JSON error.
    pub async fn git_snapshot_acked(
        &self,
        station: &StationId,
        sent: GitSnapshot,
        ack: SnapshotAck,
    ) -> Result<()> {
        let station = station.0.clone();
        let now = self.now_ms();
        let out = call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            let mut kept = Vec::new();
            for check in ack.checks {
                let Some(view) = sent
                    .projects
                    .iter()
                    .find(|v| v.project_id == check.project_id)
                else {
                    continue;
                };
                let current: Option<i64> = tx
                    .query_row(
                        "SELECT git_revision FROM projects WHERE project_id = ?1",
                        [&check.project_id],
                        |r| r.get(0),
                    )
                    .optional()?;
                let home = home_on(&tx, &check.project_id, &station)?;
                let same = current
                    .is_some_and(|revision| u64::try_from(revision) == Ok(view.settings_revision))
                    && home.is_some_and(|h| {
                        h.generation == view.home_generation
                            && check.generation == view.home_generation
                    });
                if same {
                    apply_check(&tx, &station, &check, now)?;
                    kept.push(check);
                }
            }
            tx.execute(
                "UPDATE git_snapshots SET acked_revision = revision, sent_at = ?3, checks = ?4
                 WHERE station_id = ?1 AND revision = ?2",
                params![
                    station,
                    to_i64(sent.revision)?,
                    now,
                    serde_json::to_string(&kept)?
                ],
            )?;
            tx.commit()?;
            Ok(())
        })
        .await;
        self.board_changed();
        out
    }

    /// The station answered `stored: false` with its own `revision`: the next
    /// snapshot of this station gets a revision above both.
    ///
    /// # Errors
    /// A database error.
    pub async fn git_snapshot_adopt(&self, station: &StationId, theirs: u64) -> Result<()> {
        let station = station.0.clone();
        call(&self.inner.db, move |conn| {
            conn.execute(
                "UPDATE git_snapshots SET revision = MAX(revision, ?2), digest = ''
                 WHERE station_id = ?1",
                params![station, to_i64(theirs)?],
            )?;
            Ok(())
        })
        .await
    }

    /// The stored `HomeCheck` list of `station` (tests and the board read it).
    ///
    /// # Errors
    /// A database or stored-JSON error.
    pub async fn git_checks(&self, station: &StationId) -> Result<Vec<HomeCheck>> {
        let station = station.0.clone();
        call(&self.inner.db, move |conn| checks_of(conn, &station)).await
    }

    /// The `(revision, acked_revision)` of `station` (tests).
    ///
    /// # Errors
    /// A database error.
    pub async fn git_snapshot_state(
        &self,
        station: &StationId,
    ) -> Result<Option<(u64, Option<u64>)>> {
        let station = station.0.clone();
        call(&self.inner.db, move |conn| {
            let row: Option<(i64, Option<i64>)> = conn
                .query_row(
                    "SELECT revision, acked_revision FROM git_snapshots WHERE station_id = ?1",
                    [&station],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            row.map(|(r, a)| Ok((to_u64(r)?, a.map(to_u64).transpose()?)))
                .transpose()
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use fleetdeck_proto::fleet_git::{GitSettings, GitSettingsPut, HomeCheckState};
    use fleetdeck_proto::integrations::ProjectInput;
    use fleetdeck_testkit::Det;

    use super::*;

    async fn store() -> DeckStore {
        let det = Det::new();
        DeckStore::open_in_memory(det.clock(), det.ids())
            .await
            .unwrap()
    }

    fn input(path: &str) -> ProjectInput {
        ProjectInput {
            name: "App".into(),
            station: "x".into(),
            path: path.into(),
            forge: None,
            trackers: vec![],
            prompt_template: None,
            review_prompt: None,
        }
    }

    async fn sync_project(s: &DeckStore) -> String {
        let made = s
            .create_project("st-1".into(), input("/work/app"))
            .await
            .unwrap();
        save(s, &made.project_id, 0, |g| g.sync = true).await;
        made.project_id
    }

    async fn save(s: &DeckStore, id: &str, revision: u64, f: impl FnOnce(&mut GitSettings)) {
        let mut settings = GitSettings::default();
        f(&mut settings);
        let put = GitSettingsPut {
            settings,
            revision,
            remote_url: (revision == 0).then(|| "https://github.com/acme/app.git".to_owned()),
        };
        let out = s.set_project_git(id.to_owned(), put).await.unwrap();
        assert!(
            matches!(out, crate::store::projects_git::GitWrite::Saved(_)),
            "{out:?}"
        );
    }

    fn ack(sent: &GitSnapshot, state: HomeCheckState, canonical: &str) -> SnapshotAck {
        SnapshotAck {
            stored: true,
            revision: sent.revision,
            checks: sent
                .projects
                .iter()
                .map(|p| HomeCheck {
                    project_id: p.project_id.clone(),
                    generation: p.home_generation,
                    state,
                    canonical_path: Some(canonical.into()),
                    seen: None,
                    code: None,
                })
                .collect(),
        }
    }

    #[tokio::test]
    async fn the_revision_grows_with_a_change_and_an_acked_state_sends_nothing() {
        let s = store().await;
        let id = sync_project(&s).await;
        let st = StationId::from("st-1".to_owned());
        let first = s.git_snapshot_next(&st, false).await.unwrap().unwrap();
        assert_eq!(first.projects.len(), 1);
        assert_eq!(first.projects[0].home_path, "/work/app");
        // Not acknowledged yet: the same state keeps its revision.
        let again = s.git_snapshot_next(&st, false).await.unwrap().unwrap();
        assert_eq!(again.revision, first.revision);
        s.git_snapshot_acked(
            &st,
            again.clone(),
            ack(&again, HomeCheckState::Verified, "/work/app"),
        )
        .await
        .unwrap();
        // Acknowledged: a change trigger sends nothing, a new link does.
        assert!(s.git_snapshot_next(&st, false).await.unwrap().is_none());
        let forced = s.git_snapshot_next(&st, true).await.unwrap().unwrap();
        assert_eq!(forced.revision, first.revision);
        // The canonical path of a verified check is the home path of the
        // next snapshot (one more snapshot, then the state is stable).
        s.git_snapshot_acked(
            &st,
            forced.clone(),
            ack(&forced, HomeCheckState::Verified, "/real/app"),
        )
        .await
        .unwrap();
        let real = s.git_snapshot_next(&st, false).await.unwrap().unwrap();
        assert_eq!(real.projects[0].home_path, "/real/app");
        assert!(real.revision > forced.revision);
        s.git_snapshot_acked(
            &st,
            real.clone(),
            ack(&real, HomeCheckState::Verified, "/real/app"),
        )
        .await
        .unwrap();
        assert!(s.git_snapshot_next(&st, false).await.unwrap().is_none());
        // A change gives a higher revision.
        save(&s, &id, 1, |g| {
            g.sync = true;
            g.cleanup_after_hours = 5;
        })
        .await;
        let next = s.git_snapshot_next(&st, false).await.unwrap().unwrap();
        assert!(next.revision > real.revision);
        assert_eq!(next.projects[0].settings_revision, 2);
        // A station that is ahead: the next revision is above both.
        s.git_snapshot_adopt(&st, next.revision + 1_000)
            .await
            .unwrap();
        let after = s.git_snapshot_next(&st, false).await.unwrap().unwrap();
        assert!(after.revision > next.revision + 1_000);
    }

    #[tokio::test]
    async fn a_check_of_an_old_settings_revision_is_not_kept() {
        let s = store().await;
        let id = sync_project(&s).await;
        let st = StationId::from("st-1".to_owned());
        let sent = s.git_snapshot_next(&st, false).await.unwrap().unwrap();
        // The owner saves while the snapshot is on its way.
        save(&s, &id, 1, |g| g.sync = true).await;
        s.git_snapshot_acked(
            &st,
            sent.clone(),
            ack(&sent, HomeCheckState::Verified, "/work/app"),
        )
        .await
        .unwrap();
        assert!(s.git_checks(&st).await.unwrap().is_empty());
        let p = s.project(id.clone()).await.unwrap().unwrap();
        assert_eq!(
            serde_json::to_value(p.homes[0].state).unwrap(),
            "unverified"
        );
        // A project that moved to another station keeps no check either.
        let now = s.git_snapshot_next(&st, false).await.unwrap().unwrap();
        s.git_snapshot_acked(
            &st,
            now.clone(),
            ack(&now, HomeCheckState::Verified, "/work/app"),
        )
        .await
        .unwrap();
        assert_eq!(s.git_checks(&st).await.unwrap().len(), 1);
        s.update_project(id, "st-2".into(), input("/work/app2"))
            .await
            .unwrap();
        assert!(s.git_checks(&st).await.unwrap().is_empty());
    }
}
