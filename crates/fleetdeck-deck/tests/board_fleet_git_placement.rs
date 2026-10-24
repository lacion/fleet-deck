//! Placement by station traits and the drift reference (T2.3b, FG-6, FG-11;
//! design §9, §14.3, decisions D1-D5, D6, D7): `POST /api/placement`,
//! `home = "auto"` in the batch start, `assign auto` with `requires` and
//! `prefer`, and the prefs routes.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use common::fleet_git_fake::{
    Check, FakeGit, FakeOptions, Reply, Setup, bearer, expect, setup, spawn_sender,
};
use fleetdeck_deck::store::AuditResult;
use fleetdeck_proto::StationId;
use fleetdeck_proto::fleet_git::GitHomeState;
use fleetdeck_proto::link::Scope;
use serde_json::{Value, json};

static NEXT: AtomicU32 = AtomicU32::new(1);
const BETA_PATH: &str = "/srv/projects/App";
const GAMMA_PATH: &str = "/srv/gamma/App";

/// Three stations: `fake` (A, `gpu`, linux x86_64), `beta` (B, `always_on`,
/// macos aarch64) and `gamma` (C, no traits), all with a signed-in `claude`.
struct Three {
    s: Setup,
    alpha: Arc<FakeGit>,
    beta: StationId,
    beta_fake: Arc<FakeGit>,
    gamma: StationId,
    gamma_fake: Arc<FakeGit>,
    _stop: tokio::sync::watch::Sender<bool>,
}

async fn three() -> Three {
    let mut s = setup().await;
    let alpha = FakeGit::connect(&mut s.f, "fake").await;
    let (beta, beta_fake) = FakeGit::connect_second(&mut s.f, "beta", FakeOptions::default()).await;
    let (gamma, gamma_fake) =
        FakeGit::connect_second(&mut s.f, "gamma", FakeOptions::default()).await;
    let all = [s.f.station.clone(), beta.clone(), gamma.clone()];
    s.f.wait("the station infos", |f| {
        let (store, all) = (f.store.clone(), all.clone());
        async move { all.iter().all(|i| store.station_info(i).is_some()) }
    })
    .await;
    alpha.set_traits("linux", "x86_64", None, &["gpu"]);
    beta_fake.set_traits("macos", "aarch64", Some(false), &["always_on"]);
    for (fake, id) in [
        (&alpha, &s.f.station),
        (&beta_fake, &beta),
        (&gamma_fake, &gamma),
    ] {
        fake.set_signed_in();
        fake.publish(&s.f.store, id);
    }
    let r = s
        .save(json!({}), Some(common::fleet_git_fake::REMOTE))
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    let (stop, _task) = spawn_sender(&s.f, true);
    Three {
        s,
        alpha,
        beta,
        beta_fake,
        gamma,
        gamma_fake,
        _stop: stop,
    }
}

impl Three {
    async fn place(&self, body: Value) -> common::board::Http {
        self.s.c.post("/api/placement", &self.s.h(), body).await
    }

    fn names(&self, r: &common::board::Http) -> Vec<String> {
        assert_eq!(r.status, 200, "{}", r.body);
        r.body["stations"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| {
                let id = v.as_str().unwrap();
                if id == self.s.f.station.as_str() {
                    "alpha"
                } else if id == self.beta.as_str() {
                    "beta"
                } else if id == self.gamma.as_str() {
                    "gamma"
                } else {
                    "other"
                }
                .to_owned()
            })
            .collect()
    }

    /// Adds the home of the app project on `station` with `reply`, and waits
    /// until it reads `state`.
    async fn add_home(
        &self,
        station: &StationId,
        fake: &FakeGit,
        reply: Reply,
        state: GitHomeState,
    ) {
        *fake.ensure.lock().unwrap() = reply;
        let added = self
            .s
            .c
            .post(
                &format!("/api/projects/{}/homes", self.s.app.project_id),
                &self.s.h(),
                json!({ "station": station.as_str(), "command_id": format!("add-{}", NEXT.fetch_add(1, Ordering::Relaxed)) }),
            )
            .await;
        assert_eq!(added.status, 202, "{}", added.body);
        let (store, id, station) = (
            self.s.f.store.clone(),
            self.s.app.project_id.clone(),
            station.clone(),
        );
        self.s
            .f
            .wait("the home", move |_| {
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

fn auto(id: &str) -> Value {
    json!({
        "batch_id": id, "issues": ["#7"], "provider_instance": "claude",
        "count_per_issue": 1, "home": "auto"
    })
}

// ----- POST /api/placement (A2 FG-6) -----

#[tokio::test]
async fn placement_follows_the_requirements_and_the_preferences() {
    let t = three().await;
    let r = t.place(json!({ "count": 1, "requires": ["gpu"] })).await;
    assert_eq!(t.names(&r), ["alpha"]);
    let r = t
        .place(json!({ "count": 1, "prefer": ["always_on"] }))
        .await;
    assert_eq!(t.names(&r), ["beta"]);
    let r = t
        .place(json!({ "count": 1, "requires": ["os:macos"] }))
        .await;
    assert_eq!(t.names(&r), ["beta"]);
    let r = t
        .place(json!({ "count": 1, "requires": ["arch:x86_64"] }))
        .await;
    assert_eq!(t.names(&r), ["alpha"]);
    // `not_on_battery` is satisfied by beta only.
    let r = t
        .place(json!({ "count": 1, "prefer": ["not_on_battery"] }))
        .await;
    assert_eq!(t.names(&r), ["beta"]);
    // `spread` with 3 sessions uses 3 stations.
    let r = t
        .place(json!({ "count": 3, "spread": true, "prefer": ["always_on"] }))
        .await;
    let mut names = t.names(&r);
    assert_eq!(names[0], "beta");
    names.sort();
    assert_eq!(names, ["alpha", "beta", "gamma"]);
    // Without `spread` each placement counts as one more live session.
    let r = t
        .place(json!({ "count": 3, "prefer": ["always_on"] }))
        .await;
    assert_eq!(t.names(&r), ["beta", "beta", "beta"]);
    // Nothing is audited and nothing is sent to a station.
    assert!(
        t.s.c
            .audited("placement.place", AuditResult::Ok)
            .await
            .is_empty()
    );
}

#[tokio::test]
async fn placement_refuses_bad_bodies_and_says_why_no_station_fits() {
    let t = three().await;
    let invalid = |r: &common::board::Http, message: &str| {
        expect(r, 400, "invalid_body");
        assert_eq!(r.body["message"], message, "{}", r.body);
    };
    for count in [0, 21] {
        let r = t.place(json!({ "count": count })).await;
        invalid(&r, "Count is 1 to 20.");
    }
    let r = t
        .place(json!({ "count": 1, "requires": ["Bad Label"] }))
        .await;
    invalid(
        &r,
        "A station label is 1 to 32 characters: a-z, 0-9, _ or -.",
    );
    let r = t
        .place(json!({ "count": 1, "prefer": ["idle", "idle"] }))
        .await;
    invalid(&r, "Each preference at most once.");
    let r = t.place(json!({ "count": 1, "extra": true })).await;
    expect(&r, 400, "invalid_body");
    let r = t.place(json!({ "requires": [] })).await;
    expect(&r, 400, "invalid_body");
    let r = t
        .place(json!({ "count": 1, "project_id": "no-such-project" }))
        .await;
    expect(&r, 404, "unknown_project");
    assert_eq!(r.body["message"], "Unknown project.");

    let no = |r: &common::board::Http, message: &str| {
        expect(r, 409, "no_station");
        assert_eq!(r.body["message"], message, "{}", r.body);
    };
    let r = t.place(json!({ "count": 1, "requires": ["tpu"] })).await;
    no(&r, "No station matches: tpu.");
    let r = t
        .place(json!({ "count": 1, "requires": ["gpu", "always_on"] }))
        .await;
    no(&r, "No station matches: no station has every requirement.");
    let r = t.place(json!({ "count": 1, "provider": "codex" })).await;
    no(&r, "No station matches: no station is free.");
}

#[tokio::test]
async fn placement_for_a_project_uses_its_ready_homes_only() {
    let t = three().await;
    let project = t.s.app.project_id.clone();
    // The only home (alpha) is ready; beta would be better but has no home.
    let r = t
        .place(json!({ "count": 2, "prefer": ["always_on"], "project_id": project }))
        .await;
    assert_eq!(t.names(&r), ["alpha", "alpha"]);
    // A cloning home does not count.
    t.add_home(
        &t.beta,
        &t.beta_fake,
        Reply::cloning(BETA_PATH),
        GitHomeState::Cloning,
    )
    .await;
    let r = t
        .place(json!({ "count": 1, "prefer": ["always_on"], "project_id": project }))
        .await;
    assert_eq!(t.names(&r), ["alpha"]);
    // A ready home does.
    *t.beta_fake.status.lock().unwrap() = Reply::ready(BETA_PATH);
    // Poll the cloning home now, not at the next 10 s poll (the wait is 10 s).
    t.s.f.ctx.homes_kick.link_started(t.beta.as_str());
    let (store, id, beta) = (
        t.s.f.store.clone(),
        t.s.app.project_id.clone(),
        t.beta.clone(),
    );
    t.s.f
        .wait("beta ready", move |_| {
            let (store, id, beta) = (store.clone(), id.clone(), beta.clone());
            async move {
                let p = store.project(id).await.unwrap().unwrap();
                p.homes
                    .iter()
                    .any(|h| h.station_id == beta && h.state == GitHomeState::Ready)
            }
        })
        .await;
    let r = t
        .place(json!({ "count": 1, "prefer": ["always_on"], "project_id": project }))
        .await;
    assert_eq!(t.names(&r), ["beta"]);
}

#[tokio::test]
async fn a_project_without_a_ready_home_has_no_station() {
    let t = three().await;
    // Sync on and a mismatching check: the only home is unverified.
    *t.alpha.check.lock().unwrap() = Check::Mismatch;
    let r =
        t.s.save(
            json!({ "sync": true }),
            Some(common::fleet_git_fake::REMOTE),
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    t.s.home("unverified").await;
    let r = t
        .place(json!({ "count": 1, "project_id": t.s.app.project_id }))
        .await;
    expect(&r, 409, "no_station");
    assert_eq!(
        r.body["message"],
        "No station matches: the project has no ready home."
    );
}

#[tokio::test]
async fn placement_needs_the_drive_scope() {
    let t = three().await;
    let read = bearer(&t.s.c, "reader", &[Scope::Read]).await;
    let read = [(read.0.as_str(), read.1.as_str())];
    let r =
        t.s.c
            .post("/api/placement", &read, json!({ "count": 1 }))
            .await;
    assert_eq!(r.status, 403, "{}", r.body);
    let drive = bearer(&t.s.c, "driver", &[Scope::Read, Scope::Drive]).await;
    let drive = [(drive.0.as_str(), drive.1.as_str())];
    let r =
        t.s.c
            .post("/api/placement", &drive, json!({ "count": 1 }))
            .await;
    assert_eq!(r.status, 200, "{}", r.body);
    // No login at all.
    let r =
        t.s.c
            .post("/api/placement", &[], json!({ "count": 1 }))
            .await;
    assert_eq!(r.status, 401, "{}", r.body);
}

// ----- home = "auto" in the batch start (decision D4) -----

fn cwd_of(fake: &FakeGit, n: usize) -> String {
    match &fake.starts()[n].command {
        fleetdeck_proto::command::SessionCommand::Start { spec, .. } => spec.cwd.clone(),
        other => panic!("not a start: {other:?}"),
    }
}

#[tokio::test]
async fn an_auto_batch_goes_to_the_placed_station_and_its_home_path() {
    let t = three().await;
    t.add_home(
        &t.beta,
        &t.beta_fake,
        Reply::ready(BETA_PATH),
        GitHomeState::Ready,
    )
    .await;
    // Both homes are ready; the preference picks beta.
    let mut body = auto("a1");
    body["prefer"] = json!(["always_on"]);
    let r = t.batch(body).await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body["failed"], json!([]), "{}", r.body);
    assert_eq!(t.beta_fake.requests("issues.get").len(), 1);
    assert!(t.alpha.requests("issues.get").is_empty());
    assert_eq!(t.beta_fake.starts().len(), 1);
    assert!(t.alpha.starts().is_empty());
    assert_eq!(cwd_of(&t.beta_fake, 0), BETA_PATH);
    // A requirement that only alpha meets.
    let mut body = auto("a2");
    body["requires"] = json!(["gpu"]);
    let r = t.batch(body).await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(t.alpha.starts().len(), 1);
    assert_eq!(cwd_of(&t.alpha, 0), "/work/app");
    // The station of a third home would be placed too, but gamma has none.
    assert!(t.gamma_fake.starts().is_empty());
    let _ = GAMMA_PATH;
}

#[tokio::test]
async fn an_auto_batch_with_spread_starts_one_session_on_each_station() {
    let t = three().await;
    t.add_home(
        &t.beta,
        &t.beta_fake,
        Reply::ready(BETA_PATH),
        GitHomeState::Ready,
    )
    .await;
    let mut body = auto("s1");
    body["count_per_issue"] = json!(2);
    body["spread"] = json!(true);
    body["prefer"] = json!(["always_on"]);
    let r = t.batch(body).await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body["failed"], json!([]), "{}", r.body);
    assert_eq!(r.body["started"].as_array().unwrap().len(), 2);
    // The first placed station (beta) answers the one read.
    assert_eq!(t.beta_fake.requests("issues.get").len(), 1);
    assert!(t.alpha.requests("issues.get").is_empty());
    assert_eq!(t.beta_fake.starts().len(), 1);
    assert_eq!(t.alpha.starts().len(), 1);
    assert_eq!(cwd_of(&t.beta_fake, 0), BETA_PATH);
    assert_eq!(cwd_of(&t.alpha, 0), "/work/app");
    // Without `spread` the best station takes both (one more live session
    // each time, but beta still sorts first with the same count).
    let mut body = auto("s2");
    body["count_per_issue"] = json!(2);
    body["prefer"] = json!(["always_on"]);
    let r = t.batch(body).await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(t.beta_fake.starts().len(), 3);
    assert_eq!(t.alpha.starts().len(), 1);
}

#[tokio::test]
async fn an_auto_batch_refuses_when_no_station_fits_and_sends_nothing() {
    let t = three().await;
    let mut body = auto("n1");
    body["requires"] = json!(["tpu"]);
    let r = t.batch(body).await;
    expect(&r, 409, "no_station");
    assert_eq!(r.body["message"], "No station matches: tpu.");
    // A provider instance that no station has signed in.
    let mut body = auto("n2");
    body["provider_instance"] = json!("codex");
    let r = t.batch(body).await;
    expect(&r, 409, "no_station");
    assert_eq!(r.body["message"], "No station matches: no station is free.");
    // A cloning home is not a candidate: the only ready home is alpha, which
    // lacks the label.
    t.add_home(
        &t.beta,
        &t.beta_fake,
        Reply::cloning(BETA_PATH),
        GitHomeState::Cloning,
    )
    .await;
    let mut body = auto("n3");
    body["requires"] = json!(["always_on"]);
    let r = t.batch(body).await;
    expect(&r, 409, "no_station");
    assert_eq!(r.body["message"], "No station matches: always_on.");
    // Bad placement fields are the usual 400 of the batch.
    let mut body = auto("n4");
    body["requires"] = json!(["Bad Label"]);
    let r = t.batch(body).await;
    expect(&r, 400, "invalid_params");
    for fake in [&t.alpha, &t.beta_fake, &t.gamma_fake] {
        assert!(fake.requests("issues.get").is_empty());
        assert!(fake.starts().is_empty());
    }
}

#[tokio::test]
async fn an_auto_batch_skips_a_station_without_the_provider_instance() {
    let t = three().await;
    t.add_home(
        &t.beta,
        &t.beta_fake,
        Reply::ready(BETA_PATH),
        GitHomeState::Ready,
    )
    .await;
    // Beta signs out: only alpha can take the start.
    t.beta_fake.set_info(
        "providers",
        json!([{ "instance": "claude", "provider": "claude", "version": "2.1.0",
                 "auth": "signed_out" }]),
    );
    t.beta_fake.publish(&t.s.f.store, &t.beta);
    let mut body = auto("p1");
    body["prefer"] = json!(["always_on"]);
    let r = t.batch(body).await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(t.alpha.starts().len(), 1);
    assert!(t.beta_fake.starts().is_empty());
}

// ----- assign auto (decision D2) -----

#[tokio::test]
async fn assign_names_the_spawn_station_by_the_placement_order() {
    let t = three().await;
    let assign = |body: Value| {
        let c = &t.s.c;
        let h = t.s.h();
        async move { c.post("/api/assign", &h, body).await }
    };
    // No session fits: the best station for a new session, by `prefer`.
    let r = assign(json!({ "command_id": "as1", "text": "x", "prefer": ["always_on"] })).await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body["outcome"], "unrouted");
    assert_eq!(r.body["spawn_on"]["name"], "beta", "{}", r.body);
    let r = assign(json!({ "command_id": "as2", "text": "x", "requires": ["gpu"] })).await;
    assert_eq!(r.body["spawn_on"]["name"], "fake", "{}", r.body);
    // A requirement that no station meets: no spawn station.
    let r = assign(json!({ "command_id": "as3", "text": "x", "requires": ["tpu"] })).await;
    assert_eq!(r.body["outcome"], "unrouted");
    assert!(r.body["spawn_on"].is_null(), "{}", r.body);
    // Bad placement fields are refused and audited as refused.
    let r = assign(json!({ "command_id": "as4", "text": "x", "requires": ["Bad Label"] })).await;
    expect(&r, 400, "invalid_body");
    assert_eq!(
        r.body["message"],
        "A station label is 1 to 32 characters: a-z, 0-9, _ or -."
    );
    let r = assign(json!({ "command_id": "as5", "text": "x", "prefer": ["idle", "idle"] })).await;
    expect(&r, 400, "invalid_body");
    let refused = t.s.c.audited("session.assign", AuditResult::Refused).await;
    assert_eq!(refused.len(), 2, "{refused:?}");
}

// ----- the prefs routes (FG-11, decisions D6, D7) -----

#[tokio::test]
async fn the_drift_reference_is_set_read_and_cleared_by_the_owner() {
    let t = three().await;
    let get = || async { t.s.c.get("/api/fleet-git/prefs", &t.s.h()).await };
    let put = |body: Value| {
        let c = &t.s.c;
        let h = t.s.h();
        async move {
            c.req("PUT", "/api/fleet-git/drift-reference", &h, Some(body))
                .await
        }
    };
    let r = get().await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body, json!({ "drift_reference": null }));
    let r = put(json!({ "station": t.beta.as_str() })).await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body, json!({ "drift_reference": t.beta.as_str() }));
    assert_eq!(get().await.body, r.body);
    // An unknown station is refused and the reference stays.
    let r = put(json!({ "station": "nope" })).await;
    expect(&r, 404, "unknown_station");
    assert_eq!(r.body["message"], "Unknown station.");
    // A station name is not an id.
    let r = put(json!({ "station": "beta" })).await;
    expect(&r, 404, "unknown_station");
    let r = put(json!({ "station": 3 })).await;
    expect(&r, 400, "invalid_body");
    let r = put(json!({ "station": null, "extra": 1 })).await;
    expect(&r, 400, "invalid_body");
    assert_eq!(get().await.body["drift_reference"], t.beta.as_str());
    // `null` clears.
    let r = put(json!({ "station": null })).await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body, json!({ "drift_reference": null }));
    // Audit: the station id, or `none`; refusals carry the code.
    let ok = t.s.c.audited("drift.reference", AuditResult::Ok).await;
    assert_eq!(ok.len(), 2, "{ok:?}");
    let refused = t.s.c.audited("drift.reference", AuditResult::Refused).await;
    assert_eq!(refused.len(), 4, "{refused:?}");
}

#[tokio::test]
async fn a_stored_reference_of_a_station_that_is_gone_reads_as_null() {
    let t = three().await;
    t.s.f
        .store
        .set_fleet_git_drift_reference(Some("ghost".to_owned()))
        .await
        .unwrap();
    let r = t.s.c.get("/api/fleet-git/prefs", &t.s.h()).await;
    assert_eq!(r.body, json!({ "drift_reference": null }));
    // The row stays.
    assert_eq!(
        t.s.f.store.fleet_git_drift_reference().await.unwrap(),
        Some("ghost".to_owned())
    );
}

#[tokio::test]
async fn the_prefs_need_read_and_the_reference_is_the_owners_alone() {
    let t = three().await;
    let read = bearer(&t.s.c, "reader", &[Scope::Read]).await;
    let read = [(read.0.as_str(), read.1.as_str())];
    let r = t.s.c.get("/api/fleet-git/prefs", &read).await;
    assert_eq!(r.status, 200, "{}", r.body);
    let admin = bearer(&t.s.c, "admin", &[Scope::Read, Scope::Admin]).await;
    let admin = [(admin.0.as_str(), admin.1.as_str())];
    let r =
        t.s.c
            .req(
                "PUT",
                "/api/fleet-git/drift-reference",
                &admin,
                Some(json!({ "station": t.beta.as_str() })),
            )
            .await;
    expect(&r, 403, "owner_only");
    // No CSRF token with the cookie login.
    let no_csrf = [("Cookie", t.s.cookie.as_str())];
    let r =
        t.s.c
            .req(
                "PUT",
                "/api/fleet-git/drift-reference",
                &no_csrf,
                Some(json!({ "station": null })),
            )
            .await;
    assert_eq!(r.status, 403, "{}", r.body);
    assert_eq!(t.s.f.store.fleet_git_drift_reference().await.unwrap(), None);
}
