//! A home that adopts an existing folder (design §8.4, `adopt_path`): the
//! `path` of `POST /api/projects/{id}/homes` reaches the station in the
//! request, a path that is not absolute is refused, and a station without
//! `fleet_git.homes_adopt.v1` is refused.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::Arc;

use common::fleet_git_fake::{
    FakeGit, FakeOptions, REMOTE, Reply, Setup, expect, setup, spawn_sender,
};
use fleetdeck_core::fleet_git_digest::ensure_home_digest;
use fleetdeck_deck::store::AuditResult;
use fleetdeck_proto::StationId;
use fleetdeck_proto::fleet_git::{EnsureHomeParams, GitHomeState};
use fleetdeck_proto::version::feature;
use serde_json::{Value, json};

const ADOPTED: &str = "/home/you/projects/app";

struct Rig {
    s: Setup,
    beta: StationId,
    beta_fake: Arc<FakeGit>,
    _stop: tokio::sync::watch::Sender<bool>,
}

/// The app project with a remote, a home on `fake`, a second station `beta`
/// and the sender running.
async fn rig() -> Rig {
    let mut s = setup().await;
    let r = s.save(json!({}), Some(REMOTE)).await;
    assert_eq!(r.status, 200, "{}", r.body);
    let _alpha = FakeGit::connect(&mut s.f, "fake").await;
    let (beta, beta_fake) = FakeGit::connect_second(&mut s.f, "beta", FakeOptions::default()).await;
    let both = [s.f.station.clone(), beta.clone()];
    s.f.wait("the station infos", |f| {
        let (store, both) = (f.store.clone(), both.clone());
        async move { both.iter().all(|i| store.station_info(i).is_some()) }
    })
    .await;
    let (stop, _task) = spawn_sender(&s.f, true);
    Rig {
        s,
        beta,
        beta_fake,
        _stop: stop,
    }
}

impl Rig {
    async fn add(&self, body: Value) -> common::board::Http {
        self.s
            .c
            .post(
                &format!("/api/projects/{}/homes", self.s.app.project_id),
                &self.s.h(),
                body,
            )
            .await
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
}

#[tokio::test]
async fn the_path_is_sent_to_the_station_in_the_request_and_audited() {
    let r = rig().await;
    *r.beta_fake.ensure.lock().unwrap() = Reply::ready(ADOPTED);
    // The path is trimmed.
    let added = r
        .add(json!({ "station": r.beta.as_str(), "command_id": "c1", "path": format!("  {ADOPTED} ") }))
        .await;
    assert_eq!(added.status, 202, "{}", added.body);
    let sent = r.beta_fake.wait_for("project.ensure_home", 1).await;
    assert_eq!(sent[0]["adopt_path"], ADOPTED);
    // The digest covers the path.
    let params: EnsureHomeParams = serde_json::from_value(sent[0].clone()).unwrap();
    assert_eq!(ensure_home_digest(&params), params.request_digest);
    r.wait_beta(GitHomeState::Ready).await;
    let rows = r.s.c.audited("project.home_add", AuditResult::Ok).await;
    assert_eq!(rows.len(), 1);
    assert!(
        rows[0]
            .as_deref()
            .unwrap()
            .ends_with(&format!(" path {ADOPTED}")),
        "{rows:?}"
    );
}

#[tokio::test]
async fn an_add_without_a_path_sends_no_adopt_path() {
    let r = rig().await;
    *r.beta_fake.ensure.lock().unwrap() = Reply::ready("/srv/projects/App");
    let added = r
        .add(json!({ "station": r.beta.as_str(), "command_id": "c1" }))
        .await;
    assert_eq!(added.status, 202, "{}", added.body);
    let sent = r.beta_fake.wait_for("project.ensure_home", 1).await;
    assert!(!sent[0].as_object().unwrap().contains_key("adopt_path"));
}

#[tokio::test]
async fn a_path_that_is_not_absolute_is_invalid_path() {
    let r = rig().await;
    let long = format!("/{}", "x".repeat(4096));
    for (i, path) in [
        "",
        "   ",
        "relative/app",
        "~/app",
        "/tmp/a\u{0}b",
        long.as_str(),
    ]
    .into_iter()
    .enumerate()
    {
        let refused = r
            .add(json!({ "station": r.beta.as_str(), "command_id": format!("c{i}"), "path": path }))
            .await;
        expect(&refused, 400, "invalid_path");
        assert_eq!(refused.body["message"], "Give an absolute folder path.");
    }
    assert!(r.beta_fake.requests("project.ensure_home").is_empty());
    assert_eq!(r.s.app().await.homes.len(), 1);
}

#[tokio::test]
async fn a_station_without_the_adopt_feature_is_refused() {
    let r = rig().await;
    // The link of beta negotiated the homes feature but not the adopt one.
    r.s.f
        .store
        .set_link_features(&r.beta, 77, vec![feature::FLEET_GIT_HOMES.to_owned()]);
    let refused = r
        .add(json!({ "station": r.beta.as_str(), "command_id": "c1", "path": ADOPTED }))
        .await;
    expect(&refused, 409, "fleet_git_unsupported");
    assert_eq!(
        refused.body["message"],
        "Update this station to use an existing folder."
    );
    assert!(r.beta_fake.requests("project.ensure_home").is_empty());
    assert_eq!(r.s.app().await.homes.len(), 1);
    // A clone without a path still works on this station.
    *r.beta_fake.ensure.lock().unwrap() = Reply::ready("/srv/projects/App");
    let added = r
        .add(json!({ "station": r.beta.as_str(), "command_id": "c2" }))
        .await;
    assert_eq!(added.status, 202, "{}", added.body);
}
