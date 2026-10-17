#![allow(clippy::unwrap_used, clippy::expect_used)]

//! The write adapter and `repair()` (design §8.2, §3.9 item 6, §8.9).

use fleetdeck_proto::fleet_git::{GitHomeState, GitSettings, GitSettingsPut};
use fleetdeck_proto::integrations::ProjectInput;
use fleetdeck_testkit::Det;

use super::super::DeckError;
use super::super::DeckStore;
use super::super::projects_legacy;

async fn store() -> DeckStore {
    let det = Det::new();
    DeckStore::open_in_memory(det.clock(), det.ids())
        .await
        .unwrap()
}

fn input(name: &str, path: &str) -> ProjectInput {
    ProjectInput {
        name: name.into(),
        station: "x".into(),
        path: path.into(),
        forge: None,
        trackers: vec![],
        prompt_template: None,
        review_prompt: None,
    }
}

async fn exec(s: &DeckStore, sql: &'static str, id: &str) {
    let id = id.to_owned();
    s.db()
        .call(move |conn| conn.execute(sql, [id]).map(|_| ()).map_err(Into::into))
        .await
        .unwrap();
}

/// A second home on `station` in state `state`.
async fn add_home(s: &DeckStore, id: &str, station: &str, position: i64, state: &str) {
    let (id, station, state) = (id.to_owned(), station.to_owned(), state.to_owned());
    s.db()
        .call(move |conn| {
            conn.execute(
                "INSERT INTO project_homes (project_id, station_id, generation, position, state,
                     path, canonical_path, created_at, updated_at)
                 VALUES (?1, ?2, 1, ?3, ?4, ?5, ?5, 1, 1)",
                rusqlite::params![
                    id,
                    station,
                    position,
                    state,
                    (state != "cloning").then(|| format!("/srv/{station}"))
                ],
            )
            .map(|_| ())
            .map_err(Into::into)
        })
        .await
        .unwrap();
}

async fn sync_on(s: &DeckStore, id: &str) {
    let p = s.project(id.to_owned()).await.unwrap().unwrap();
    let put = GitSettingsPut {
        settings: GitSettings {
            sync: true,
            ..GitSettings::default()
        },
        revision: p.git_revision,
        remote_url: Some("https://github.com/acme/app.git".into()),
    };
    s.set_project_git(id.to_owned(), put).await.unwrap();
}

#[tokio::test]
async fn a_new_project_gets_its_position_0_home_and_the_copy_follows_it() {
    let s = store().await;
    let p = s
        .create_project("st-1".into(), input("App", "/work/app"))
        .await
        .unwrap();
    assert_eq!(p.homes.len(), 1);
    let h = &p.homes[0];
    assert_eq!(
        (h.station_id.as_str(), h.generation, h.position, h.state),
        ("st-1", 1, 0, GitHomeState::Ready)
    );
    assert_eq!(h.path.as_deref(), Some("/work/app"));
    assert_eq!(
        (p.station_id.as_str(), p.path.as_str()),
        ("st-1", "/work/app")
    );
}

#[tokio::test]
async fn an_update_with_the_same_home_changes_no_home_and_a_new_folder_is_a_new_generation() {
    let s = store().await;
    let p = s
        .create_project("st-1".into(), input("App", "/work/app"))
        .await
        .unwrap();
    let id = p.project_id.clone();
    let same = s
        .update_project(id.clone(), "st-1".into(), input("Renamed", "/work/app"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!((same.git_revision, same.homes[0].generation), (0, 1));
    // A new folder on the same station: generation 2, ready (sync off).
    let moved = s
        .update_project(id.clone(), "st-1".into(), input("Renamed", "/work/other"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(moved.homes.len(), 1);
    assert_eq!(
        (
            moved.homes[0].generation,
            moved.homes[0].state,
            moved.git_revision
        ),
        (2, GitHomeState::Ready, 1)
    );
    assert_eq!(moved.path, "/work/other");
    // Another station: a new pair, generation 1; the old pair keeps its last.
    let away = s
        .update_project(id.clone(), "st-2".into(), input("Renamed", "/work/other"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (away.station_id.as_str(), away.homes[0].generation),
        ("st-2", 1)
    );
    let back = s
        .update_project(id.clone(), "st-1".into(), input("Renamed", "/work/third"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        back.homes[0].generation, 3,
        "a home never reuses a generation"
    );
    // With sync on, the new home is unverified and owns its folder.
    sync_on(&s, &id).await;
    let synced = s
        .update_project(id, "st-1".into(), input("Renamed", "/work/fourth"))
        .await
        .unwrap()
        .unwrap();
    let h = &synced.homes[0];
    assert_eq!(
        (h.state, h.sync_owner, h.code.clone(), h.generation),
        (GitHomeState::Unverified, true, None, 4)
    );
}

#[tokio::test]
async fn a_project_with_two_homes_or_a_busy_home_refuses_a_new_folder() {
    let s = store().await;
    let p = s
        .create_project("st-1".into(), input("App", "/work/app"))
        .await
        .unwrap();
    let id = p.project_id;
    add_home(&s, &id, "st-2", 1, "ready").await;
    let refused = |e: DeckError| match e {
        DeckError::HomeRefused(r) => (r.code, r.message),
        other => panic!("{other}"),
    };
    let e = s
        .update_project(id.clone(), "st-1".into(), input("App", "/work/other"))
        .await
        .unwrap_err();
    assert_eq!(
        refused(e),
        (
            "use_homes",
            "This project has more than one home. Change the homes in the Homes section.".into()
        )
    );
    // The same folder is fine with two homes.
    assert!(
        s.update_project(id.clone(), "st-1".into(), input("Renamed", "/work/app"))
            .await
            .unwrap()
            .is_some()
    );
    // One home, a clone that runs: busy.
    exec(
        &s,
        "DELETE FROM project_homes WHERE project_id = ?1 AND position = 1",
        &id,
    )
    .await;
    exec(
        &s,
        "INSERT INTO home_attempts (attempt_id, request_command_id, project_id, station_id,
             generation, request_digest, request, state, created_at)
         VALUES ('a1', 'c1', ?1, 'st-1', 1, 'd', '{}', 'cloning', 1)",
        &id,
    )
    .await;
    let e = s
        .update_project(id.clone(), "st-1".into(), input("App", "/work/other"))
        .await
        .unwrap_err();
    assert_eq!(
        refused(e),
        ("home_busy", "A clone runs for this home.".into())
    );
    // The folder did not change.
    assert_eq!(s.project(id).await.unwrap().unwrap().path, "/work/app");
}

#[tokio::test]
async fn deleting_a_project_deletes_its_homes_and_generations_and_keeps_the_git_names() {
    let s = store().await;
    let p = s
        .create_project("st-1".into(), input("App", "/work/app"))
        .await
        .unwrap();
    s.db()
        .call(|conn| {
            conn.execute(
                "INSERT INTO slug_reservations (repo, slug, station_id, first_used_at)
                 VALUES ('github.com/acme/app', 'st', 'st-1', 1)",
                [],
            )
            .map(|_| ())
            .map_err(Into::into)
        })
        .await
        .unwrap();
    assert!(s.delete_project(p.project_id).await.unwrap());
    let counts: (i64, i64, i64) = s
        .db()
        .call(|conn| {
            conn.query_row(
                "SELECT (SELECT COUNT(*) FROM project_homes),
                        (SELECT COUNT(*) FROM home_generations),
                        (SELECT COUNT(*) FROM slug_reservations)",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .map_err(Into::into)
        })
        .await
        .unwrap();
    assert_eq!(counts, (0, 0, 1));
}

#[tokio::test]
async fn repair_makes_a_changed_copy_again_from_the_position_0_home() {
    let s = store().await;
    let a = s
        .create_project("st-1".into(), input("A", "/work/a"))
        .await
        .unwrap();
    let b = s
        .create_project("st-1".into(), input("B", "/work/b"))
        .await
        .unwrap();
    exec(
        &s,
        "UPDATE projects SET station_id = 'wrong', path = '/wrong' WHERE project_id = ?1",
        &a.project_id,
    )
    .await;
    let repaired = super::super::call(&s.db(), |conn| projects_legacy::repair(conn))
        .await
        .unwrap();
    assert_eq!(repaired, vec![a.project_id.clone()]);
    let fixed = s.project(a.project_id).await.unwrap().unwrap();
    assert_eq!(
        (fixed.station_id.as_str(), fixed.path.as_str()),
        ("st-1", "/work/a")
    );
    let kept = s.project(b.project_id).await.unwrap().unwrap();
    assert_eq!(kept.path, "/work/b");
    // A second run finds nothing.
    let again = super::super::call(&s.db(), |conn| projects_legacy::repair(conn))
        .await
        .unwrap();
    assert!(again.is_empty());
}

#[tokio::test]
async fn the_copy_follows_a_promotion_and_a_removal() {
    let s = store().await;
    let p = s
        .create_project("st-1".into(), input("App", "/work/app"))
        .await
        .unwrap();
    let id = p.project_id;
    add_home(&s, &id, "st-2", 1, "ready").await;
    add_home(&s, &id, "st-3", 2, "ready").await;
    // Removing the primary promotes the ready home with the lowest position.
    s.home_remove(id.clone(), "st-1".into())
        .await
        .unwrap()
        .unwrap();
    let after = s.project(id.clone()).await.unwrap().unwrap();
    assert_eq!(
        (after.station_id.as_str(), after.path.as_str()),
        ("st-2", "/srv/st-2")
    );
    let positions: Vec<(String, u32)> = after
        .homes
        .iter()
        .map(|h| (h.station_id.0.clone(), h.position))
        .collect();
    assert_eq!(positions, [("st-2".into(), 0), ("st-3".into(), 2)]);
    // Removing a home that is not the primary leaves the copy alone.
    s.home_remove(id.clone(), "st-3".into())
        .await
        .unwrap()
        .unwrap();
    let after = s.project(id).await.unwrap().unwrap();
    assert_eq!((after.station_id.as_str(), after.homes.len()), ("st-2", 1));
}
