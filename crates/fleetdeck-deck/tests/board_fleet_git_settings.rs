//! Fleet Git on the deck, phase 1 (T1.5, design §3.1, §3.3, §8.5, §17.1):
//! `PUT /api/projects/{id}/git` (its rules, its revision and its audit), the
//! snapshot that the deck sends to the station (`fleet_git.projects_set`),
//! the start in a verified sync home, and "Push now". A fake station answers
//! the link; a sync-off project behaves as before (O1).

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::time::Duration;

use common::fleet_git_fake::{
    Check, FakeGit, PushAnswer, REMOTE, Setup, bearer, body, expect, input, invalid, setup, synced,
};
use fleetdeck_deck::link::fleet_git_sync;
use fleetdeck_deck::store::AuditResult;
use fleetdeck_proto::SessionSummary;
use fleetdeck_proto::command::SessionCommand;
use fleetdeck_proto::fleet_git::{PushState, StartKind};
use fleetdeck_proto::link::Scope;
use serde_json::{Value, json};

// ----- PUT /api/projects/{id}/git -----

#[tokio::test]
async fn the_owner_saves_the_settings_and_the_project_list_reads_them_back() {
    let s = setup().await;
    // A project that never saved reads the defaults.
    let p = s.app().await;
    assert_eq!((p.git_revision, p.git.sync), (0, false));
    assert_eq!(p.homes.len(), 1);
    assert_eq!(serde_json::to_value(p.homes[0].state).unwrap(), "ready");

    let r = s
        .save(
            json!({ "cleanup_after_hours": 48, "worktree_env": { "RUSTC_WRAPPER": "sccache" } }),
            None,
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body["git_revision"], 1);
    assert_eq!(r.body["git"]["cleanup_after_hours"], 48);
    assert_eq!(r.body["git"]["push_remote"], "origin");

    let list = s.c.get("/api/projects", &s.h()[..1]).await;
    assert_eq!(list.status, 200, "{}", list.body);
    let project = &list.body["projects"][0];
    assert_eq!(project["git_revision"], 1);
    assert_eq!(project["git"]["worktree_env"]["RUSTC_WRAPPER"], "sccache");
    assert_eq!(project["homes"][0]["position"], 0);
    assert_eq!(project["homes"][0]["generation"], 1);
    assert_eq!(project["homes"][0]["path"], "/work/app");
    assert_eq!(
        s.c.audited("project.git_set", AuditResult::Ok).await.len(),
        1
    );
}

#[tokio::test]
async fn the_remote_url_is_scrubbed_and_split_and_sync_needs_it() {
    let s = setup().await;
    let r = s.save(json!({ "sync": true }), None).await;
    invalid(&r, "sync", "Set the remote URL first.");

    let r = s.save(json!({ "sync": true }), Some(REMOTE)).await;
    assert_eq!(r.status, 200, "{}", r.body);
    let p = s.app().await;
    assert_eq!(p.remote_url.as_deref(), Some("github.com:acme/app.git"));
    assert_eq!(p.remote_ssh_user.as_deref(), Some("git"));
    assert_eq!(p.repo.as_deref(), Some("github.com/acme/app"));
    assert_eq!(
        p.remote_dest.as_deref(),
        Some("ssh://git@github.com:22/~/acme/app.git")
    );
    // No link yet: the home is not verified.
    assert_eq!(
        serde_json::to_value(p.homes[0].state).unwrap(),
        "unverified"
    );
    assert!(p.homes[0].sync_owner);
}

#[tokio::test]
async fn each_rule_refuses_with_its_field_and_changes_nothing() {
    let s = setup().await;
    for (settings, remote, field, message) in [
        (
            json!({ "cleanup_after_hours": 0 }),
            None,
            "cleanup_after_hours",
            "Clean up after 1 to 720 hours.",
        ),
        (
            json!({ "push": "on_turn" }),
            None,
            "push",
            "Turn on sync first.",
        ),
        (
            json!({ "worktree_env": { "PATH": "/x" } }),
            None,
            "worktree_env",
            "The variable PATH is not allowed.",
        ),
        (
            json!({ "wip_remote": "file:///srv/wip.git" }),
            None,
            "wip_remote",
            "Local remotes are allowed only on stations with allow_local_remotes = true.",
        ),
        (
            json!({ "radar_home": "another-station" }),
            None,
            "radar_home",
            "Pick a station with a ready home.",
        ),
        (
            json!({}),
            Some("https://user:secret@github.com/acme/app.git"),
            "remote_url",
            "The remote URL must be an https, ssh or user@host:path URL without a password.",
        ),
        (
            json!({}),
            Some("file:///srv/remote.git"),
            "remote_url",
            "Local remotes are allowed only on stations with allow_local_remotes = true.",
        ),
        (
            json!({}),
            Some("https://github.com/acme/a%2Fb.git"),
            "remote_url",
            "This remote URL has an escaped path. Use a plain path.",
        ),
        (
            json!({}),
            Some("bad name@github.com:acme/app.git"),
            "remote_url",
            "The remote URL must be an https, ssh or user@host:path URL without a password.",
        ),
    ] {
        let r = s.save(settings.clone(), remote).await;
        invalid(&r, field, message);
    }
    // The radar home may be the station of the project.
    let own = s.f.station.0.clone();
    let r = s.save(json!({ "radar_home": own }), None).await;
    assert_eq!(r.status, 200, "{}", r.body);
    // Every refusal left the stored state alone: one save, revision 1.
    assert_eq!(s.app().await.git_revision, 1);
    assert_eq!(s.app().await.remote_url, None);
}

#[tokio::test]
async fn another_synced_project_on_the_folder_is_refused() {
    let s = setup().await;
    let twin =
        s.f.store
            .create_project(s.f.station.0.clone(), input("Twin", "/work/app"))
            .await
            .unwrap();
    let other =
        s.f.store
            .create_project(s.f.station.0.clone(), input("Other", "/work/other"))
            .await
            .unwrap();
    assert_eq!(
        s.save(json!({ "sync": true }), Some(REMOTE)).await.status,
        200
    );
    let r = s
        .put(
            &twin.project_id,
            body(json!({ "sync": true }), 0, Some(REMOTE)),
        )
        .await;
    invalid(
        &r,
        "sync",
        "Another synced project uses this folder on this station.",
    );
    // Another folder is free, and the twin may save with sync off.
    let r = s
        .put(
            &other.project_id,
            body(json!({ "sync": true }), 0, Some(REMOTE)),
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    let r = s.put(&twin.project_id, body(json!({}), 0, None)).await;
    assert_eq!(r.status, 200, "{}", r.body);
}

#[tokio::test]
async fn a_stale_revision_is_refused_and_a_new_remote_needs_sync_off() {
    let s = setup().await;
    let id = s.app.project_id.clone();
    assert_eq!(s.put(&id, body(json!({}), 0, None)).await.status, 200);
    let r = s.put(&id, body(json!({}), 0, None)).await;
    expect(&r, 409, "stale_revision");
    assert_eq!(
        r.body["message"],
        "The Git settings changed. Reload and try again."
    );

    let r = s
        .put(&id, body(json!({ "sync": true }), 1, Some(REMOTE)))
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    // Sync is on: another URL is a rebind. The same URL is no change.
    let r = s
        .put(
            &id,
            body(
                json!({ "sync": true }),
                2,
                Some("git@github.com:acme/other.git"),
            ),
        )
        .await;
    expect(&r, 409, "rebind_required");
    assert_eq!(r.body["message"], "Turn off sync to change the remote URL.");
    let r = s
        .put(&id, body(json!({ "sync": true }), 2, Some(REMOTE)))
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    // The rule looks at the stored sync: turning it off and changing the URL
    // in one save is refused too.
    let other = Some("https://github.com/acme/other.git");
    let r = s.put(&id, body(json!({}), 3, other)).await;
    expect(&r, 409, "rebind_required");
    // Sync off first, then the change goes through and recomputes the identity.
    assert_eq!(s.put(&id, body(json!({}), 3, None)).await.status, 200);
    let r = s.put(&id, body(json!({}), 4, other)).await;
    assert_eq!(r.status, 200, "{}", r.body);
    let p = s.app().await;
    assert_eq!(p.repo.as_deref(), Some("github.com/acme/other"));
    assert_eq!(p.remote_ssh_user, None);
    assert_eq!(p.git_revision, 5);
}

#[tokio::test]
async fn only_the_owner_login_with_a_csrf_token_saves() {
    let s = setup().await;
    let id = s.app.project_id.clone();
    let path = format!("/api/projects/{id}/git");
    let b = body(json!({}), 0, None);

    let admin = bearer(&s.c, "admin", &[Scope::Read, Scope::Admin]).await;
    let admin = [(admin.0.as_str(), admin.1.as_str())];
    let r = s.c.req("PUT", &path, &admin, Some(b.clone())).await;
    expect(&r, 403, "owner_only");
    let drive = bearer(&s.c, "driver", &[Scope::Read, Scope::Drive]).await;
    let drive = [(drive.0.as_str(), drive.1.as_str())];
    assert_eq!(
        s.c.req("PUT", &path, &drive, Some(b.clone())).await.status,
        403
    );
    let no_csrf = [("Cookie", s.cookie.as_str())];
    assert_eq!(
        s.c.req("PUT", &path, &no_csrf, Some(b.clone()))
            .await
            .status,
        403
    );
    assert_eq!(s.app().await.git_revision, 0);

    let r = s.put("nope", b.clone()).await;
    expect(&r, 404, "unknown_project");
    let r = s
        .put(&id, json!({ "settings": {}, "revision": 0, "extra": 1 }))
        .await;
    expect(&r, 400, "invalid_body");
    let r = s
        .put(&id, json!({ "settings": { "unknown": 1 }, "revision": 0 }))
        .await;
    expect(&r, 400, "invalid_body");
}

// ----- the snapshot -----

#[tokio::test]
async fn a_new_link_gets_the_snapshot_and_a_verified_check_makes_the_home_ready() {
    let (s, fake) = synced().await;
    let snapshots = fake.snapshots();
    assert_eq!(snapshots.len(), 1, "{snapshots:?}");
    let snapshot = &snapshots[0];
    assert!(snapshot.revision > 0);
    assert_eq!(snapshot.projects.len(), 1);
    let view = &snapshot.projects[0];
    assert_eq!(view.project_id, s.app.project_id);
    assert_eq!(view.project_name, "App");
    assert!(view.settings.sync);
    assert_eq!(view.settings_revision, 1);
    assert_eq!(view.remote.repo, "github.com/acme/app");
    assert_eq!(view.remote.dest, "ssh://git@github.com:22/~/acme/app.git");
    assert_eq!(view.clone_url, REMOTE);
    assert_eq!(view.forge.as_ref().unwrap().repo, "acme/app");
    assert_eq!(
        (view.home_generation, view.home_path.as_str()),
        (1, "/work/app")
    );
    assert!(!view.radar_home);
    assert_eq!(snapshot.homes.len(), 1);
    assert!(snapshot.homes[0].sync);

    let station = s.f.station.clone();
    let (revision, acked) =
        s.f.store
            .git_snapshot_state(&station)
            .await
            .unwrap()
            .unwrap();
    assert_eq!(
        (revision, acked),
        (snapshot.revision, Some(snapshot.revision))
    );
    let checks = s.f.store.git_checks(&station).await.unwrap();
    assert_eq!(checks.len(), 1);
    assert_eq!(s.app().await.homes[0].code, None);
}

#[tokio::test]
async fn a_save_sends_a_higher_revision_and_an_unchanged_state_sends_nothing() {
    let (s, fake) = synced().await;
    let first = fake.snapshots()[0].revision;

    let r = s
        .save(json!({ "sync": true, "push": "on_turn" }), None)
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    let all = {
        let got = fake.wait_for("fleet_git.projects_set", 2).await;
        assert_eq!(got.len(), 2);
        fake.snapshots()
    };
    assert!(all[1].revision > first, "{} > {first}", all[1].revision);
    assert_eq!(all[1].projects[0].settings_revision, 2);
    s.home("ready").await;

    // The station acknowledged this state: a change trigger sends nothing.
    let station = s.f.station.clone();
    fleet_git_sync::sync_changed(&s.f.ctx, station.clone()).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(fake.requests("fleet_git.projects_set").len(), 2);
    // A new link-up sends it again, with the same revision.
    fleet_git_sync::sync_station(&s.f.ctx, station).await;
    let all = fake.snapshots();
    assert_eq!(all.len(), 3);
    assert_eq!(all[2].revision, all[1].revision);
}

#[tokio::test]
async fn a_station_that_says_stale_gets_a_revision_above_its_own() {
    let (s, fake) = synced().await;
    let theirs = fake.snapshots()[0].revision + 10_000_000_000;
    *fake.stale_once.lock().unwrap() = Some(theirs);
    let r = s
        .save(json!({ "sync": true, "cleanup_after_hours": 5 }), None)
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    let got = fake.wait_for("fleet_git.projects_set", 3).await;
    assert_eq!(got.len(), 3);
    let all = fake.snapshots();
    assert!(all[2].revision > theirs, "{} > {theirs}", all[2].revision);
    s.home("ready").await;
    let state =
        s.f.store
            .git_snapshot_state(&s.f.station)
            .await
            .unwrap()
            .unwrap();
    assert_eq!(state, (all[2].revision, Some(all[2].revision)));
}

#[tokio::test]
async fn a_mismatch_keeps_the_home_unverified_and_a_save_forgets_a_verified_check() {
    let (s, fake) = synced().await;
    *fake.check.lock().unwrap() = Check::Mismatch;
    let r = s
        .save(json!({ "sync": true, "cleanup_after_hours": 6 }), None)
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    fake.wait_for("fleet_git.projects_set", 2).await;
    s.f.wait("the mismatch", |_| {
        let (store, id) = (s.f.store.clone(), s.app.project_id.clone());
        async move {
            let p = store.project(id).await.unwrap().unwrap();
            p.homes[0].code.as_deref() == Some("mismatch")
        }
    })
    .await;
    s.home("unverified").await;
    // The check of a save is not kept before the station answers it.
    *fake.check.lock().unwrap() = Check::Verified;
    let r = s
        .save(json!({ "sync": true, "cleanup_after_hours": 7 }), None)
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    s.home("ready").await;
}

#[tokio::test]
async fn a_deleted_project_leaves_the_snapshot() {
    let (s, fake) = synced().await;
    let r =
        s.c.req(
            "DELETE",
            &format!("/api/projects/{}", s.app.project_id),
            &s.h(),
            None,
        )
        .await;
    assert_eq!(r.status, 204, "{}", r.body);
    let got = fake.wait_for("fleet_git.projects_set", 2).await;
    assert_eq!(got.len(), 2);
    let last = fake.snapshots().pop().unwrap();
    assert!(
        last.projects.is_empty() && last.homes.is_empty(),
        "{last:?}"
    );
}

// ----- the start -----

fn start_of(fake: &FakeGit, n: usize) -> Option<Box<fleetdeck_proto::fleet_git::FleetGitStart>> {
    match &fake.starts()[n].command {
        SessionCommand::Start { fleet_git, .. } => fleet_git.clone(),
        other => panic!("not a start: {other:?}"),
    }
}

#[tokio::test]
async fn a_start_in_the_verified_home_of_a_sync_project_carries_the_snapshot() {
    let (s, fake) = synced().await;
    let r = s
        .start("c1", "/work/app/src", json!({ "kind": "worktree" }))
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    let start = start_of(&fake, 0).expect("a fleet git start");
    assert_eq!(start.kind, StartKind::Worktree);
    assert_eq!(start.project_id, s.app.project_id);
    assert_eq!(start.project_name, "App");
    assert_eq!(start.slug, "fake");
    assert_eq!((start.settings_revision, start.home_generation), (1, 1));
    assert_eq!(start.home_path, "/work/app");
    assert_eq!(start.clone_url, REMOTE);
    assert_eq!(start.remote.repo, "github.com/acme/app");
    assert!(start.settings.sync);
    // The name is reserved for this station, for everybody to see.
    assert_eq!(s.app().await.slugs.len(), 1);
    assert_eq!(s.app().await.slugs[0].slug, "fake");

    // No workspace request: an in-place start. A clone: a plain start.
    assert_eq!(s.start("c2", "/work/app", Value::Null).await.status, 200);
    assert_eq!(start_of(&fake, 1).unwrap().kind, StartKind::InPlace);
    let clone = json!({ "kind": "clone", "url": "https://github.com/acme/app.git" });
    assert_eq!(s.start("c3", "/work/app", clone).await.status, 200);
    assert!(start_of(&fake, 2).is_none());
    // Another folder is a plain start (O1).
    let r = s
        .start("c4", "/work/other", json!({ "kind": "worktree" }))
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert!(start_of(&fake, 3).is_none());
    // The path rule is by components: `/work/application` is not inside.
    let r = s
        .start("c5", "/work/application", json!({ "kind": "worktree" }))
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert!(start_of(&fake, 4).is_none());
}

#[tokio::test]
async fn a_start_from_the_board_cannot_bring_its_own_fleet_git_snapshot() {
    let (s, fake) = synced().await;
    let own = start_of_value(&s).await;
    let r =
        s.c.post(
            "/api/commands/session.start",
            &s.h(),
            json!({
                "station": "fake",
                "command_id": "forged",
                "command": {
                    "op": "start",
                    "spec": { "provider_instance": "claude", "cwd": "/work/other" },
                    "prompt": null,
                    "workspace": { "kind": "worktree" },
                    "fleet_git": own
                }
            }),
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert!(start_of(&fake, 0).is_none(), "the deck dropped the field");
}

async fn start_of_value(s: &Setup) -> Value {
    let p = s.app().await;
    json!({
        "kind": "worktree",
        "project_id": p.project_id,
        "project_name": "App",
        "settings": p.git,
        "settings_revision": p.git_revision,
        "home_generation": 1,
        "home_path": "/work/app",
        "remote": { "repo": "github.com/acme/app", "dest": "ssh://git@github.com:22/~/acme/app.git" },
        "slug": "someone-else",
        "forge": null,
        "clone_url": REMOTE
    })
}

#[tokio::test]
async fn a_start_in_an_unverified_home_is_refused_and_sends_nothing() {
    let (s, fake) = synced().await;
    *fake.check.lock().unwrap() = Check::Mismatch;
    let r = s
        .save(json!({ "sync": true, "cleanup_after_hours": 6 }), None)
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    s.home("unverified").await;
    let r = s
        .start("c1", "/work/app", json!({ "kind": "worktree" }))
        .await;
    expect(&r, 409, "home_unverified");
    assert_eq!(
        r.body["message"],
        "The home /work/app on fake does not point to github.com/acme/app. Fix its remote, then try again."
    );
    assert!(fake.starts().is_empty());
    // No check at all (a fresh save) is unverified too.
    assert_eq!(s.app().await.slugs.len(), 0, "a refusal reserves nothing");
}

#[tokio::test]
async fn a_git_name_that_another_station_reserved_refuses_the_start() {
    let (s, fake) = synced().await;
    s.f.store
        .db()
        .call(|conn| {
            conn.execute(
                "INSERT INTO slug_reservations (repo, slug, station_id, first_used_at)
                 VALUES ('github.com/acme/app', 'fake', 'other-station', 1)",
                [],
            )
            .map(|_| ())
            .map_err(Into::into)
        })
        .await
        .unwrap();
    let r = s
        .start("c1", "/work/app", json!({ "kind": "worktree" }))
        .await;
    expect(&r, 409, "slug_taken");
    assert_eq!(
        r.body["message"],
        "The Git name fake is reserved for station other-station in github.com/acme/app. Rename this station."
    );
    assert!(fake.starts().is_empty());
}

#[tokio::test]
async fn a_start_needs_the_station_name_before_anything_else() {
    // The plain fixture leaves `station.info` unanswered: the deck does not
    // know the name.
    let mut s = setup().await;
    let r = s.save(json!({ "sync": true }), Some(REMOTE)).await;
    assert_eq!(r.status, 200, "{}", r.body);
    let _station = s.f.connect().await;
    let r = s
        .start("c1", "/work/app", json!({ "kind": "worktree" }))
        .await;
    expect(&r, 409, "station_info_missing");
    assert_eq!(
        r.body["message"],
        "The deck does not know this station's name yet. Try again in a moment."
    );
}

#[tokio::test]
async fn a_sync_off_project_starts_exactly_as_before() {
    let mut s = setup().await;
    let fake = FakeGit::connect(&mut s.f, "fake").await;
    // The snapshot lists the home only in `homes`, with sync off.
    let snapshots = fake.wait_for("fleet_git.projects_set", 1).await;
    assert_eq!(snapshots.len(), 1);
    let snapshot = &fake.snapshots()[0];
    assert!(snapshot.projects.is_empty());
    assert_eq!(snapshot.homes.len(), 1);
    assert!(!snapshot.homes[0].sync);
    // A start has no Fleet Git part and reserves nothing.
    let r = s
        .start("c1", "/work/app", json!({ "kind": "worktree" }))
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert!(start_of(&fake, 0).is_none());
    assert!(s.app().await.slugs.is_empty());
    // Saving other settings of a sync-off project changes no start either.
    let r = s.save(json!({ "cleanup_after_hours": 12 }), None).await;
    assert_eq!(r.status, 200, "{}", r.body);
    let r = s.start("c2", "/work/app", Value::Null).await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert!(start_of(&fake, 1).is_none());
}

#[tokio::test]
async fn a_batch_start_in_a_sync_project_carries_the_snapshot() {
    let (s, fake) = synced().await;
    let path = format!("/api/projects/{}/sessions", s.app.project_id);
    let batch = json!({
        "batch_id": "b1", "issues": ["#7"], "provider_instance": "claude", "count_per_issue": 1
    });
    let r = s.c.post(&path, &s.h(), batch.clone()).await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body["failed"], json!([]), "{}", r.body);
    let start = start_of(&fake, 0).expect("a fleet git start");
    assert_eq!(start.kind, StartKind::Worktree);
    assert_eq!(start.project_id, s.app.project_id);

    // An unverified home fails the start of the batch with the same 409.
    *fake.check.lock().unwrap() = Check::Mismatch;
    let r = s
        .save(json!({ "sync": true, "cleanup_after_hours": 6 }), None)
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    s.home("unverified").await;
    let mut again = batch;
    again["batch_id"] = json!("b2");
    let r = s.c.post(&path, &s.h(), again).await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body["started"], json!([]), "{}", r.body);
    assert_eq!(r.body["failed"][0]["code"], "home_unverified", "{}", r.body);
    assert_eq!(fake.starts().len(), 1);
}

// ----- Push now -----

fn card(id: &str) -> SessionSummary {
    let mut s = SessionSummary::new(id.into());
    s.cwd = Some("/work/app".to_owned());
    s
}

#[tokio::test]
async fn push_now_forwards_to_the_station_and_maps_its_errors() {
    let (s, fake) = synced().await;
    s.f.store
        .apply_snapshot(s.f.station.clone(), s.f.inc.clone(), 0, vec![card("s1")])
        .await
        .unwrap();
    let path = "/api/sessions/fake/s1/push";
    let r = s.c.post(path, &s.h(), json!({})).await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body, json!({ "state": "due" }));
    assert_eq!(
        fake.requests("fleet_git.push_now"),
        [json!({ "session_id": "s1" })]
    );
    assert_eq!(
        s.c.audited("session.push_now", AuditResult::Ok).await.len(),
        1
    );

    for (answer, status, code, text) in [
        (
            PushAnswer::Error("not_synced"),
            409,
            "not_synced",
            "This session is not in a synced project.",
        ),
        (
            PushAnswer::Error("held"),
            409,
            "held",
            "A land or a move holds this session.",
        ),
        (
            PushAnswer::Error("unknown_session"),
            404,
            "unknown_session",
            "The station does not know this session.",
        ),
        (
            PushAnswer::Error("boom"),
            502,
            "station_error",
            "boom: the fake says no",
        ),
    ] {
        *fake.push.lock().unwrap() = answer;
        let r = s.c.post(path, &s.h(), json!({})).await;
        expect(&r, status, code);
        assert_eq!(r.body["message"], text);
    }
    *fake.push.lock().unwrap() = PushAnswer::State(PushState::Pushed);
    let r = s.c.post(path, &s.h(), json!({})).await;
    assert_eq!(r.body, json!({ "state": "pushed" }));

    // The deck knows its sessions: nothing goes to the station for another.
    let before = fake.requests("fleet_git.push_now").len();
    let r =
        s.c.post("/api/sessions/fake/zzz/push", &s.h(), json!({}))
            .await;
    expect(&r, 404, "unknown_session");
    let r =
        s.c.post("/api/sessions/nope/s1/push", &s.h(), json!({}))
            .await;
    expect(&r, 404, "unknown_station");
    assert_eq!(fake.requests("fleet_git.push_now").len(), before);
}

#[tokio::test]
async fn push_now_needs_drive_and_a_csrf_token_and_is_closed_to_a_read_token() {
    let (s, _fake) = synced().await;
    s.f.store
        .apply_snapshot(s.f.station.clone(), s.f.inc.clone(), 0, vec![card("s1")])
        .await
        .unwrap();
    let path = "/api/sessions/fake/s1/push";
    let read = bearer(&s.c, "reader", &[Scope::Read]).await;
    let read = [(read.0.as_str(), read.1.as_str())];
    assert_eq!(s.c.post(path, &read, json!({})).await.status, 403);
    let no_csrf = [("Cookie", s.cookie.as_str())];
    assert_eq!(s.c.post(path, &no_csrf, json!({})).await.status, 403);
    let drive = bearer(&s.c, "driver", &[Scope::Read, Scope::Drive]).await;
    let drive = [(drive.0.as_str(), drive.1.as_str())];
    let r = s.c.post(path, &drive, json!({})).await;
    assert_eq!(r.status, 200, "{}", r.body);
}
