#![allow(clippy::unwrap_used, clippy::expect_used)]

//! The remote that the deck derives from the forge (Fleet Sync §1.3).

use fleetdeck_proto::integrations::{ForgeBinding, ForgeKind, Project, ProjectInput};
use fleetdeck_testkit::Det;

use super::super::DeckStore;
use crate::knobs::DeckKnobs;

async fn store() -> DeckStore {
    let det = Det::new();
    DeckStore::open_in_memory(det.clock(), det.ids())
        .await
        .unwrap()
}

fn input(name: &str, forge: Option<&str>) -> ProjectInput {
    ProjectInput {
        name: name.into(),
        station: "x".into(),
        path: format!("/work/{name}"),
        forge: forge.map(|repo| ForgeBinding {
            kind: ForgeKind::Github,
            host: "github.com".into(),
            repo: repo.into(),
        }),
        trackers: vec![],
        prompt_template: None,
        review_prompt: None,
    }
}

fn ssh() -> DeckKnobs {
    DeckKnobs {
        clone_ssh: true,
        ..DeckKnobs::default()
    }
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

async fn get(s: &DeckStore, id: &str) -> Project {
    s.project(id.to_owned()).await.unwrap().unwrap()
}

#[tokio::test]
async fn a_new_project_with_a_forge_gets_an_https_remote() {
    let s = store().await;
    let p = s
        .create_project("st-1".into(), input("App", Some("acme/app")))
        .await
        .unwrap();
    // The answer of the create shows the remote already.
    assert_eq!(
        p.remote_url.as_deref(),
        Some("https://github.com/acme/app.git")
    );
    assert_eq!(p.remote_ssh_user, None);
    assert_eq!(p.repo.as_deref(), Some("github.com/acme/app"));
    assert_eq!(
        p.remote_dest.as_deref(),
        Some("https://github.com:443/acme/app")
    );
    assert_eq!(
        p.git_revision, 0,
        "the derivation does not bump the revision"
    );
    assert_eq!(get(&s, &p.project_id).await, p);
}

#[tokio::test]
async fn the_ssh_knob_derives_a_scp_like_remote_with_the_login_split_off() {
    let s = store().await;
    s.set_knobs(ssh());
    let p = s
        .create_project("st-1".into(), input("App", Some("acme/app")))
        .await
        .unwrap();
    assert_eq!(p.remote_url.as_deref(), Some("github.com:acme/app.git"));
    assert_eq!(p.remote_ssh_user.as_deref(), Some("git"));
    assert_eq!(
        p.remote_dest.as_deref(),
        Some("https://github.com:443/acme/app")
    );
}

#[tokio::test]
async fn a_forge_that_the_identity_refuses_saves_the_project_without_a_remote() {
    let s = store().await;
    let p = s
        .create_project("st-1".into(), input("App", Some("a/../b")))
        .await
        .unwrap();
    assert_eq!((p.remote_url, p.repo, p.remote_dest), (None, None, None));
}

#[tokio::test]
async fn an_update_derives_a_missing_remote_and_never_replaces_one() {
    let s = store().await;
    let p = s
        .create_project("st-1".into(), input("App", None))
        .await
        .unwrap();
    assert_eq!(p.remote_url, None);
    // The update adds a forge: the remote is derived.
    let p = s
        .update_project(p.project_id, "st-1".into(), input("App", Some("acme/app")))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        p.remote_url.as_deref(),
        Some("https://github.com/acme/app.git")
    );
    // Another forge later does not change the stored remote.
    let p = s
        .update_project(
            p.project_id,
            "st-1".into(),
            input("App", Some("acme/other")),
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        p.remote_url.as_deref(),
        Some("https://github.com/acme/app.git")
    );
    assert_eq!(p.repo.as_deref(), Some("github.com/acme/app"));
}

#[tokio::test]
async fn the_deck_start_derives_a_missing_remote_and_recomputes_a_stale_dest() {
    let s = store().await;
    s.set_knobs(ssh());
    let none = s
        .create_project("st-1".into(), input("None", None))
        .await
        .unwrap();
    let missing = s
        .create_project("st-1".into(), input("Missing", Some("acme/missing")))
        .await
        .unwrap();
    let stale = s
        .create_project("st-1".into(), input("Stale", Some("acme/app")))
        .await
        .unwrap();
    let odd = s
        .create_project("st-1".into(), input("Odd", None))
        .await
        .unwrap();
    // `Missing` has a forge and no remote; `Stale` has the strict dest of an
    // older release; `Odd` has a remote URL that the identity refuses.
    exec(
        &s,
        "UPDATE projects SET remote_url = NULL, remote_ssh_user = NULL, repo = NULL,
             remote_dest = NULL WHERE project_id = ?1",
        vec![missing.project_id.clone()],
    )
    .await;
    exec(
        &s,
        "UPDATE projects SET repo = 'github.com/acme/App',
             remote_dest = 'ssh://git@github.com:22/~/acme/app.git' WHERE project_id = ?1",
        vec![stale.project_id.clone()],
    )
    .await;
    exec(
        &s,
        "UPDATE projects SET remote_url = 'ext::x', repo = 'keep', remote_dest = 'keep'
         WHERE project_id = ?1",
        vec![odd.project_id.clone()],
    )
    .await;

    s.derive_forge_remotes().await.unwrap();

    let none = get(&s, &none.project_id).await;
    assert_eq!(none.remote_url, None);
    let missing = get(&s, &missing.project_id).await;
    assert_eq!(
        missing.remote_url.as_deref(),
        Some("github.com:acme/missing.git")
    );
    assert_eq!(missing.remote_ssh_user.as_deref(), Some("git"));
    assert_eq!(missing.repo.as_deref(), Some("github.com/acme/missing"));
    let stale = get(&s, &stale.project_id).await;
    assert_eq!(stale.repo.as_deref(), Some("github.com/acme/app"));
    assert_eq!(
        stale.remote_dest.as_deref(),
        Some("https://github.com:443/acme/app")
    );
    // The URL of a remote is never rewritten, and an error changes nothing.
    assert_eq!(stale.remote_url.as_deref(), Some("github.com:acme/app.git"));
    let odd = get(&s, &odd.project_id).await;
    assert_eq!(
        (
            odd.remote_url.as_deref(),
            odd.repo.as_deref(),
            odd.remote_dest.as_deref()
        ),
        (Some("ext::x"), Some("keep"), Some("keep"))
    );
    // A second run finds nothing to do.
    s.derive_forge_remotes().await.unwrap();
    assert_eq!(get(&s, &stale.project_id).await, stale);
}

async fn count(s: &DeckStore, sql: &'static str, arg: &str) -> i64 {
    let arg = arg.to_owned();
    s.db()
        .call(move |conn| conn.query_row(sql, [arg], |r| r.get(0)).map_err(Into::into))
        .await
        .unwrap()
}

const SLUGS: &str = "SELECT COUNT(*) FROM slug_reservations WHERE repo = ?1";
const LOCKS: &str = "SELECT COUNT(*) FROM land_queue WHERE lock_key = ?1";

async fn reserve(s: &DeckStore, repo: &str, slug: &str) {
    exec(
        s,
        "INSERT INTO slug_reservations (repo, slug, station_id, first_used_at)
         VALUES (?1, ?2, 'st-1', 1)",
        vec![repo.to_owned(), slug.to_owned()],
    )
    .await;
}

async fn land(s: &DeckStore, id: &str, project: &str, lock_key: &str, state: &'static str) {
    exec(
        s,
        "INSERT INTO land_queue (land_id, command_id, requested_by, request_command_id,
             project_id, lock_key, station_id, session_id, state, created_at)
         VALUES (?1, ?1, 'owner', ?1, ?2, ?3, 'st-1', ?1, ?4, 1)",
        vec![
            id.to_owned(),
            project.to_owned(),
            lock_key.to_owned(),
            state.to_owned(),
        ],
    )
    .await;
}

#[tokio::test]
async fn a_repaired_repo_key_moves_the_reservations_and_the_open_land_locks() {
    let s = store().await;
    let p = s
        .create_project("st-1".into(), input("App", Some("acme/app")))
        .await
        .unwrap();
    const OLD: &str = "github.com/Acme/App";
    const NEW: &str = "github.com/acme/app";
    // A mixed-case key of an older release, with state under it.
    exec(
        &s,
        "UPDATE projects SET repo = ?2 WHERE project_id = ?1",
        vec![p.project_id.clone(), OLD.to_owned()],
    )
    .await;
    reserve(&s, OLD, "alpha").await;
    land(&s, "l-queued", &p.project_id, OLD, "queued").await;
    land(&s, "l-ended", &p.project_id, OLD, "landed").await;

    s.derive_forge_remotes().await.unwrap();

    assert_eq!(get(&s, &p.project_id).await.repo.as_deref(), Some(NEW));
    assert_eq!(
        (count(&s, SLUGS, OLD).await, count(&s, SLUGS, NEW).await),
        (0, 1)
    );
    // The open land moves; an ended land keeps its key.
    assert_eq!(
        (count(&s, LOCKS, NEW).await, count(&s, LOCKS, OLD).await),
        (1, 1)
    );
    let moved = count(
        &s,
        "SELECT COUNT(*) FROM land_queue WHERE land_id = ?1 AND lock_key = 'github.com/acme/app'",
        "l-queued",
    )
    .await;
    assert_eq!(moved, 1);
}

#[tokio::test]
async fn a_collision_of_the_repaired_repo_key_keeps_the_existing_rows() {
    let s = store().await;
    let p = s
        .create_project("st-1".into(), input("App", Some("acme/app")))
        .await
        .unwrap();
    const OLD: &str = "github.com/Acme/App";
    const NEW: &str = "github.com/acme/app";
    exec(
        &s,
        "UPDATE projects SET repo = ?2 WHERE project_id = ?1",
        vec![p.project_id.clone(), OLD.to_owned()],
    )
    .await;
    // The slug `alpha` is reserved under both keys, `beta` only under the old
    // one. A running land of another project holds the new lock key.
    reserve(&s, OLD, "alpha").await;
    reserve(&s, OLD, "beta").await;
    reserve(&s, NEW, "alpha").await;
    land(&s, "l-old-running", &p.project_id, OLD, "running").await;
    land(&s, "l-old-queued", &p.project_id, OLD, "queued").await;
    land(&s, "l-new-running", "other", NEW, "running").await;

    s.derive_forge_remotes().await.unwrap();

    // `beta` moved; the old `alpha` stays, and the existing new `alpha` too.
    assert_eq!(
        (count(&s, SLUGS, OLD).await, count(&s, SLUGS, NEW).await),
        (1, 2)
    );
    // The queued land moved; the running land keeps the old key, because the
    // new key already has a running land.
    let key_of = |land: &'static str| {
        let s = s.clone();
        async move {
            s.db()
                .call(move |conn| {
                    conn.query_row(
                        "SELECT lock_key FROM land_queue WHERE land_id = ?1",
                        [land],
                        |r| r.get::<_, String>(0),
                    )
                    .map_err(Into::into)
                })
                .await
                .unwrap()
        }
    };
    assert_eq!(key_of("l-old-queued").await, NEW);
    assert_eq!(key_of("l-old-running").await, OLD);
    assert_eq!(key_of("l-new-running").await, NEW);
    // The deck start did not fail, and the project has the new key.
    assert_eq!(get(&s, &p.project_id).await.repo.as_deref(), Some(NEW));
}
