#![allow(clippy::unwrap_used, clippy::expect_used)]

//! Merge into project (Fleet Sync §2): the checks in order and the changes.

use fleetdeck_proto::fleet_git::{GitHomeState, GitSettings, GitSettingsPut};
use fleetdeck_proto::integrations::{ForgeBinding, ForgeKind, Project, ProjectInput};
use fleetdeck_testkit::Det;

use super::super::DeckStore;

async fn store() -> DeckStore {
    let det = Det::new();
    DeckStore::open_in_memory(det.clock(), det.ids())
        .await
        .unwrap()
}

fn forge(repo: &str) -> ForgeBinding {
    ForgeBinding {
        kind: ForgeKind::Github,
        host: "github.com".into(),
        repo: repo.into(),
    }
}

fn input(name: &str, path: &str, forge: Option<ForgeBinding>) -> ProjectInput {
    ProjectInput {
        name: name.into(),
        station: "x".into(),
        path: path.into(),
        forge,
        trackers: vec![],
        prompt_template: None,
        review_prompt: None,
    }
}

/// A project on `station` with the forge `lacion/fleet-deck`.
async fn app(s: &DeckStore, name: &str, station: &str, path: &str) -> Project {
    s.create_project(
        station.into(),
        input(name, path, Some(forge("lacion/fleet-deck"))),
    )
    .await
    .unwrap()
}

async fn exec(s: &DeckStore, sql: &'static str, args: Vec<String>) {
    s.db()
        .call(move |conn| {
            conn.execute(sql, rusqlite::params_from_iter(args))
                .map(|_| ())
                .map_err(Into::into)
        })
        .await
        .unwrap();
}

async fn count(s: &DeckStore, sql: &'static str, id: &str) -> i64 {
    let id = id.to_owned();
    s.db()
        .call(move |conn| conn.query_row(sql, [id], |r| r.get(0)).map_err(Into::into))
        .await
        .unwrap()
}

/// A second home of `id` on `station` in state `state`.
async fn add_home(s: &DeckStore, id: &str, station: &str, position: i64, state: &str) {
    let (id, station, state) = (id.to_owned(), station.to_owned(), state.to_owned());
    s.db()
        .call(move |conn| {
            conn.execute(
                "INSERT INTO project_homes (project_id, station_id, generation, position, state,
                     path, canonical_path, created_at, updated_at)
                 VALUES (?1, ?2, 1, ?3, ?4, ?5, ?6, 1, 1)",
                rusqlite::params![
                    id,
                    station,
                    position,
                    state,
                    (state != "cloning").then(|| format!("/srv/{station}")),
                    (state != "cloning").then(|| format!("/canon/{station}"))
                ],
            )
            .map(|_| ())
            .map_err(Into::into)
        })
        .await
        .unwrap();
}

async fn save_git(s: &DeckStore, id: &str, sync: bool) {
    let p = s.project(id.to_owned()).await.unwrap().unwrap();
    let put = GitSettingsPut {
        settings: GitSettings {
            sync,
            ..GitSettings::default()
        },
        revision: p.git_revision,
        remote_url: None,
    };
    s.set_project_git(id.to_owned(), put).await.unwrap();
}

/// The refusal `(code, message)` of a merge.
async fn refused(s: &DeckStore, target: &str, from: &str) -> (&'static str, String) {
    let r = s
        .project_merge(target.to_owned(), from.to_owned())
        .await
        .unwrap()
        .unwrap_err();
    (r.code, r.message)
}

#[tokio::test]
async fn an_unknown_project_is_refused_on_either_side() {
    let s = store().await;
    let a = app(&s, "A", "st-1", "/work/a").await;
    let expect = ("unknown_project", "Unknown project.".to_owned());
    assert_eq!(refused(&s, &a.project_id, "nope").await, expect);
    assert_eq!(refused(&s, "nope", &a.project_id).await, expect);
    assert_eq!(refused(&s, "nope", "nope").await, expect);
}

#[tokio::test]
async fn a_project_cannot_merge_into_itself() {
    let s = store().await;
    let a = app(&s, "A", "st-1", "/work/a").await;
    assert_eq!(
        refused(&s, &a.project_id, &a.project_id).await,
        ("invalid_body", "Choose another project.".to_owned())
    );
}

#[tokio::test]
async fn both_projects_need_a_remote() {
    let s = store().await;
    let a = app(&s, "A", "st-1", "/work/a").await;
    let bare = s
        .create_project("st-2".into(), input("Bare", "/work/bare", None))
        .await
        .unwrap();
    let expect = (
        "needs_remote",
        "Set the forge or the remote URL of both projects first.".to_owned(),
    );
    assert_eq!(refused(&s, &a.project_id, &bare.project_id).await, expect);
    assert_eq!(refused(&s, &bare.project_id, &a.project_id).await, expect);
}

#[tokio::test]
async fn another_repository_is_refused_with_both_names() {
    let s = store().await;
    let a = app(&s, "A", "st-1", "/work/a").await;
    let other = s
        .create_project(
            "st-2".into(),
            input("Other", "/work/other", Some(forge("acme/tools"))),
        )
        .await
        .unwrap();
    assert_eq!(
        refused(&s, &a.project_id, &other.project_id).await,
        (
            "not_same_repo",
            "Other uses github.com/acme/tools, not github.com/lacion/fleet-deck.".to_owned()
        )
    );
}

#[tokio::test]
async fn sync_on_in_the_source_is_refused_before_the_revision_check() {
    let s = store().await;
    let a = app(&s, "A", "st-1", "/work/a").await;
    let b = app(&s, "B", "st-2", "/work/b").await;
    save_git(&s, &b.project_id, true).await;
    assert_eq!(
        refused(&s, &a.project_id, &b.project_id).await,
        ("merge_sync_on", "Turn sync off in B first.".to_owned())
    );
}

#[tokio::test]
async fn a_source_that_changed_its_git_settings_is_refused() {
    let s = store().await;
    let a = app(&s, "A", "st-1", "/work/a").await;
    let b = app(&s, "B", "st-2", "/work/b").await;
    save_git(&s, &b.project_id, false).await;
    assert_eq!(
        refused(&s, &a.project_id, &b.project_id).await,
        (
            "merge_sync_used",
            "B has Fleet Git settings. Merge works only for a project that never changed them."
                .to_owned()
        )
    );
}

#[tokio::test]
async fn a_station_with_a_home_of_both_is_refused() {
    let s = store().await;
    let a = app(&s, "A", "st-1", "/work/a").await;
    let b = app(&s, "B", "st-2", "/work/b").await;
    add_home(&s, &b.project_id, "st-1", 1, "ready").await;
    assert_eq!(
        refused(&s, &a.project_id, &b.project_id).await,
        (
            "home_exists",
            "Both projects have a home on st-1. Remove one of the homes first.".to_owned()
        )
    );
}

#[tokio::test]
async fn a_home_that_is_not_ready_is_refused() {
    let s = store().await;
    let a = app(&s, "A", "st-1", "/work/a").await;
    let b = app(&s, "B", "st-2", "/work/b").await;
    add_home(&s, &b.project_id, "st-3", 1, "cloning").await;
    assert_eq!(
        refused(&s, &a.project_id, &b.project_id).await,
        ("home_busy", "A home of B is not ready.".to_owned())
    );
}

#[tokio::test]
async fn a_land_that_is_not_finished_is_refused() {
    let s = store().await;
    let a = app(&s, "A", "st-1", "/work/a").await;
    let b = app(&s, "B", "st-2", "/work/b").await;
    for state in ["queued", "running"] {
        exec(
            &s,
            "DELETE FROM land_queue WHERE project_id = ?1",
            vec![b.project_id.clone()],
        )
        .await;
        let sql: &'static str = if state == "queued" {
            "INSERT INTO land_queue (land_id, command_id, requested_by, request_command_id,
                 project_id, lock_key, station_id, session_id, state, created_at)
             VALUES ('l1', 'c1', 'owner', 'r1', ?1, 'k', 'st-2', 's1', 'queued', 1)"
        } else {
            "INSERT INTO land_queue (land_id, command_id, requested_by, request_command_id,
                 project_id, lock_key, station_id, session_id, state, created_at)
             VALUES ('l1', 'c1', 'owner', 'r1', ?1, 'k', 'st-2', 's1', 'running', 1)"
        };
        exec(&s, sql, vec![b.project_id.clone()]).await;
        assert_eq!(
            refused(&s, &a.project_id, &b.project_id).await,
            ("home_busy", "A land of B is not finished.".to_owned()),
            "{state}"
        );
    }
    // A land that ended does not block.
    exec(
        &s,
        "UPDATE land_queue SET state = 'landed' WHERE project_id = ?1",
        vec![b.project_id.clone()],
    )
    .await;
    assert!(
        s.project_merge(a.project_id, b.project_id)
            .await
            .unwrap()
            .is_ok()
    );
}

#[tokio::test]
async fn a_git_name_that_another_station_holds_is_refused() {
    let s = store().await;
    let a = app(&s, "A", "st-1", "/work/a").await;
    let b = app(&s, "B", "st-2", "/work/b").await;
    exec(
        &s,
        "INSERT INTO slug_reservations (repo, slug, station_id, first_used_at)
         VALUES ('github.com/lacion/fleet-deck', 'st-2', 'other-station', 1)",
        vec![],
    )
    .await;
    assert_eq!(
        refused(&s, &a.project_id, &b.project_id).await,
        (
            "slug_taken",
            "The Git name st-2 is reserved for station other-station in github.com/lacion/fleet-deck. Rename this station."
                .to_owned()
        )
    );
    // The refusal wrote nothing.
    assert!(s.project(b.project_id).await.unwrap().is_some());
    assert_eq!(
        s.project(a.project_id).await.unwrap().unwrap().homes.len(),
        1
    );
}

#[tokio::test]
async fn the_homes_move_to_the_target_and_the_source_ends() {
    let s = store().await;
    let a = app(&s, "A", "st-1", "/work/a").await;
    let b = app(&s, "B", "st-2", "/work/b").await;
    add_home(&s, &b.project_id, "st-3", 1, "unverified").await;
    // The target had a home on st-3 once: its generation counter goes on.
    exec(
        &s,
        "INSERT INTO home_generations (project_id, station_id, last_generation)
         VALUES (?1, 'st-3', 4)",
        vec![a.project_id.clone()],
    )
    .await;
    exec(
        &s,
        "INSERT INTO project_sessions (station_id, session_id, project_id, assigned_at)
         VALUES ('st-2', 'sess-1', ?1, 1)",
        vec![b.project_id.clone()],
    )
    .await;
    exec(
        &s,
        "INSERT INTO schedules (schedule_id, name, enabled, cron, timezone, station_id,
             project_id, path, provider_instance, prompt, reuse_session, created_at, armed_at)
         VALUES ('sch-1', 'n', 1, '* * * * *', 'UTC', 'st-2', ?1, '/work/b', 'claude', 'p', 0, 1, 1)",
        vec![b.project_id.clone()],
    )
    .await;
    exec(
        &s,
        "INSERT INTO radar_pairs (project_id, a_station, a_session, b_station, b_session,
             conflict, files, checked_at)
         VALUES (?1, 'st-2', 'x', 'st-3', 'y', 0, '[]', 1)",
        vec![b.project_id.clone()],
    )
    .await;

    let (merged, moved) = s
        .project_merge(a.project_id.clone(), b.project_id.clone())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(moved, 2);
    assert_eq!(merged.project_id, a.project_id);
    assert_eq!(merged.name, "A");
    let homes: Vec<_> = merged
        .homes
        .iter()
        .map(|h| {
            (
                h.station_id.as_str(),
                h.position,
                h.generation,
                h.state,
                h.path.as_deref(),
                h.sync_owner,
            )
        })
        .collect();
    assert_eq!(
        homes,
        vec![
            ("st-1", 0, 1, GitHomeState::Ready, Some("/work/a"), false),
            ("st-2", 1, 1, GitHomeState::Ready, Some("/work/b"), false),
            ("st-3", 2, 5, GitHomeState::Ready, Some("/srv/st-3"), false),
        ]
    );
    let canonical: Vec<(String, String)> = s
        .db()
        .call({
            let id = a.project_id.clone();
            move |conn| {
                let mut stmt = conn.prepare(
                    "SELECT station_id, canonical_path FROM project_homes
                     WHERE project_id = ?1 AND position > 0 ORDER BY position",
                )?;
                let rows = stmt
                    .query_map([id], |r| Ok((r.get(0)?, r.get(1)?)))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                Ok(rows)
            }
        })
        .await
        .unwrap();
    assert_eq!(
        canonical,
        vec![
            ("st-2".to_owned(), "/work/b".to_owned()),
            ("st-3".to_owned(), "/canon/st-3".to_owned())
        ]
    );
    // The source ended with its generations and its radar pairs.
    assert!(s.project(b.project_id.clone()).await.unwrap().is_none());
    let b_id = &b.project_id;
    for sql in [
        "SELECT COUNT(*) FROM home_generations WHERE project_id = ?1",
        "SELECT COUNT(*) FROM radar_pairs WHERE project_id = ?1",
        "SELECT COUNT(*) FROM project_homes WHERE project_id = ?1",
        "SELECT COUNT(*) FROM project_sessions WHERE project_id = ?1",
        "SELECT COUNT(*) FROM schedules WHERE project_id = ?1",
    ] {
        assert_eq!(count(&s, sql, b_id).await, 0, "{sql}");
    }
    // The sessions and the schedules moved to the target.
    let a_id = &a.project_id;
    assert_eq!(
        count(
            &s,
            "SELECT COUNT(*) FROM project_sessions WHERE project_id = ?1",
            a_id
        )
        .await,
        1
    );
    assert_eq!(
        count(
            &s,
            "SELECT COUNT(*) FROM schedules WHERE project_id = ?1",
            a_id
        )
        .await,
        1
    );
    // The reserved Git names stay.
    assert_eq!(
        count(
            &s,
            "SELECT COUNT(*) FROM slug_reservations WHERE repo = 'github.com/lacion/fleet-deck' AND ?1 <> ''",
            a_id
        )
        .await,
        2
    );
}

#[tokio::test]
async fn a_target_with_sync_on_waits_for_the_check_of_each_moved_home() {
    let s = store().await;
    let a = app(&s, "A", "st-1", "/work/a").await;
    let b = app(&s, "B", "st-2", "/work/b").await;
    save_git(&s, &a.project_id, true).await;
    let (merged, moved) = s
        .project_merge(a.project_id, b.project_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(moved, 1);
    let h = merged
        .homes
        .iter()
        .find(|h| h.station_id.as_str() == "st-2")
        .unwrap();
    assert_eq!(h.state, GitHomeState::Unverified);
    assert_eq!(h.code.as_deref(), Some("rebind_pending"));
    assert_eq!(
        h.message.as_deref(),
        Some("Waiting for the station to check the new remote.")
    );
    assert!(!h.sync_owner);
}

#[tokio::test]
async fn a_target_with_sync_off_gets_ready_homes() {
    let s = store().await;
    let a = app(&s, "A", "st-1", "/work/a").await;
    let b = app(&s, "B", "st-2", "/work/b").await;
    let (merged, _) = s
        .project_merge(a.project_id, b.project_id)
        .await
        .unwrap()
        .unwrap();
    let h = merged
        .homes
        .iter()
        .find(|h| h.station_id.as_str() == "st-2")
        .unwrap();
    assert_eq!(
        (h.state, h.code.clone(), h.message.clone()),
        (GitHomeState::Ready, None, None)
    );
}

#[tokio::test]
async fn two_projects_with_a_forge_and_no_remote_merge_after_the_derivation() {
    let s = store().await;
    // A project that was saved before the derivation existed: a forge, no remote.
    let a = s
        .create_project("st-1".into(), input("A", "/work/a", None))
        .await
        .unwrap();
    let b = s
        .create_project("st-2".into(), input("B", "/work/b", None))
        .await
        .unwrap();
    let json = serde_json::to_string(&forge("lacion/fleet-deck")).unwrap();
    for id in [&a.project_id, &b.project_id] {
        let json = json.clone();
        let id = id.clone();
        s.db()
            .call(move |conn| {
                conn.execute(
                    "UPDATE projects SET forge = ?2 WHERE project_id = ?1",
                    [id, json],
                )
                .map(|_| ())
                .map_err(Into::into)
            })
            .await
            .unwrap();
    }
    assert_eq!(
        s.project(a.project_id.clone())
            .await
            .unwrap()
            .unwrap()
            .remote_url,
        None
    );
    let (merged, moved) = s
        .project_merge(a.project_id, b.project_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(moved, 1);
    assert_eq!(merged.homes.len(), 2);
    assert_eq!(
        merged.remote_url.as_deref(),
        Some("https://github.com/lacion/fleet-deck.git")
    );
    assert_eq!(
        merged.remote_dest.as_deref(),
        Some("https://github.com:443/lacion/fleet-deck")
    );
}
