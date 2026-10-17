//! The remote of a project and the rules around the homes (T2.3a, design
//! §8.4, decisions B9, B10, B21): `PUT /api/projects/{id}/remote`, the
//! local-remote rule, a change of the folder of a project with homes, and a
//! station that has no `fleet_git.homes.v1`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::Arc;

use common::fleet_git_fake::{
    FakeGit, FakeOptions, REMOTE, Reply, Setup, bearer, expect, input, setup, spawn_sender,
};
use fleetdeck_deck::link::fleet_git_sync;
use fleetdeck_deck::store::AuditResult;
use fleetdeck_proto::StationId;
use fleetdeck_proto::fleet_git::GitHomeState;
use fleetdeck_proto::link::Scope;
use serde_json::{Value, json};

const BETA_PATH: &str = "/srv/projects/App";
const RENAMED: &str = "https://github.com/acme/renamed.git";

struct Two {
    s: Setup,
    alpha: Arc<FakeGit>,
    beta: StationId,
    beta_fake: Arc<FakeGit>,
    _stop: tokio::sync::watch::Sender<bool>,
}

/// The app project with `settings` and `remote`, a home on `fake` and a
/// ready home on `beta` (alpha and beta report `local` for local remotes).
async fn two(settings: Value, remote: &str, local: (bool, bool)) -> Two {
    let mut s = setup().await;
    let alpha_options = FakeOptions {
        fleet_git: Some(local.0),
    };
    let alpha = FakeGit::connect_with(&mut s.f, "fake", alpha_options).await;
    let beta_options = FakeOptions {
        fleet_git: Some(local.1),
    };
    let (beta, beta_fake) = FakeGit::connect_second(&mut s.f, "beta", beta_options).await;
    let both = [s.f.station.clone(), beta.clone()];
    s.f.wait("the station infos", |f| {
        let (store, both) = (f.store.clone(), both.clone());
        async move { both.iter().all(|i| store.station_info(i).is_some()) }
    })
    .await;
    let r = s.save(settings, Some(remote)).await;
    assert_eq!(r.status, 200, "{}", r.body);
    let (stop, _task) = spawn_sender(&s.f, true);
    Two {
        s,
        alpha,
        beta,
        beta_fake,
        _stop: stop,
    }
}

impl Two {
    fn path(&self, tail: &str) -> String {
        format!("/api/projects/{}{tail}", self.s.app.project_id)
    }

    async fn add_beta(&self, command: &str) -> common::board::Http {
        self.s
            .c
            .post(
                &self.path("/homes"),
                &self.s.h(),
                json!({ "station": self.beta.as_str(), "command_id": command }),
            )
            .await
    }

    async fn ready_beta(&self) {
        *self.beta_fake.ensure.lock().unwrap() = Reply::ready(BETA_PATH);
        let added = self.add_beta("c1").await;
        assert_eq!(added.status, 202, "{}", added.body);
        self.wait_beta(GitHomeState::Ready).await;
    }

    async fn wait_beta(&self, state: GitHomeState) {
        let (store, id, beta) = (
            self.s.f.store.clone(),
            self.s.app.project_id.clone(),
            self.beta.clone(),
        );
        self.s
            .f
            .wait("the home of beta", move |_| {
                let (store, id, beta) = (store.clone(), id.clone(), beta.clone());
                async move {
                    let p = store.project(id).await.unwrap().unwrap();
                    p.homes
                        .iter()
                        .any(|h| h.station_id == beta && h.state == state)
                }
            })
            .await;
    }

    async fn remote(&self, url: &str, rebind: bool, command: &str) -> common::board::Http {
        self.s
            .c
            .req(
                "PUT",
                &self.path("/remote"),
                &self.s.h(),
                Some(json!({ "remote_url": url, "rebind": rebind, "command_id": command })),
            )
            .await
    }
}

// ----- PUT /remote -----

#[tokio::test]
async fn the_same_destination_updates_the_url_parts_and_changes_no_home() {
    let t = two(json!({}), "https://github.com/acme/app.git", (false, false)).await;
    t.ready_beta().await;
    let before = t.s.app().await;
    let r = t
        .remote("https://GITHUB.com:443/acme/app.git", false, "r1")
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body["remote_url"], "https://GITHUB.com:443/acme/app.git");
    assert_eq!(r.body["remote_ssh_user"], Value::Null);
    assert_eq!(r.body["repo"], "github.com/acme/app");
    assert_eq!(r.body["remote_dest"], "https://github.com:443/acme/app.git");
    assert_eq!(r.body["git_revision"], before.git_revision + 1);
    let after = t.s.app().await;
    assert_eq!(after.homes, before.homes, "no home changed");
    assert_eq!(
        after.remote_url.as_deref(),
        Some("https://GITHUB.com:443/acme/app.git")
    );
    // The command id is audited, not stored: the same PUT again is fine.
    assert_eq!(
        t.remote("https://GITHUB.com:443/acme/app.git", false, "r1")
            .await
            .status,
        200
    );
    assert_eq!(
        t.s.c.audited("project.remote", AuditResult::Ok).await.len(),
        2
    );
}

#[tokio::test]
async fn a_project_without_a_remote_gets_it_without_a_rebind() {
    let mut s = setup().await;
    let fake = FakeGit::connect(&mut s.f, "fake").await;
    let r =
        s.c.req(
            "PUT",
            &format!("/api/projects/{}/remote", s.app.project_id),
            &s.h(),
            Some(json!({ "remote_url": REMOTE, "command_id": "r1" })),
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body["remote_url"], "github.com:acme/app.git");
    assert_eq!(r.body["remote_ssh_user"], "git");
    assert_eq!(r.body["git_revision"], 1);
    assert_eq!(s.app().await.repo.as_deref(), Some("github.com/acme/app"));
    // Every station with a home gets the new snapshot.
    let _ = fake;
}

#[tokio::test]
async fn another_destination_needs_a_rebind_and_a_sync_project_checks_its_homes_again() {
    let t = two(
        json!({ "sync": true }),
        "https://github.com/acme/app.git",
        (false, false),
    )
    .await;
    t.s.home("ready").await;
    t.ready_beta().await;
    let revision = t.s.app().await.git_revision;
    let refused = t.remote(RENAMED, false, "r1").await;
    expect(&refused, 409, "rebind_needed");
    assert_eq!(
        refused.body["message"],
        "This URL names another repository than github.com/acme/app. Confirm the rebind; every home must match the new remote before sync runs again."
    );
    assert_eq!(t.s.app().await.git_revision, revision, "nothing changed");
    assert_eq!(
        t.s.c.audited("project.remote", AuditResult::Refused).await,
        [Some("rebind_needed".to_owned())]
    );

    // The store write: every home waits for the check of the new remote.
    let changed =
        t.s.f
            .store
            .remote_set(t.s.app.project_id.clone(), RENAMED.to_owned(), true)
            .await
            .unwrap()
            .unwrap();
    assert_eq!(
        (changed.repo.as_str(), changed.git_revision),
        ("github.com/acme/renamed", revision + 1)
    );
    let p = t.s.app().await;
    for h in &p.homes {
        assert_eq!(h.state, GitHomeState::Unverified, "{h:?}");
        assert_eq!(h.code.as_deref(), Some("rebind_pending"));
        assert_eq!(
            h.message.as_deref(),
            Some("Waiting for the station to check the new remote.")
        );
    }
    // The station checks the new remote: both homes are ready again.
    fleet_git_sync::sync_project(&t.s.f.ctx, t.s.app.project_id.clone()).await;
    t.wait_beta(GitHomeState::Ready).await;
    t.s.home("ready").await;
    let _ = &t.alpha;
}

#[tokio::test]
async fn a_rebind_keeps_the_homes_of_a_sync_off_project_ready_and_a_busy_home_refuses_it() {
    let t = two(json!({}), "https://github.com/acme/app.git", (false, false)).await;
    t.ready_beta().await;
    // A live session in the primary home.
    let mut card = fleetdeck_proto::SessionSummary::new("s1".into());
    card.cwd = Some("/work/app".to_owned());
    t.s.f
        .store
        .apply_snapshot(t.s.f.station.clone(), t.s.f.inc.clone(), 0, vec![card])
        .await
        .unwrap();
    let busy = t.remote(RENAMED, true, "r1").await;
    expect(&busy, 409, "home_busy");
    assert_eq!(busy.body["message"], "A session runs in this home.");
    // The session ends.
    t.s.f
        .store
        .apply_snapshot(t.s.f.station.clone(), t.s.f.inc.clone(), 0, vec![])
        .await
        .unwrap();
    let r = t.remote(RENAMED, true, "r2").await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(
        r.body["remote_dest"],
        "https://github.com:443/acme/renamed.git"
    );
    for h in &t.s.app().await.homes {
        assert_eq!((h.state, h.code.clone()), (GitHomeState::Ready, None));
    }
}

#[tokio::test]
async fn the_remote_route_checks_who_asks_the_body_and_the_url() {
    let t = two(json!({}), "https://github.com/acme/app.git", (false, false)).await;
    let path = t.path("/remote");
    let body = json!({ "remote_url": RENAMED, "rebind": true, "command_id": "r1" });
    let admin = bearer(&t.s.c, "admin", &[Scope::Read, Scope::Admin]).await;
    let admin = [(admin.0.as_str(), admin.1.as_str())];
    let r = t.s.c.req("PUT", &path, &admin, Some(body.clone())).await;
    expect(&r, 403, "owner_only");
    let no_csrf = [("Cookie", t.s.cookie.as_str())];
    let r = t.s.c.req("PUT", &path, &no_csrf, Some(body)).await;
    assert_eq!(r.status, 403);
    expect(
        &t.remote(RENAMED, true, "bad id!").await,
        400,
        "invalid_command_id",
    );
    let missing =
        t.s.c
            .req(
                "PUT",
                &path,
                &t.s.h(),
                Some(json!({ "remote_url": RENAMED })),
            )
            .await;
    expect(&missing, 400, "invalid_body");
    let unknown =
        t.s.c
            .req(
                "PUT",
                "/api/projects/nope/remote",
                &t.s.h(),
                Some(json!({ "remote_url": RENAMED, "command_id": "r1" })),
            )
            .await;
    expect(&unknown, 404, "unknown_project");
    let creds = t
        .remote("https://user:secret@github.com/acme/app.git", true, "r2")
        .await;
    expect(&creds, 400, "invalid_git_settings");
    assert_eq!(creds.body["field"], "remote_url");
    assert_eq!(
        creds.body["message"],
        "The remote URL must be an https, ssh or user@host:path URL without a password."
    );
    assert_eq!(t.s.app().await.git_revision, 1, "only the first save");
}

// ----- local remotes (decision B10) -----

#[tokio::test]
async fn a_local_remote_needs_every_home_station_to_allow_it() {
    // Alpha allows local remotes, beta does not.
    let t = two(json!({}), "file:///srv/remote.git", (true, false)).await;
    assert_eq!(
        t.s.app().await.remote_url.as_deref(),
        Some("file:///srv/remote.git")
    );
    let refused = t.add_beta("c1").await;
    expect(&refused, 409, "remote_local_refused");
    assert_eq!(
        refused.body["message"],
        "This station does not allow a local remote."
    );
    assert!(t.beta_fake.requests("project.ensure_home").is_empty());
    assert_eq!(t.s.app().await.homes.len(), 1);
    let _ = &t.alpha;
}

#[tokio::test]
async fn a_station_that_reports_nothing_counts_as_not_allowing_a_local_remote() {
    let mut s = setup().await;
    let _fake = FakeGit::connect(&mut s.f, "fake").await;
    s.f.wait("the info", |f| {
        let (store, id) = (f.store.clone(), f.station.clone());
        async move { store.station_info(&id).is_some() }
    })
    .await;
    let r = s.save(json!({}), Some("file:///srv/remote.git")).await;
    expect(&r, 400, "invalid_git_settings");
    assert_eq!(
        r.body["message"],
        "Local remotes are allowed only on stations with allow_local_remotes = true."
    );
    let r =
        s.c.req(
            "PUT",
            &format!("/api/projects/{}/remote", s.app.project_id),
            &s.h(),
            Some(json!({ "remote_url": "file:///srv/remote.git", "command_id": "r1" })),
        )
        .await;
    expect(&r, 400, "invalid_git_settings");
}

// ----- the folder of a project with homes (decision B21) -----

#[tokio::test]
async fn a_new_folder_of_a_project_with_two_homes_or_a_busy_home_is_refused() {
    let t = two(json!({}), REMOTE, (false, false)).await;
    let update = |path: &'static str| async {
        let mut body = serde_json::to_value(input("App", path)).unwrap();
        body["station"] = json!("fake");
        let url = t.path("");
        t.s.c.req("PUT", &url, &t.s.h(), Some(body)).await
    };
    // One home: the folder changes and the generation grows.
    let busy = {
        let mut card = fleetdeck_proto::SessionSummary::new("s1".into());
        card.cwd = Some("/work/app/src".to_owned());
        t.s.f
            .store
            .apply_snapshot(t.s.f.station.clone(), t.s.f.inc.clone(), 0, vec![card])
            .await
            .unwrap();
        update("/work/other").await
    };
    expect(&busy, 409, "home_busy");
    assert_eq!(busy.body["message"], "A session runs in this home.");
    t.s.f
        .store
        .apply_snapshot(t.s.f.station.clone(), t.s.f.inc.clone(), 0, vec![])
        .await
        .unwrap();
    let moved = update("/work/other").await;
    assert_eq!(moved.status, 200, "{}", moved.body);
    assert_eq!(moved.body["homes"][0]["generation"], 2);
    assert_eq!(moved.body["homes"][0]["path"], "/work/other");
    // Two homes: use the Homes section.
    t.ready_beta().await;
    let two = update("/work/third").await;
    expect(&two, 409, "use_homes");
    assert_eq!(
        two.body["message"],
        "This project has more than one home. Change the homes in the Homes section."
    );
    assert_eq!(t.s.app().await.path, "/work/other");
}

// ----- a station without the homes feature (decision B6c) -----

#[tokio::test]
async fn an_attempt_for_a_station_without_the_feature_is_not_sent_until_the_next_link_start() {
    let mut s = setup().await;
    let r = s.save(json!({}), Some(REMOTE)).await;
    assert_eq!(r.status, 200, "{}", r.body);
    let _alpha = FakeGit::connect(&mut s.f, "fake").await;
    let (beta, beta_fake) = FakeGit::connect_second(&mut s.f, "beta", FakeOptions::default()).await;
    s.f.wait("the info", |f| {
        let (store, id) = (f.store.clone(), beta.clone());
        async move { store.station_info(&id).is_some() }
    })
    .await;
    *beta_fake.ensure.lock().unwrap() = Reply::ready(BETA_PATH);
    // The attempt is stored while no sender runs.
    let added =
        s.c.post(
            &format!("/api/projects/{}/homes", s.app.project_id),
            &s.h(),
            json!({ "station": beta.as_str(), "command_id": "c1" }),
        )
        .await;
    assert_eq!(added.status, 202, "{}", added.body);
    let id = added.body["attempt_id"].as_str().unwrap().to_owned();
    // Now the link of beta has no `fleet_git.homes.v1`.
    s.f.store.set_link_features(&beta, 77, vec![]);
    let (_stop, _task) = spawn_sender(&s.f, true);
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    assert!(
        beta_fake.requests("project.ensure_home").is_empty(),
        "nothing sent"
    );
    let (sends, next): (i64, Option<i64>) =
        s.f.store
            .db()
            .call({
                let id = id.clone();
                move |conn| {
                    conn.query_row(
                    "SELECT send_attempts, next_send_at FROM home_attempts WHERE attempt_id = ?1",
                    [id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .map_err(Into::into)
                }
            })
            .await
            .unwrap();
    assert_eq!((sends, next), (0, None), "no backoff, waiting for a link");
    // The link comes back with the feature: the next link start sends it.
    s.f.store.clear_link_features(&beta, 77);
    fleet_git_sync::sync_station(&s.f.ctx, beta.clone()).await;
    beta_fake.wait_for("project.ensure_home", 1).await;
    let (store, pid, station) = (s.f.store.clone(), s.app.project_id.clone(), beta.clone());
    s.f.wait("the home is ready", move |_| {
        let (store, pid, station) = (store.clone(), pid.clone(), station.clone());
        async move {
            let p = store.project(pid).await.unwrap().unwrap();
            p.homes
                .iter()
                .any(|h| h.station_id == station && h.state == GitHomeState::Ready)
        }
    })
    .await;
}
