#![allow(clippy::unwrap_used, clippy::expect_used)]

//! The homes of a project on the deck store (design §8.9, decisions B4, B13,
//! B14): the migration, the constraints, sync ownership and what a check of
//! the station does to a home.

use fleetdeck_proto::StationId;
use fleetdeck_proto::fleet_git::{
    GitSettings, GitSettingsPut, HomeCheck, HomeCheckState, RemoteIdentity, SnapshotAck,
};
use fleetdeck_proto::integrations::ProjectInput;
use fleetdeck_testkit::Det;

use super::super::projects_git::GitWrite;
use super::super::schema::migrations;
use super::*;
use crate::store::DeckStore;

const REMOTE: &str = "https://github.com/acme/app.git";

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

/// Saves the settings of `id` with `edit`, at the stored revision; the remote
/// is set at the first save.
async fn save(s: &DeckStore, id: &str, edit: impl FnOnce(&mut GitSettings)) -> GitWrite {
    let p = s.project(id.to_owned()).await.unwrap().unwrap();
    let mut settings = p.git.clone();
    edit(&mut settings);
    s.set_project_git(
        id.to_owned(),
        GitSettingsPut {
            settings,
            revision: p.git_revision,
            remote_url: p.remote_url.is_none().then(|| REMOTE.to_owned()),
        },
    )
    .await
    .unwrap()
}

async fn sync_project(s: &DeckStore, name: &str, station: &str, path: &str) -> String {
    let p = s
        .create_project(station.to_owned(), input(name, path))
        .await
        .unwrap();
    let out = save(s, &p.project_id, |g| g.sync = true).await;
    assert!(matches!(out, GitWrite::Saved(_)), "{out:?}");
    p.project_id
}

async fn home(s: &DeckStore, id: &str, station: &str) -> fleetdeck_proto::fleet_git::ProjectHome {
    let p = s.project(id.to_owned()).await.unwrap().unwrap();
    p.homes
        .into_iter()
        .find(|h| h.station_id.as_str() == station)
        .unwrap()
}

fn state(h: &fleetdeck_proto::fleet_git::ProjectHome) -> String {
    serde_json::to_value(h.state)
        .unwrap()
        .as_str()
        .unwrap()
        .to_owned()
}

fn check(
    project: &str,
    generation: u64,
    state: HomeCheckState,
    canonical: Option<&str>,
) -> HomeCheck {
    HomeCheck {
        project_id: project.to_owned(),
        generation,
        state,
        canonical_path: canonical.map(str::to_owned),
        seen: None,
        code: None,
    }
}

/// Sends the snapshot of `station` and answers it with `checks`.
async fn answer(s: &DeckStore, station: &str, checks: Vec<HomeCheck>) {
    let st = StationId::from(station.to_owned());
    let sent = s.git_snapshot_next(&st, true).await.unwrap().unwrap();
    let ack = SnapshotAck {
        stored: true,
        revision: sent.revision,
        checks,
    };
    s.git_snapshot_acked(&st, sent, ack).await.unwrap();
}

async fn canonical(s: &DeckStore, id: &str) -> Option<String> {
    let id = id.to_owned();
    s.db()
        .call(move |conn| {
            conn.query_row(
                "SELECT canonical_path FROM project_homes WHERE project_id = ?1",
                [id],
                |r| r.get(0),
            )
            .map_err(Into::into)
        })
        .await
        .unwrap()
}

// ----- the migration -----

#[test]
#[allow(clippy::type_complexity)]
fn the_migration_moves_each_project_into_a_home_and_keeps_every_field() {
    let mut conn = rusqlite::Connection::open_in_memory().unwrap();
    let migrations = migrations();
    migrations.to_version(&mut conn, 25).unwrap();
    for (id, station, path, at, git) in [
        ("p1", "st-1", "/work/app", 1, "{}"),
        ("p2", "st-2", "/srv/other", 2, r#"{"sync":true}"#),
    ] {
        conn.execute(
            "INSERT INTO projects (project_id, name, station_id, path, forge, trackers,
                 prompt_template, review_prompt, created_at, git)
             VALUES (?1, ?1, ?2, ?3, NULL, '[]', 'Do it.', NULL, ?4, ?5)",
            rusqlite::params![id, station, path, at, git],
        )
        .unwrap();
    }
    migrations.to_latest(&mut conn).unwrap();
    let rows: Vec<(String, String, i64, i64, String, String, String, i64)> = conn
        .prepare(
            "SELECT project_id, station_id, generation, position, state, path, canonical_path,
                    sync_owner FROM project_homes ORDER BY project_id",
        )
        .unwrap()
        .query_map([], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
                r.get(6)?,
                r.get(7)?,
            ))
        })
        .unwrap()
        .map(Result::unwrap)
        .collect();
    let want = [
        ("p1", "st-1", "/work/app", 0),
        ("p2", "st-2", "/srv/other", 1),
    ];
    for (row, (id, station, path, owner)) in rows.iter().zip(want) {
        assert_eq!(
            (row.0.as_str(), row.1.as_str(), row.2, row.3, row.4.as_str()),
            (id, station, 1, 0, "ready")
        );
        assert_eq!((row.5.as_str(), row.6.as_str(), row.7), (path, path, owner));
    }
    // The project fields are untouched, and the generations are written.
    let prompt: String = conn
        .query_row(
            "SELECT prompt_template FROM projects WHERE project_id = 'p1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(prompt, "Do it.");
    let generations: i64 = conn
        .query_row("SELECT COUNT(*) FROM home_generations", [], |r| r.get(0))
        .unwrap();
    assert_eq!(generations, 2);
}

#[test]
fn two_projects_on_one_folder_keep_their_home_and_only_the_oldest_sync_project_owns_it() {
    let mut conn = rusqlite::Connection::open_in_memory().unwrap();
    let migrations = migrations();
    migrations.to_version(&mut conn, 25).unwrap();
    for (id, at, git) in [
        ("young", 5, r#"{"sync":true}"#),
        ("old", 1, r#"{"sync":true}"#),
        ("off", 3, "{}"),
    ] {
        conn.execute(
            "INSERT INTO projects (project_id, name, station_id, path, trackers, created_at, git)
             VALUES (?1, ?1, 'st-1', '/work/app', '[]', ?2, ?3)",
            rusqlite::params![id, at, git],
        )
        .unwrap();
    }
    migrations.to_latest(&mut conn).unwrap();
    let get = |id: &str| -> (String, Option<String>, Option<String>, i64) {
        conn.query_row(
            "SELECT state, code, message, sync_owner FROM project_homes WHERE project_id = ?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap()
    };
    assert_eq!(get("old"), ("ready".into(), None, None, 1));
    assert_eq!(get("off"), ("ready".into(), None, None, 0));
    let young = get("young");
    assert_eq!(
        (young.0.as_str(), young.1.as_deref(), young.3),
        ("unverified", Some("home_owned"), 0)
    );
    assert_eq!(
        young.2.as_deref(),
        Some("This folder is already the home of project old with sync on.")
    );
}

// ----- the constraints -----

#[tokio::test]
async fn a_station_has_one_home_positions_are_unique_and_a_ready_home_has_a_path() {
    let s = store().await;
    let p = s
        .create_project("st-1".into(), input("App", "/work/app"))
        .await
        .unwrap();
    let id = p.project_id;
    let insert =
        |station: &'static str, position: i64, state: &'static str, path: Option<&'static str>| {
            let id = id.clone();
            let db = s.db();
            async move {
                db.call(move |conn| {
                    conn.execute(
                        "INSERT INTO project_homes (project_id, station_id, generation, position,
                         state, path, created_at, updated_at)
                     VALUES (?1, ?2, 1, ?3, ?4, ?5, 1, 1)",
                        rusqlite::params![id, station, position, state, path],
                    )
                    .map(|_| ())
                    .map_err(Into::into)
                })
                .await
            }
        };
    assert!(
        insert("st-1", 1, "cloning", None).await.is_err(),
        "one home per station"
    );
    assert!(
        insert("st-2", 0, "cloning", None).await.is_err(),
        "unique positions"
    );
    assert!(
        insert("st-2", 1, "ready", None).await.is_err(),
        "ready needs a path"
    );
    assert!(insert("st-2", 1, "cloning", None).await.is_ok());
    // The cascade: deleting the project deletes its homes and its generations.
    assert!(s.delete_project(id.clone()).await.unwrap());
    let left: i64 = s
        .db()
        .call(|conn| {
            conn.query_row(
                "SELECT (SELECT COUNT(*) FROM project_homes) + (SELECT COUNT(*) FROM home_generations)",
                [],
                |r| r.get(0),
            )
            .map_err(Into::into)
        })
        .await
        .unwrap();
    assert_eq!(left, 0);
}

// ----- sync ownership -----

#[tokio::test]
async fn sync_on_claims_the_folder_and_another_project_cannot_take_it() {
    let s = store().await;
    let first = sync_project(&s, "First", "st-1", "/work/app").await;
    let h = home(&s, &first, "st-1").await;
    assert_eq!(
        (state(&h), h.sync_owner, h.code.clone()),
        ("unverified".into(), true, None)
    );
    // The same folder: the phase-1 text.
    let twin = s
        .create_project("st-1".into(), input("Twin", "/work/app"))
        .await
        .unwrap();
    let GitWrite::Invalid(e) = save(&s, &twin.project_id, |g| g.sync = true).await else {
        panic!("refused");
    };
    assert_eq!(
        (e.field.as_str(), e.message.as_str()),
        (
            "sync",
            "Another synced project uses this folder on this station."
        )
    );
    // Inside the home of First, and above it (decision B14).
    let inside = s
        .create_project("st-1".into(), input("Inside", "/work/app/sub"))
        .await
        .unwrap();
    let GitWrite::Invalid(e) = save(&s, &inside.project_id, |g| g.sync = true).await else {
        panic!("refused");
    };
    assert_eq!(
        e.message,
        "This folder is inside the home of project First, which has sync on."
    );
    let above = s
        .create_project("st-1".into(), input("Above", "/work"))
        .await
        .unwrap();
    let GitWrite::Invalid(e) = save(&s, &above.project_id, |g| g.sync = true).await else {
        panic!("refused");
    };
    assert_eq!(
        e.message,
        "This folder contains the home of project First, which has sync on."
    );
    // Another station is free; a refused save changed nothing.
    let other = sync_project(&s, "Other", "st-2", "/work/app").await;
    assert!(home(&s, &other, "st-2").await.sync_owner);
    assert_eq!(
        s.project(twin.project_id)
            .await
            .unwrap()
            .unwrap()
            .git_revision,
        0
    );
}

#[tokio::test]
async fn sync_off_makes_the_homes_ready_and_ownerless_and_keeps_cloning_homes() {
    let s = store().await;
    let id = sync_project(&s, "App", "st-1", "/work/app").await;
    // A second home that still clones.
    let pid = id.clone();
    s.db()
        .call(move |conn| {
            conn.execute(
                "INSERT INTO project_homes (project_id, station_id, generation, position, state,
                     created_at, updated_at)
                 VALUES (?1, 'st-2', 1, 1, 'cloning', 1, 1)",
                [pid],
            )
            .map(|_| ())
            .map_err(Into::into)
        })
        .await
        .unwrap();
    let out = save(&s, &id, |g| g.sync = false).await;
    assert!(matches!(out, GitWrite::Saved(_)), "{out:?}");
    let first = home(&s, &id, "st-1").await;
    assert_eq!(
        (state(&first), first.sync_owner, first.code, first.message),
        ("ready".into(), false, None, None)
    );
    assert_eq!(state(&home(&s, &id, "st-2").await), "cloning");
}

// ----- what a check does to a home (decisions B4b, B4c) -----

#[tokio::test]
async fn a_verified_check_makes_the_home_ready_with_the_canonical_path_and_other_states_unverify_it()
 {
    let s = store().await;
    let id = sync_project(&s, "App", "st-1", "/work/app").await;
    answer(
        &s,
        "st-1",
        vec![check(&id, 1, HomeCheckState::Verified, Some("/real/app"))],
    )
    .await;
    let h = home(&s, &id, "st-1").await;
    assert_eq!((state(&h), h.code, h.message), ("ready".into(), None, None));
    assert_eq!(h.path.as_deref(), Some("/work/app"));
    assert_eq!(canonical(&s, &id).await.as_deref(), Some("/real/app"));

    for (check_state, code, text) in [
        (
            HomeCheckState::Missing,
            "missing",
            "The home /work/app on st-1 does not exist.",
        ),
        (
            HomeCheckState::NotARepo,
            "not_a_repo",
            "The home /work/app on st-1 is not a Git repository.",
        ),
        (
            HomeCheckState::Refused,
            "refused",
            "The station refused the home /work/app.",
        ),
        (
            HomeCheckState::NoRemote,
            "no_remote",
            "The home /work/app has no remote origin. Add it: git remote add origin https://github.com/acme/app.git.",
        ),
        (
            HomeCheckState::RemoteAmbiguous,
            "remote_ambiguous",
            "The remote origin of /work/app has more than one URL. Fleet Deck pushes only to one verified repository; remove the extra url or pushurl.",
        ),
        (
            HomeCheckState::Unknown,
            "unknown",
            "The station could not verify the home /work/app.",
        ),
    ] {
        // The check of an old settings revision would not count; the state of
        // the home before is `ready` or `unverified`.
        answer(&s, "st-1", vec![check(&id, 1, check_state, None)]).await;
        let h = home(&s, &id, "st-1").await;
        assert_eq!(state(&h), "unverified", "{code}");
        assert_eq!(
            (h.code.as_deref(), h.message.as_deref()),
            (Some(code), Some(text)),
            "{code}"
        );
    }
    // A mismatch names the repository that the station saw.
    let mut seen = check(&id, 1, HomeCheckState::Mismatch, None);
    seen.seen = Some(RemoteIdentity {
        repo: "github.com/acme/other".into(),
        dest: "https://github.com:443/acme/other.git".into(),
    });
    answer(&s, "st-1", vec![seen]).await;
    let h = home(&s, &id, "st-1").await;
    assert_eq!(
        h.message.as_deref(),
        Some(
            "The home /work/app on st-1 points to github.com/acme/other, not github.com/acme/app."
        )
    );
}

#[tokio::test]
async fn a_check_never_changes_a_cloning_or_failed_home_a_sync_off_home_or_another_generation() {
    let s = store().await;
    let id = sync_project(&s, "App", "st-1", "/work/app").await;
    // Another generation: dropped.
    answer(
        &s,
        "st-1",
        vec![check(&id, 7, HomeCheckState::Verified, Some("/work/app"))],
    )
    .await;
    assert_eq!(state(&home(&s, &id, "st-1").await), "unverified");
    // A failed home keeps its state.
    let pid = id.clone();
    s.db()
        .call(move |conn| {
            conn.execute(
                "UPDATE project_homes SET state = 'failed', code = 'clone_failed' WHERE project_id = ?1",
                [pid],
            )
            .map(|_| ())
            .map_err(Into::into)
        })
        .await
        .unwrap();
    // The store rule, called directly: the check would be fenced earlier.
    let late = check(&id, 1, HomeCheckState::Verified, Some("/work/app"));
    s.db()
        .call(move |conn| {
            apply_check(conn, "st-1", &late, 1).unwrap();
            Ok(())
        })
        .await
        .unwrap();
    assert_eq!(state(&home(&s, &id, "st-1").await), "failed");
}

#[tokio::test]
async fn a_home_that_becomes_ready_while_another_sync_project_owns_the_folder_is_unverified() {
    let s = store().await;
    let first = sync_project(&s, "First", "st-1", "/work/app").await;
    // A second project, sync on, on a folder that is free for now.
    let second = sync_project(&s, "Second", "st-1", "/work/second").await;
    answer(
        &s,
        "st-1",
        vec![
            check(&first, 1, HomeCheckState::Verified, Some("/work/app")),
            // The station resolves the folder of Second to the one of First.
            check(&second, 1, HomeCheckState::Verified, Some("/work/app")),
        ],
    )
    .await;
    assert_eq!(state(&home(&s, &first, "st-1").await), "ready");
    let h = home(&s, &second, "st-1").await;
    assert_eq!(
        (state(&h).as_str(), h.code.as_deref(), h.sync_owner),
        ("unverified", Some("home_owned"), false)
    );
    assert_eq!(
        h.message.as_deref(),
        Some("This folder is already the home of project First with sync on.")
    );
}

// ----- the owner of a folder (design §8.5, M12) -----

fn query(station: &str, cwd: &str) -> super::super::fleet_git_start::StartQuery {
    super::super::fleet_git_start::StartQuery {
        station: station.to_owned(),
        cwd: cwd.to_owned(),
        kind: fleetdeck_proto::fleet_git::StartKind::Worktree,
        slug: Some("st-1".to_owned()),
        feature: super::super::LinkFeature::Negotiated,
        project: None,
    }
}

#[tokio::test]
async fn the_sync_owner_of_a_folder_wins_and_a_sync_off_project_never_matches() {
    use super::super::fleet_git_start::StartOutcome;
    let s = store().await;
    let owner = sync_project(&s, "Owner", "st-1", "/work/app").await;
    answer(
        &s,
        "st-1",
        vec![check(
            &owner,
            1,
            HomeCheckState::Verified,
            Some("/work/app"),
        )],
    )
    .await;
    // A project that lost the folder at the migration: sync on, not owner.
    let twin = s
        .create_project("st-1".into(), input("Twin", "/work/app"))
        .await
        .unwrap();
    let twin_id = twin.project_id.clone();
    s.db()
        .call(move |conn| {
            conn.execute(
                "UPDATE projects SET git = '{\"sync\":true}' WHERE project_id = ?1",
                [&twin_id],
            )?;
            conn.execute(
                "UPDATE project_homes SET state = 'unverified', code = 'home_owned'
                 WHERE project_id = ?1",
                [&twin_id],
            )
            .map(|_| ())
            .map_err(Into::into)
        })
        .await
        .unwrap();
    // A sync-off project on a folder above never matches.
    s.create_project("st-1".into(), input("Above", "/work"))
        .await
        .unwrap();
    let outcome = s.git_start(query("st-1", "/work/app/src")).await.unwrap();
    let StartOutcome::Attach(start) = outcome else {
        panic!("{outcome:?}");
    };
    assert_eq!(start.project_id, owner);
    assert_eq!(
        (start.home_generation, start.home_path.as_str()),
        (1, "/work/app")
    );
    // No project holds the folder: a plain start (O1).
    assert_eq!(
        s.git_start(query("st-1", "/work/other")).await.unwrap(),
        StartOutcome::Plain
    );
    assert_eq!(
        s.git_start(query("st-2", "/work/app")).await.unwrap(),
        StartOutcome::Plain
    );
    // The owner's home is no longer ready: the start is refused, and the
    // twin does not take over.
    answer(
        &s,
        "st-1",
        vec![check(&owner, 1, HomeCheckState::Missing, None)],
    )
    .await;
    let outcome = s.git_start(query("st-1", "/work/app/src")).await.unwrap();
    assert!(
        matches!(&outcome, StartOutcome::Unverified { path, .. } if path == "/work/app"),
        "{outcome:?}"
    );
}

#[tokio::test]
async fn a_start_finds_a_home_by_its_reserved_path_and_a_named_project_ignores_the_folder() {
    use super::super::fleet_git_start::StartOutcome;
    let s = store().await;
    let id = sync_project(&s, "App", "st-1", "/work/app").await;
    let pid = id.clone();
    s.db()
        .call(move |conn| {
            conn.execute(
                "INSERT INTO project_homes (project_id, station_id, generation, position, state,
                     reserved_path, created_at, updated_at)
                 VALUES (?1, 'st-2', 1, 1, 'cloning', '/srv/projects/App', 1, 1)",
                [pid],
            )
            .map(|_| ())
            .map_err(Into::into)
        })
        .await
        .unwrap();
    let outcome = s
        .git_start(query("st-2", "/srv/projects/App/src"))
        .await
        .unwrap();
    assert_eq!(
        outcome,
        StartOutcome::NotReady("The home /srv/projects/App of App is still cloning.".into())
    );
    // A batch start names the project: the folder does not matter.
    let mut named = query("st-2", "/anywhere");
    named.project = Some(id);
    assert!(matches!(
        s.git_start(named).await.unwrap(),
        StartOutcome::NotReady(_)
    ));
}
