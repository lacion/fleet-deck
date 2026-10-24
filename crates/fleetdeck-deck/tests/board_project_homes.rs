//! The homes of a project on the deck (T2.3a, design §8.4, §8.9): add, retry,
//! remove and the change of the remote through the board routes; the sender
//! and the poller of the home attempts; the fence; the local-remote rule.
//! Fake stations answer the link (`project.ensure_home`,
//! `project.home_status`, `fleet_git.projects_set`).

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::Arc;

use common::Fixture;
use common::fleet_git_fake::{
    FakeGit, FakeOptions, REMOTE, Reply, Setup, bearer, expect, input, setup, setup_in,
    spawn_sender,
};
use fleetdeck_core::fleet_git_digest::ensure_home_digest;
use fleetdeck_deck::store::AuditResult;
use fleetdeck_proto::StationId;
use fleetdeck_proto::fleet_git::{EnsureHomeParams, GitHomeState};
use fleetdeck_proto::link::Scope;
use serde_json::json;
use tokio::sync::watch;
use tokio::task::JoinHandle;

const BETA_PATH: &str = "/srv/projects/App";

struct Rig {
    s: Setup,
    alpha: Arc<FakeGit>,
    beta: StationId,
    beta_fake: Arc<FakeGit>,
    _stop: watch::Sender<bool>,
    _task: JoinHandle<()>,
}

async fn infos(f: &Fixture, ids: &[StationId]) {
    let ids = ids.to_vec();
    f.wait("the station infos", |f| {
        let (store, ids) = (f.store.clone(), ids.clone());
        async move { ids.iter().all(|i| store.station_info(i).is_some()) }
    })
    .await;
}

/// The project `App` (remote set) with a home on `fake`, a second station
/// `beta`, and the sender running.
async fn rig_of(mut s: Setup, options: FakeOptions, sync: bool) -> Rig {
    let r = s.save(json!({ "sync": sync }), Some(REMOTE)).await;
    assert_eq!(r.status, 200, "{}", r.body);
    let alpha = FakeGit::connect_with(&mut s.f, "fake", options).await;
    let (beta, beta_fake) = FakeGit::connect_second(&mut s.f, "beta", options).await;
    let both = [s.f.station.clone(), beta.clone()];
    infos(&s.f, &both).await;
    let (stop, task) = spawn_sender(&s.f, true);
    Rig {
        s,
        alpha,
        beta,
        beta_fake,
        _stop: stop,
        _task: task,
    }
}

async fn rig(sync: bool) -> Rig {
    rig_of(setup().await, FakeOptions::default(), sync).await
}

impl Rig {
    fn path(&self, tail: &str) -> String {
        format!("/api/projects/{}{tail}", self.s.app.project_id)
    }

    async fn add(&self, station: &str, command: &str) -> common::board::Http {
        self.s
            .c
            .post(
                &self.path("/homes"),
                &self.s.h(),
                json!({ "station": station, "command_id": command }),
            )
            .await
    }

    async fn add_beta(&self, command: &str) -> common::board::Http {
        self.add(self.beta.as_str(), command).await
    }

    async fn retry(&self, station: &str, command: &str) -> common::board::Http {
        self.s
            .c
            .post(
                &self.path(&format!("/homes/{station}/retry")),
                &self.s.h(),
                json!({ "command_id": command }),
            )
            .await
    }

    async fn remove(&self, station: &str) -> common::board::Http {
        self.s
            .c
            .req(
                "DELETE",
                &self.path(&format!("/homes/{station}")),
                &self.s.h(),
                None,
            )
            .await
    }

    /// The home of the app project on `station`.
    async fn home(&self, station: &StationId) -> fleetdeck_proto::fleet_git::ProjectHome {
        self.s
            .app()
            .await
            .homes
            .into_iter()
            .find(|h| h.station_id == *station)
            .expect("a home")
    }

    async fn wait_home(&self, station: &StationId, state: GitHomeState) {
        let (store, id, station) = (
            self.s.f.store.clone(),
            self.s.app.project_id.clone(),
            station.clone(),
        );
        self.s
            .f
            .wait(&format!("home {state:?}"), move |_| {
                let (store, id, station) = (store.clone(), id.clone(), station.clone());
                async move {
                    let p = store.project(id).await.unwrap().unwrap();
                    p.homes
                        .iter()
                        .any(|h| h.station_id == station && h.state == state)
                }
            })
            .await;
    }
}

fn state(h: &fleetdeck_proto::fleet_git::ProjectHome) -> String {
    serde_json::to_value(h.state)
        .unwrap()
        .as_str()
        .unwrap()
        .to_owned()
}

/// The `send_attempts` and the state of an attempt.
async fn attempt(f: &Fixture, id: &str) -> (i64, String) {
    let id = id.to_owned();
    f.store
        .db()
        .call(move |conn| {
            conn.query_row(
                "SELECT send_attempts, state FROM home_attempts WHERE attempt_id = ?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map_err(Into::into)
        })
        .await
        .unwrap()
}

// ----- add -----

#[tokio::test]
async fn an_added_home_clones_through_the_sender_and_the_poller_and_becomes_ready() {
    let r = rig(false).await;
    *r.beta_fake.ensure.lock().unwrap() = Reply::cloning(BETA_PATH);
    *r.beta_fake.status.lock().unwrap() = Reply::ready(BETA_PATH);
    let added = r.add_beta("c1").await;
    assert_eq!(added.status, 202, "{}", added.body);
    assert_eq!(added.body["generation"], 1);
    let attempt_id = added.body["attempt_id"].as_str().unwrap().to_owned();
    assert!(attempt_id.starts_with("ha-"), "{attempt_id}");

    // The sender sends the stored request once; the poller moves it to ready.
    let sent = r.beta_fake.wait_for("project.ensure_home", 1).await;
    let params: EnsureHomeParams = serde_json::from_value(sent[0].clone()).unwrap();
    assert_eq!(
        (
            params.project_id.as_str(),
            params.folder.as_str(),
            params.url.as_str()
        ),
        (r.s.app.project_id.as_str(), "App", REMOTE)
    );
    assert_eq!(
        (
            params.remote_name.as_str(),
            params.generation,
            params.retry_of.clone()
        ),
        ("origin", 1, None)
    );
    assert_eq!(params.attempt_id, attempt_id);
    assert_eq!(params.remote.repo, "github.com/acme/app");
    assert_eq!(params.request_digest, ensure_home_digest(&params));
    r.wait_home(&r.beta, GitHomeState::Ready).await;
    let h = r.home(&r.beta).await;
    assert_eq!(
        (h.path.as_deref(), h.position, h.generation, h.code.clone()),
        (Some(BETA_PATH), 1, 1, None)
    );
    assert_eq!(h.attempt_id.as_deref(), Some(attempt_id.as_str()));
    assert_eq!(attempt(&r.s.f, &attempt_id).await.1, "ready");
    // The primary home and the copy stay.
    let p = r.s.app().await;
    assert_eq!(
        (p.station_id.as_str(), p.path.as_str()),
        (r.s.f.station.as_str(), "/work/app")
    );
    assert_eq!(p.homes.len(), 2);
    // The station gets a snapshot that lists the home with its generation.
    r.s.f
        .wait("the home in the snapshot of beta", |_| {
            let fake = Arc::clone(&r.beta_fake);
            async move {
                fake.snapshots()
                    .last()
                    .is_some_and(|s| s.homes.iter().any(|h| h.path == BETA_PATH))
            }
        })
        .await;
    let last = r.beta_fake.snapshots().pop().unwrap();
    assert_eq!(last.homes.len(), 1);
    assert_eq!(
        (
            last.homes[0].generation,
            last.homes[0].path.as_str(),
            last.homes[0].sync
        ),
        (1, BETA_PATH, false)
    );
    assert_eq!(
        r.s.c
            .audited("project.home_add", AuditResult::Ok)
            .await
            .len(),
        1
    );
}

#[tokio::test]
async fn a_repeated_command_id_returns_the_first_answer_and_another_request_is_refused() {
    let r = rig(false).await;
    let first = r.add_beta("c1").await;
    assert_eq!(first.status, 202, "{}", first.body);
    let again = r.add_beta("c1").await;
    assert_eq!((again.status, &again.body), (202, &first.body));
    r.beta_fake.wait_for("project.ensure_home", 1).await;
    assert_eq!(r.beta_fake.requests("project.ensure_home").len(), 1);
    assert_eq!(r.s.app().await.homes.len(), 2, "one home only");
    // The same id for another station, another route or another project.
    let other = r.add(r.s.f.station.as_str(), "c1").await;
    expect(&other, 409, "command_id_reused");
    assert_eq!(
        other.body["message"],
        "This command id was used for another request."
    );
    let route = r.retry(r.beta.as_str(), "c1").await;
    expect(&route, 409, "command_id_reused");
    let twin =
        r.s.f
            .store
            .create_project(r.s.f.station.0.clone(), input("Twin", "/work/twin"))
            .await
            .unwrap();
    let r2 =
        r.s.c
            .post(
                &format!("/api/projects/{}/homes", twin.project_id),
                &r.s.h(),
                json!({ "station": r.beta.as_str(), "command_id": "c1" }),
            )
            .await;
    expect(&r2, 409, "command_id_reused");
}

#[tokio::test]
async fn adding_a_home_is_for_the_owner_login_with_a_csrf_token_and_is_audited() {
    let r = rig(false).await;
    let path = r.path("/homes");
    let body = json!({ "station": r.beta.as_str(), "command_id": "c1" });
    let admin = bearer(&r.s.c, "admin", &[Scope::Read, Scope::Admin]).await;
    let admin = [(admin.0.as_str(), admin.1.as_str())];
    let denied = r.s.c.post(&path, &admin, body.clone()).await;
    expect(&denied, 403, "owner_only");
    let no_csrf = [("Cookie", r.s.cookie.as_str())];
    assert_eq!(r.s.c.post(&path, &no_csrf, body.clone()).await.status, 403);
    assert_eq!(r.s.c.post(&path, &[], body.clone()).await.status, 401);
    assert_eq!(r.s.app().await.homes.len(), 1);
    // Refusals are audited with their code; the success with its station.
    let refused = r.add("nope", "c2").await;
    expect(&refused, 404, "unknown_station");
    assert_eq!(refused.body["message"], "Unknown station.");
    let ok = r.add_beta("c3").await;
    assert_eq!(ok.status, 202, "{}", ok.body);
    let refused_rows =
        r.s.c
            .audited("project.home_add", AuditResult::Refused)
            .await;
    assert_eq!(refused_rows, [Some("unknown_station".to_owned())]);
    let ok_rows = r.s.c.audited("project.home_add", AuditResult::Ok).await;
    assert_eq!(ok_rows.len(), 1);
    assert!(ok_rows[0].as_deref().unwrap().contains("generation 1"));
}

#[tokio::test]
async fn the_add_checks_run_in_order_with_their_codes_and_texts() {
    let r = rig(false).await;
    // Unknown project, bad command id, bad body.
    let bad =
        r.s.c
            .post(
                "/api/projects/nope/homes",
                &r.s.h(),
                json!({ "station": "x", "command_id": "c" }),
            )
            .await;
    expect(&bad, 404, "unknown_project");
    assert_eq!(bad.body["message"], "Unknown project.");
    expect(&r.add_beta("bad id!").await, 400, "invalid_command_id");
    let body =
        r.s.c
            .post(&r.path("/homes"), &r.s.h(), json!({ "station": "x" }))
            .await;
    expect(&body, 400, "invalid_body");
    // 2. The station has a home already.
    let exists = r.add(r.s.f.station.as_str(), "c1").await;
    expect(&exists, 409, "home_exists");
    assert_eq!(
        exists.body["message"],
        "This station already has a home of the project."
    );
    // 3. The station is not connected: a paired station with no link.
    let ghost =
        r.s.f
            .store
            .pin_station(fleetdeck_deck::store::stations::NewStation {
                key: fleetdeck_core::identity::Identity::generate()
                    .unwrap()
                    .public_key(),
                name: "ghost".into(),
                scopes: Default::default(),
                dial_url: None,
            })
            .await
            .unwrap()
            .station_id;
    let offline = r.add(ghost.as_str(), "c2").await;
    expect(&offline, 409, "station_offline");
    assert_eq!(offline.body["message"], "The station is not connected.");
    // 4. A link without `fleet_git.homes.v1`.
    r.s.f.store.set_link_features(&ghost, 1, vec![]);
    let old = r.add(ghost.as_str(), "c3").await;
    expect(&old, 409, "fleet_git_unsupported");
    assert_eq!(
        old.body["message"],
        "This station does not support Fleet Git. Update it first."
    );
    // 6. The link carries the feature, but the deck has no `station.info`.
    r.s.f.store.clear_link_features(&ghost, 1);
    r.s.f.store.set_link_features(
        &ghost,
        2,
        vec![fleetdeck_proto::version::feature::FLEET_GIT_HOMES.to_owned()],
    );
    let nameless = r.add(ghost.as_str(), "c4").await;
    expect(&nameless, 409, "station_info_missing");
    assert_eq!(
        nameless.body["message"],
        "The deck does not know this station's name yet. Try again in a moment."
    );
    // 7. The Git name of the station is reserved for another station.
    r.s.f
        .store
        .db()
        .call(|conn| {
            conn.execute(
                "INSERT INTO slug_reservations (repo, slug, station_id, first_used_at)
                 VALUES ('github.com/acme/app', 'beta', 'other-station', 1)",
                [],
            )
            .map(|_| ())
            .map_err(Into::into)
        })
        .await
        .unwrap();
    let taken = r.add_beta("c5").await;
    expect(&taken, 409, "slug_taken");
    assert_eq!(
        taken.body["message"],
        "The Git name beta is reserved for station other-station in github.com/acme/app. Rename this station."
    );
    assert!(r.beta_fake.requests("project.ensure_home").is_empty());
    assert_eq!(r.s.app().await.homes.len(), 1);
    // 1. A project without a remote.
    let mut no_forge = input("Bare", "/work/bare");
    no_forge.forge = None;
    let bare =
        r.s.f
            .store
            .create_project(r.s.f.station.0.clone(), no_forge)
            .await
            .unwrap();
    let needs =
        r.s.c
            .post(
                &format!("/api/projects/{}/homes", bare.project_id),
                &r.s.h(),
                json!({ "station": r.beta.as_str(), "command_id": "c6" }),
            )
            .await;
    expect(&needs, 400, "needs_remote");
    assert_eq!(
        needs.body["message"],
        "Set the remote URL of the project first."
    );
}

// ----- the sender and the poller -----

#[tokio::test]
async fn a_failed_clone_makes_the_home_failed_and_a_retry_sends_a_new_attempt_of_the_same_generation()
 {
    let r = rig(false).await;
    *r.beta_fake.ensure.lock().unwrap() = Reply::failed("clone_failed", "The clone failed: boom");
    let first = r.add_beta("c1").await;
    assert_eq!(first.status, 202, "{}", first.body);
    let old = first.body["attempt_id"].as_str().unwrap().to_owned();
    r.wait_home(&r.beta, GitHomeState::Failed).await;
    let h = r.home(&r.beta).await;
    assert_eq!(
        (h.code.as_deref(), h.message.as_deref()),
        (Some("clone_failed"), Some("The clone failed: boom"))
    );
    assert_eq!(h.reserved_path.as_deref(), Some("/srv/projects/App"));
    assert_eq!(attempt(&r.s.f, &old).await.1, "failed");
    // Only a failed home can be retried.
    let alpha = r.retry(r.s.f.station.as_str(), "c2").await;
    expect(&alpha, 409, "home_not_failed");
    assert_eq!(alpha.body["message"], "Only a failed home can be retried.");
    expect(&r.retry("nope", "c3").await, 404, "unknown_home");
    // The retry: a new attempt, the same generation, `retry_of` the old one.
    *r.beta_fake.ensure.lock().unwrap() = Reply::ready(BETA_PATH);
    let retried = r.retry(r.beta.as_str(), "c4").await;
    assert_eq!(retried.status, 202, "{}", retried.body);
    assert_eq!(retried.body["generation"], 1);
    let new = retried.body["attempt_id"].as_str().unwrap().to_owned();
    assert_ne!(new, old);
    r.wait_home(&r.beta, GitHomeState::Ready).await;
    let sent = r.beta_fake.wait_for("project.ensure_home", 2).await;
    let params: EnsureHomeParams = serde_json::from_value(sent[1].clone()).unwrap();
    assert_eq!(
        (
            params.attempt_id.clone(),
            params.retry_of.clone(),
            params.generation
        ),
        (new.clone(), Some(old), 1)
    );
    assert_eq!(params.request_digest, ensure_home_digest(&params));
    // A repeat of the command id returns the first answer.
    let again = r.retry(r.beta.as_str(), "c4").await;
    assert_eq!((again.status, &again.body), (202, &retried.body));
    assert_eq!(
        r.s.c
            .audited("project.home_retry", AuditResult::Ok)
            .await
            .len(),
        2
    );
}

#[tokio::test]
async fn a_retry_names_the_newest_attempt_that_the_station_stored_not_a_refused_one() {
    let r = rig(false).await;
    // The first attempt ends `interrupted` (the station stored it).
    *r.beta_fake.ensure.lock().unwrap() = Reply::State {
        state: "interrupted",
        path: None,
        reserved: Some(BETA_PATH.to_owned()),
        code: Some("clone_interrupted"),
        message: Some("The clone stopped.".to_owned()),
    };
    let first = r.add_beta("c1").await;
    let interrupted = first.body["attempt_id"].as_str().unwrap().to_owned();
    r.wait_home(&r.beta, GitHomeState::Failed).await;
    assert_eq!(attempt(&r.s.f, &interrupted).await.1, "interrupted");

    // The station refuses the first retry (a link error, no row there): the
    // attempt is `refused` and the home is failed again.
    *r.beta_fake.ensure.lock().unwrap() = Reply::Error("incomplete_folder");
    let refused = r.retry(r.beta.as_str(), "c2").await;
    assert_eq!(refused.status, 202, "{}", refused.body);
    let refused = refused.body["attempt_id"].as_str().unwrap().to_owned();
    r.s.f
        .wait("the refused attempt", |f| {
            let (store, id) = (f.store.clone(), refused.clone());
            async move { store.home_attempt_state(id).await.unwrap().as_deref() == Some("refused") }
        })
        .await;
    let h = r.home(&r.beta).await;
    assert_eq!(h.state, GitHomeState::Failed);
    assert_eq!(h.code.as_deref(), Some("incomplete_folder"));

    // The next retry names the interrupted attempt, not the refused one.
    *r.beta_fake.ensure.lock().unwrap() = Reply::ready(BETA_PATH);
    let again = r.retry(r.beta.as_str(), "c3").await;
    assert_eq!(again.status, 202, "{}", again.body);
    r.wait_home(&r.beta, GitHomeState::Ready).await;
    let sent = r.beta_fake.wait_for("project.ensure_home", 3).await;
    let params: EnsureHomeParams = serde_json::from_value(sent[2].clone()).unwrap();
    assert_eq!(params.retry_of, Some(interrupted));
    assert_eq!(params.request_digest, ensure_home_digest(&params));
}

#[tokio::test]
async fn an_error_that_ends_the_attempt_fails_the_home_and_another_error_waits_with_a_backoff() {
    let r = rig(false).await;
    *r.beta_fake.ensure.lock().unwrap() = Reply::Error("home_conflict");
    let added = r.add_beta("c1").await;
    let id = added.body["attempt_id"].as_str().unwrap().to_owned();
    r.wait_home(&r.beta, GitHomeState::Failed).await;
    let h = r.home(&r.beta).await;
    assert_eq!(h.code.as_deref(), Some("home_conflict"));
    assert_eq!(h.message.as_deref(), Some("the fake says no"));
    // A link error: the station stored no row, so the attempt is `refused`.
    assert_eq!(attempt(&r.s.f, &id).await.1, "refused");

    // An error of the station that does not end the attempt: `sent` again
    // with the backoff.
    *r.beta_fake.ensure.lock().unwrap() = Reply::Error("internal");
    let retried = r.retry(r.beta.as_str(), "c2").await;
    let id = retried.body["attempt_id"].as_str().unwrap().to_owned();
    r.beta_fake.wait_for("project.ensure_home", 2).await;
    r.s.f
        .wait("the backoff", |f| {
            let (store, id) = (f.store.clone(), id.clone());
            async move {
                store
                    .db()
                    .call(move |conn| {
                        conn.query_row(
                            "SELECT send_attempts >= 1 AND state = 'sent' AND next_send_at > 0
                             FROM home_attempts WHERE attempt_id = ?1",
                            [id],
                            |r| r.get(0),
                        )
                        .map_err(Into::into)
                    })
                    .await
                    .unwrap()
            }
        })
        .await;
    assert_eq!(state(&r.home(&r.beta).await), "cloning");
    // The station answers at the next send, after the backoff of 2 s.
    *r.beta_fake.ensure.lock().unwrap() = Reply::ready(BETA_PATH);
    r.s.f.det.advance(std::time::Duration::from_secs(3));
    r.s.f.ctx.homes_kick.kick();
    r.wait_home(&r.beta, GitHomeState::Ready).await;
}

#[tokio::test]
async fn a_station_that_lost_the_attempt_gets_the_stored_request_again() {
    let r = rig(false).await;
    // The first answer is `cloning`; the poll then finds no attempt.
    let added = r.add_beta("c1").await;
    let id = added.body["attempt_id"].as_str().unwrap().to_owned();
    r.beta_fake.wait_for("project.ensure_home", 1).await;
    r.beta_fake.attempts.lock().unwrap().clear();
    *r.beta_fake.ensure.lock().unwrap() = Reply::ready(BETA_PATH);
    // A new link start polls the cloning home once.
    fleetdeck_deck::link::fleet_git_sync::sync_station(&r.s.f.ctx, r.beta.clone()).await;
    r.wait_home(&r.beta, GitHomeState::Ready).await;
    let sent = r.beta_fake.requests("project.ensure_home");
    assert_eq!(sent.len(), 2, "the same request twice");
    assert_eq!(sent[0], sent[1], "the stored request is sent unchanged");
    assert_eq!(attempt(&r.s.f, &id).await.1, "ready");
}

#[tokio::test]
async fn a_late_answer_of_an_old_generation_is_dropped_by_the_fence() {
    let r = rig(false).await;
    *r.beta_fake.ensure.lock().unwrap() = Reply::failed("clone_failed", "boom");
    let first = r.add_beta("c1").await;
    let old = first.body["attempt_id"].as_str().unwrap().to_owned();
    r.wait_home(&r.beta, GitHomeState::Failed).await;
    // Remove the home, then add it again: generation 2.
    assert_eq!(r.remove(r.beta.as_str()).await.status, 204);
    assert_eq!(attempt(&r.s.f, &old).await.1, "superseded");
    *r.beta_fake.ensure.lock().unwrap() = Reply::cloning(BETA_PATH);
    *r.beta_fake.status.lock().unwrap() = Reply::cloning(BETA_PATH);
    let second = r.add_beta("c2").await;
    assert_eq!(second.body["generation"], 2, "{}", second.body);
    r.beta_fake.wait_for("project.ensure_home", 2).await;
    r.wait_home(&r.beta, GitHomeState::Cloning).await;
    // The old attempt answers late with `ready`: dropped, nothing changes.
    let late = fleetdeck_proto::fleet_git::HomeStatus {
        attempt_id: old,
        generation: 1,
        state: fleetdeck_proto::fleet_git::HomeAttemptState::Ready,
        path: Some("/old/path".into()),
        reserved_path: None,
        code: None,
        message: None,
    };
    let applied = r.s.f.store.home_status_apply(late).await.unwrap();
    assert_eq!(
        applied,
        fleetdeck_deck::store::home_attempts::Applied::Stale
    );
    let h = r.home(&r.beta).await;
    assert_eq!(
        (state(&h).as_str(), h.generation, h.path.clone()),
        ("cloning", 2, None)
    );
}

#[tokio::test]
async fn a_sent_attempt_survives_a_restart_and_is_sent_once() {
    // M5: the route commits while the sender sends nothing; a sender that
    // starts later sends the stored request once.
    let dir = tempfile::tempdir().unwrap();
    let mut s = setup_in(dir.path()).await;
    let r = s.save(json!({}), Some(REMOTE)).await;
    assert_eq!(r.status, 200, "{}", r.body);
    let options = FakeOptions::default();
    let _alpha = FakeGit::connect_with(&mut s.f, "fake", options).await;
    let (beta, beta_fake) = FakeGit::connect_second(&mut s.f, "beta", options).await;
    infos(&s.f, &[s.f.station.clone(), beta.clone()]).await;
    *beta_fake.ensure.lock().unwrap() = Reply::ready(BETA_PATH);

    let (stop, task) = spawn_sender(&s.f, false);
    let added =
        s.c.post(
            &format!("/api/projects/{}/homes", s.app.project_id),
            &s.h(),
            json!({ "station": beta.as_str(), "command_id": "c1" }),
        )
        .await;
    assert_eq!(added.status, 202, "{}", added.body);
    let id = added.body["attempt_id"].as_str().unwrap().to_owned();
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    assert!(
        beta_fake.requests("project.ensure_home").is_empty(),
        "nothing sent"
    );
    assert_eq!(attempt(&s.f, &id).await.1, "sent");
    stop.send(true).unwrap();
    task.await.unwrap();

    let (_stop, _task) = spawn_sender(&s.f, true);
    s.f.wait("the home is ready", |f| {
        let (store, pid, beta) = (f.store.clone(), s.app.project_id.clone(), beta.clone());
        async move {
            let p = store.project(pid).await.unwrap().unwrap();
            p.homes
                .iter()
                .any(|h| h.station_id == beta && h.state == GitHomeState::Ready)
        }
    })
    .await;
    assert_eq!(
        beta_fake.requests("project.ensure_home").len(),
        1,
        "one send"
    );
    assert_eq!(beta_fake.attempts.lock().unwrap().len(), 1);
    assert!(beta_fake.attempts.lock().unwrap().contains_key(&id));
    // A repeat of the route after a lost answer returns the stored answer.
    let again =
        s.c.post(
            &format!("/api/projects/{}/homes", s.app.project_id),
            &s.h(),
            json!({ "station": beta.as_str(), "command_id": "c1" }),
        )
        .await;
    assert_eq!(
        (again.status, again.body["attempt_id"].as_str()),
        (202, Some(id.as_str()))
    );
}

#[tokio::test]
async fn a_ready_home_of_a_sync_project_owns_its_folder_and_the_station_checks_it() {
    let r = rig(true).await;
    r.wait_home(&r.s.f.station, GitHomeState::Ready).await;
    *r.beta_fake.ensure.lock().unwrap() = Reply::ready(BETA_PATH);
    let added = r.add_beta("c1").await;
    assert_eq!(added.status, 202, "{}", added.body);
    r.wait_home(&r.beta, GitHomeState::Ready).await;
    let h = r.home(&r.beta).await;
    assert!(h.sync_owner, "{h:?}");
    // The snapshot to beta lists the project with its home generation.
    r.s.f
        .wait("the project in the snapshot of beta", |_| {
            let fake = Arc::clone(&r.beta_fake);
            async move {
                fake.snapshots()
                    .last()
                    .is_some_and(|s| s.projects.iter().any(|p| p.home_path == BETA_PATH))
            }
        })
        .await;
    let snapshot = r.beta_fake.snapshots().pop().unwrap();
    assert_eq!(snapshot.projects[0].home_generation, 1);
    assert!(snapshot.homes[0].sync);
    let _ = &r.alpha;
}

// ----- remove -----

#[tokio::test]
async fn a_home_is_removed_unless_it_is_the_last_busy_or_the_only_ready_primary() {
    let r = rig(false).await;
    let last = r.remove(r.s.f.station.as_str()).await;
    expect(&last, 409, "last_home");
    assert_eq!(last.body["message"], "A project needs one home.");
    expect(&r.remove("nope").await, 404, "unknown_home");

    // A clone that runs makes the home busy.
    *r.beta_fake.ensure.lock().unwrap() = Reply::cloning(BETA_PATH);
    assert_eq!(r.add_beta("c1").await.status, 202);
    r.wait_home(&r.beta, GitHomeState::Cloning).await;
    let busy = r.remove(r.beta.as_str()).await;
    expect(&busy, 409, "home_busy");
    assert_eq!(busy.body["message"], "A clone runs for this home.");
    // The primary needs another ready home: beta is only cloning.
    let primary = r.remove(r.s.f.station.as_str()).await;
    expect(&primary, 409, "no_ready_home");
    assert_eq!(
        primary.body["message"],
        "Another home must be ready before you remove the first home."
    );
    assert_eq!(r.s.app().await.homes.len(), 2);
}

#[tokio::test]
async fn removing_the_primary_promotes_the_ready_home_and_a_live_session_makes_a_home_busy() {
    let r = rig(false).await;
    *r.beta_fake.ensure.lock().unwrap() = Reply::ready(BETA_PATH);
    assert_eq!(r.add_beta("c1").await.status, 202);
    r.wait_home(&r.beta, GitHomeState::Ready).await;
    // A live session in the primary home.
    let mut card = fleetdeck_proto::SessionSummary::new("s1".into());
    card.cwd = Some("/work/app/src".to_owned());
    r.s.f
        .store
        .apply_snapshot(r.s.f.station.clone(), r.s.f.inc.clone(), 0, vec![card])
        .await
        .unwrap();
    let busy = r.remove(r.s.f.station.as_str()).await;
    expect(&busy, 409, "home_busy");
    assert_eq!(busy.body["message"], "A session runs in this home.");
    // The session ends: the removal promotes beta and updates the copy.
    let mut ended = fleetdeck_proto::SessionSummary::new("s1".into());
    ended.cwd = Some("/work/app/src".to_owned());
    ended.state = fleetdeck_proto::event::SessionState::Exited;
    r.s.f
        .store
        .apply_snapshot(r.s.f.station.clone(), r.s.f.inc.clone(), 0, vec![ended])
        .await
        .unwrap();
    let removed = r.remove(r.s.f.station.as_str()).await;
    assert_eq!(removed.status, 204, "{}", removed.body);
    let p = r.s.app().await;
    assert_eq!(p.homes.len(), 1);
    assert_eq!(
        (p.homes[0].station_id.clone(), p.homes[0].position),
        (r.beta.clone(), 0)
    );
    assert_eq!(
        (p.station_id.as_str(), p.path.as_str()),
        (r.beta.as_str(), BETA_PATH)
    );
    // The removed station gets a snapshot without the home; no file is
    // deleted (the deck never calls the station for it).
    r.alpha.wait_for("fleet_git.projects_set", 2).await;
    let last = r.alpha.snapshots().pop().unwrap();
    assert!(last.homes.is_empty(), "{last:?}");
    assert_eq!(
        r.s.c
            .audited("project.home_remove", AuditResult::Ok)
            .await
            .len(),
        1
    );
}
