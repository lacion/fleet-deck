//! A session start in a home of a project (T2.3a, design §8.5, M12,
//! decisions B8, B16, B20): the owner of a folder is found first and its
//! readiness checked second; the batch start names a home; a sync-off
//! project starts as before.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::Arc;

use common::fleet_git_fake::{
    Check, FakeGit, FakeOptions, REMOTE, Reply, Setup, expect, setup, spawn_sender,
};
use fleetdeck_proto::StationId;
use fleetdeck_proto::command::SessionCommand;
use fleetdeck_proto::fleet_git::{FleetGitStart, GitHomeState, StartKind};
use serde_json::{Value, json};

const BETA_PATH: &str = "/srv/projects/App";

struct Two {
    s: Setup,
    alpha: Arc<FakeGit>,
    beta: StationId,
    beta_fake: Arc<FakeGit>,
    _stop: tokio::sync::watch::Sender<bool>,
}

/// The app project (remote set, `settings`) with a home on `fake` and a
/// second station `beta`; the sender runs.
async fn two(settings: Value) -> Two {
    let mut s = setup().await;
    let alpha = FakeGit::connect(&mut s.f, "fake").await;
    let (beta, beta_fake) = FakeGit::connect_second(&mut s.f, "beta", FakeOptions::default()).await;
    let both = [s.f.station.clone(), beta.clone()];
    s.f.wait("the station infos", |f| {
        let (store, both) = (f.store.clone(), both.clone());
        async move { both.iter().all(|i| store.station_info(i).is_some()) }
    })
    .await;
    let r = s.save(settings, Some(REMOTE)).await;
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
    async fn add_beta(&self, reply: Reply) {
        *self.beta_fake.ensure.lock().unwrap() = reply;
        let added = self
            .s
            .c
            .post(
                &format!("/api/projects/{}/homes", self.s.app.project_id),
                &self.s.h(),
                json!({ "station": self.beta.as_str(), "command_id": "add-1" }),
            )
            .await;
        assert_eq!(added.status, 202, "{}", added.body);
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

    /// `POST /api/commands/session.start` on `station` in `cwd`.
    async fn start(&self, station: &str, id: &str, cwd: &str) -> common::board::Http {
        let command = json!({
            "op": "start",
            "spec": { "provider_instance": "claude", "cwd": cwd },
            "prompt": null,
            "workspace": { "kind": "worktree" }
        });
        self.s
            .c
            .post(
                "/api/commands/session.start",
                &self.s.h(),
                json!({ "station": station, "command_id": id, "command": command }),
            )
            .await
    }

    async fn batch(&self, body: Value) -> common::board::Http {
        self.s
            .c
            .post(
                &format!("/api/projects/{}/sessions", self.s.app.project_id),
                &self.s.h(),
                body,
            )
            .await
    }
}

fn fleet_git_of(fake: &FakeGit, n: usize) -> Option<Box<FleetGitStart>> {
    match &fake.starts()[n].command {
        SessionCommand::Start { fleet_git, .. } => fleet_git.clone(),
        other => panic!("not a start: {other:?}"),
    }
}

fn cwd_of(fake: &FakeGit, n: usize) -> String {
    match &fake.starts()[n].command {
        SessionCommand::Start { spec, .. } => spec.cwd.clone(),
        other => panic!("not a start: {other:?}"),
    }
}

fn batch(id: &str) -> Value {
    json!({
        "batch_id": id, "issues": ["#7"], "provider_instance": "claude", "count_per_issue": 1
    })
}

// ----- a direct start by path (M12) -----

#[tokio::test]
async fn a_start_in_a_sync_home_that_is_not_verified_cloning_or_failed_is_refused() {
    let t = two(json!({ "sync": true })).await;
    t.s.home("ready").await;
    // An unverified home of a sync project.
    *t.alpha.check.lock().unwrap() = Check::Mismatch;
    let r =
        t.s.save(json!({ "sync": true, "cleanup_after_hours": 6 }), None)
            .await;
    assert_eq!(r.status, 200, "{}", r.body);
    t.s.home("unverified").await;
    let r = t.start("fake", "c1", "/work/app/src").await;
    expect(&r, 409, "home_unverified");
    assert!(t.alpha.starts().is_empty(), "nothing was sent");

    // A home that still clones: the folder is its reserved path.
    *t.beta_fake.status.lock().unwrap() = Reply::cloning(BETA_PATH);
    t.add_beta(Reply::cloning(BETA_PATH)).await;
    t.wait_beta(GitHomeState::Cloning).await;
    t.s.f
        .wait("the reserved path", |f| {
            let (store, id) = (f.store.clone(), t.s.app.project_id.clone());
            async move {
                let p = store.project(id).await.unwrap().unwrap();
                p.homes.iter().any(|h| h.reserved_path.is_some())
            }
        })
        .await;
    let r = t.start("beta", "c2", "/srv/projects/App/src").await;
    expect(&r, 409, "home_not_ready");
    assert_eq!(
        r.body["message"],
        "The home /srv/projects/App of App is still cloning."
    );
    assert!(t.beta_fake.starts().is_empty());
}

#[tokio::test]
async fn a_start_in_a_failed_sync_home_names_the_failure() {
    let t = two(json!({ "sync": true })).await;
    t.add_beta(Reply::failed("clone_failed", "The clone failed: boom"))
        .await;
    t.wait_beta(GitHomeState::Failed).await;
    let r = t.start("beta", "c1", "/srv/projects/App").await;
    expect(&r, 409, "home_not_ready");
    assert_eq!(
        r.body["message"],
        "The home /srv/projects/App of App failed: The clone failed: boom. Retry or remove it."
    );
    assert!(t.beta_fake.starts().is_empty());
}

#[tokio::test]
async fn a_ready_sync_home_starts_with_its_generation_and_a_sync_off_home_starts_plain() {
    let t = two(json!({ "sync": true })).await;
    t.s.home("ready").await;
    t.add_beta(Reply::ready(BETA_PATH)).await;
    t.wait_beta(GitHomeState::Ready).await;
    let r = t.start("beta", "c1", "/srv/projects/App/src").await;
    assert_eq!(r.status, 200, "{}", r.body);
    let start = fleet_git_of(&t.beta_fake, 0).expect("a fleet git start");
    assert_eq!(start.kind, StartKind::Worktree);
    assert_eq!(
        (
            start.home_generation,
            start.home_path.as_str(),
            start.slug.as_str()
        ),
        (1, BETA_PATH, "beta")
    );
    let r = t.start("fake", "c2", "/work/app").await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert!(fleet_git_of(&t.alpha, 0).is_some());

    // The same project with sync off: a plain start in every home.
    let r = t.s.save(json!({}), None).await;
    assert_eq!(r.status, 200, "{}", r.body);
    let r = t.start("beta", "c3", "/srv/projects/App").await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert!(fleet_git_of(&t.beta_fake, 1).is_none());
}

// ----- the batch start names a home (decision B8) -----

#[tokio::test]
async fn a_batch_start_goes_to_the_chosen_home_and_its_station() {
    let t = two(json!({ "sync": true })).await;
    t.s.home("ready").await;
    t.add_beta(Reply::ready(BETA_PATH)).await;
    t.wait_beta(GitHomeState::Ready).await;
    // Absent: the position-0 home.
    let r = t.batch(batch("b1")).await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body["failed"], json!([]), "{}", r.body);
    assert_eq!(t.alpha.requests("issues.get").len(), 1);
    assert_eq!(cwd_of(&t.alpha, 0), "/work/app");
    // A station id: the reads and the starts go to that station.
    let mut body = batch("b2");
    body["home"] = json!(t.beta.as_str());
    let r = t.batch(body).await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body["failed"], json!([]), "{}", r.body);
    assert_eq!(t.beta_fake.requests("issues.get").len(), 1);
    assert_eq!(
        t.alpha.requests("issues.get").len(),
        1,
        "alpha was not asked"
    );
    assert_eq!(cwd_of(&t.beta_fake, 0), BETA_PATH);
    let start = fleet_git_of(&t.beta_fake, 0).expect("a fleet git start");
    assert_eq!(
        (start.home_generation, start.home_path.as_str()),
        (1, BETA_PATH)
    );
}

#[tokio::test]
async fn a_batch_start_refuses_an_unknown_home_and_a_home_that_is_not_ready() {
    // `home = "auto"` is placed (T2.3b); see `board_fleet_git_placement.rs`.
    let t = two(json!({})).await;
    for (n, home) in ["nope", t.beta.as_str()].into_iter().enumerate() {
        let mut body = batch(&format!("b{}", n + 2));
        body["home"] = json!(home);
        let r = t.batch(body).await;
        expect(&r, 400, "not_a_home");
        assert_eq!(
            r.body["message"],
            "This folder is not a home of the project."
        );
    }
    // A sync-off project: a cloning home cannot start.
    t.add_beta(Reply::cloning(BETA_PATH)).await;
    t.wait_beta(GitHomeState::Cloning).await;
    let mut body = batch("b5");
    body["home"] = json!(t.beta.as_str());
    let r = t.batch(body).await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body["started"], json!([]), "{}", r.body);
    assert_eq!(r.body["failed"][0]["key"], "#7");
    assert_eq!(r.body["failed"][0]["code"], "home_not_ready");
    assert!(t.beta_fake.starts().is_empty());
}

#[tokio::test]
async fn a_batch_start_in_a_ready_home_of_a_sync_off_project_is_plain_with_the_home_path() {
    let t = two(json!({})).await;
    t.add_beta(Reply::ready(BETA_PATH)).await;
    t.wait_beta(GitHomeState::Ready).await;
    let mut body = batch("b1");
    body["home"] = json!(t.beta.as_str());
    let r = t.batch(body).await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body["failed"], json!([]), "{}", r.body);
    assert!(fleet_git_of(&t.beta_fake, 0).is_none());
    assert_eq!(cwd_of(&t.beta_fake, 0), BETA_PATH);
    // Only the add of the home reserved a name (beta); the plain start none.
    let slugs = t.s.app().await.slugs;
    assert_eq!(slugs.len(), 1, "{slugs:?}");
    assert_eq!(slugs[0].slug, "beta");
}
