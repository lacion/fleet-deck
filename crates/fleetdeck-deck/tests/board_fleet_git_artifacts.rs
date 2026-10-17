//! The deck side of the artifact share (T2.3b, FG-12, design §15.5, §15.6;
//! decisions D8-D10): the `fleet_git.artifact_authorize` handler with fake
//! stations, the artifacts file root, and the save refusal of an exposed or
//! unverified share.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
mod share_support;

use std::sync::Arc;

use common::fleet_git_fake::{FakeGit, FakeOptions, Setup, ShareReply, expect, setup_reading};
use fleetdeck_core::identity::Identity;
use fleetdeck_deck::link::runtime::STATION_SCOPES;
use fleetdeck_deck::store::stations::NewStation;
use fleetdeck_proto::StationId;
use fleetdeck_proto::fleet_git::{ArtifactAuthorization, ShareVerdict};
use fleetdeck_proto::link::Outcome;
use serde_json::{Value, json};
use share_support::{Headers, join};

const SHARE: &str = "nas1/out";

fn share(name: &str, state: &str, exposed: &[&str], path: &str) -> Value {
    json!({
        "name": name, "path": "/srv/out", "exposed": exposed,
        "exposure_state": state, "path_state": path
    })
}

fn report(machine: &str, shares: &[Value]) -> Value {
    json!({
        "state": "ok", "machine": machine, "shares": shares,
        "checked_at": 1000, "inventory_revision": 3
    })
}

fn clear_report(machine: &str) -> Value {
    report(machine, &[share("out", "clear", &[], "ok")])
}

/// The caller station `fake` (a session `s1` of the app project), and the
/// exporting station `nas` that reports the machine `nas1`.
struct Art {
    s: Setup,
    caller: Arc<FakeGit>,
    exp: Arc<FakeGit>,
    exp_id: StationId,
}

async fn art() -> Art {
    let mut s = setup_reading().await;
    let caller = FakeGit::connect(&mut s.f, "fake").await;
    let (exp_id, exp) = FakeGit::connect_second(&mut s.f, "nas", FakeOptions::default()).await;
    let both = [s.f.station.clone(), exp_id.clone()];
    s.f.wait("the station infos", |f| {
        let (store, both) = (f.store.clone(), both.clone());
        async move { both.iter().all(|i| store.station_info(i).is_some()) }
    })
    .await;
    exp.set_taildrive(clear_report("nas1"));
    exp.publish(&s.f.store, &exp_id);
    s.f.store
        .apply_snapshot(
            s.f.station.clone(),
            s.f.inc.clone(),
            0,
            vec![share_support::card("s1"), share_support::card("s2")],
        )
        .await
        .unwrap();
    Art {
        s,
        caller,
        exp,
        exp_id,
    }
}

impl Art {
    fn project(&self) -> String {
        self.s.app.project_id.clone()
    }

    /// Saves the share (and nothing else) of the app project.
    async fn save_share(&self, share: &str) -> common::board::Http {
        self.s.save(json!({ "artifact_share": share }), None).await
    }

    /// Puts session `s1` of the caller in the app project.
    async fn assign(&self) {
        self.s
            .f
            .store
            .set_session_project(
                self.s.f.station.0.clone(),
                "s1".to_owned(),
                Some(self.project()),
            )
            .await
            .unwrap();
    }

    async fn authorize(
        &self,
        from: &FakeGit,
        project: &str,
        session: &str,
        share: &str,
    ) -> Outcome {
        from.control
            .request(
                "fleet_git.artifact_authorize",
                None,
                json!({ "project_id": project, "session_id": session, "share": share }),
            )
            .await
            .expect("the link is up")
    }

    /// The share is the project's, `s1` is assigned: the usual question.
    async fn ask(&self) -> Outcome {
        self.authorize(&self.caller, &self.project(), "s1", SHARE)
            .await
    }

    fn owner(&self) -> Headers {
        vec![
            ("Cookie".to_owned(), self.s.cookie.clone()),
            ("X-CSRF-Token".to_owned(), self.s.csrf.clone()),
        ]
    }
}

fn auth(outcome: Outcome) -> ArtifactAuthorization {
    match outcome {
        Outcome::Ok(v) => serde_json::from_value(v).expect("an authorization"),
        Outcome::Err(e) => panic!("an error: {} {}", e.code, e.message),
    }
}

fn refused(outcome: Outcome, code: &str, message: &str) {
    match outcome {
        Outcome::Err(e) => assert_eq!((e.code.as_str(), e.message.as_str()), (code, message)),
        Outcome::Ok(v) => panic!("an answer: {v}"),
    }
}

// ----- fleet_git.artifact_authorize -----

#[tokio::test]
async fn the_exporting_station_gives_an_authorization_that_lasts_ten_minutes() {
    let t = art().await;
    assert_eq!(t.save_share(SHARE).await.status, 200);
    t.assign().await;
    let before = t.s.f.store.now_ms();
    let a = auth(t.ask().await);
    assert_eq!(a.verdict, ShareVerdict::Ok);
    assert_eq!(a.share, SHARE);
    assert_eq!(a.exporting_station, Some(t.exp_id.clone()));
    assert_eq!((a.inventory_revision, a.checked_at), (Some(7), Some(1234)));
    assert!(a.exposed.is_empty());
    assert!(a.issued_at >= before && a.issued_at <= t.s.f.store.now_ms());
    assert_eq!(a.expires_at - a.issued_at, 600_000);
    // The deck asked the exporting station with the share name, once.
    let asked = t.exp.requests("fleet_git.share_verdict");
    assert_eq!(asked, [json!({ "share_name": "out" })]);
    // The caller was not asked.
    assert!(t.caller.requests("fleet_git.share_verdict").is_empty());
    // Every question is a new request: it never reuses an answer.
    let _ = t.ask().await;
    assert_eq!(t.exp.requests("fleet_git.share_verdict").len(), 2);
}

#[tokio::test]
async fn the_verdict_maps_exposure_path_and_unknown_fail_closed() {
    let t = art().await;
    assert_eq!(t.save_share(SHARE).await.status, 200);
    t.assign().await;
    let set = |v: Value| *t.exp.share_verdict.lock().unwrap() = ShareReply::Answer(v);
    let answer = |path: &str, state: &str, exposed: &[&str]| {
        json!({ "inventory_revision": 9, "checked_at": 55, "path_state": path,
                "exposure_state": state, "exposed": exposed })
    };
    // Exposed, with the list that the exporting station gave.
    set(answer("ok", "exposed", &["ssh", "claude_home"]));
    let a = auth(t.ask().await);
    assert_eq!(a.verdict, ShareVerdict::Exposed);
    assert_eq!(a.exposed.len(), 2);
    assert_eq!(a.inventory_revision, Some(9));
    // A non-empty list is an exposure, whatever the state says.
    set(answer("ok", "clear", &["aws"]));
    assert_eq!(auth(t.ask().await).verdict, ShareVerdict::Exposed);
    // An exposed state with an empty list is an exposure too.
    set(answer("ok", "exposed", &[]));
    assert_eq!(auth(t.ask().await).verdict, ShareVerdict::Exposed);
    // A share that is not listed, an ambiguous path, or an unknown exposure.
    for (path, state) in [
        ("missing", "unknown"),
        ("ambiguous", "clear"),
        ("unknown", "clear"),
        ("ok", "unknown"),
    ] {
        set(answer(path, state, &[]));
        let a = auth(t.ask().await);
        assert_eq!(a.verdict, ShareVerdict::Unknown, "{path} {state}");
    }
    // An unknown wire value decodes as unknown.
    set(
        json!({ "inventory_revision": 1, "checked_at": 1, "path_state": "new",
                "exposure_state": "clear", "exposed": [] }),
    );
    assert_eq!(auth(t.ask().await).verdict, ShareVerdict::Unknown);
}

#[tokio::test]
async fn a_machine_that_is_not_a_station_is_not_a_station() {
    let t = art().await;
    // No station reports the machine `nas9`: no refusal at save time.
    assert_eq!(t.save_share("nas9/out").await.status, 200);
    t.assign().await;
    let a = auth(t.authorize(&t.caller, &t.project(), "s1", "nas9/out").await);
    assert_eq!(a.verdict, ShareVerdict::NotAStation);
    assert_eq!(a.exporting_station, None);
    assert_eq!((a.inventory_revision, a.checked_at), (None, None));
    assert!(a.exposed.is_empty());
    assert_eq!(a.expires_at - a.issued_at, 600_000);
    assert!(t.exp.requests("fleet_git.share_verdict").is_empty());
}

#[tokio::test]
async fn two_stations_with_the_same_machine_are_unverified() {
    let mut t = art().await;
    let (other, other_fake) =
        FakeGit::connect_second(&mut t.s.f, "nas-two", FakeOptions::default()).await;
    other_fake.set_taildrive(clear_report("nas1"));
    other_fake.publish(&t.s.f.store, &other);
    assert_eq!(t.save_share(SHARE).await.status, 200);
    t.assign().await;
    refused(
        t.ask().await,
        "share_unverified",
        "More than one station reports the machine nas1.",
    );
    assert!(t.exp.requests("fleet_git.share_verdict").is_empty());
    assert!(other_fake.requests("fleet_git.share_verdict").is_empty());
}

#[tokio::test]
async fn an_exporting_station_that_cannot_be_checked_is_unverified() {
    let t = art().await;
    assert_eq!(t.save_share(SHARE).await.status, 200);
    t.assign().await;
    let text = "The station that exports the share cannot be checked now.";
    // A link without `fleet_git.homes.v1`.
    t.s.f.store.set_link_features(&t.exp_id, 99, vec![]);
    refused(t.ask().await, "share_unverified", text);
    assert!(t.exp.requests("fleet_git.share_verdict").is_empty());
}

#[tokio::test]
async fn a_verdict_error_and_a_missing_link_are_unverified() {
    let t = art().await;
    assert_eq!(t.save_share(SHARE).await.status, 200);
    t.assign().await;
    let text = "The station that exports the share cannot be checked now.";
    *t.exp.share_verdict.lock().unwrap() = ShareReply::Error("internal");
    refused(t.ask().await, "share_unverified", text);
    assert_eq!(t.exp.requests("fleet_git.share_verdict").len(), 1);
    // The exporting station is known from its report but has no link.
    let identity = Identity::generate().unwrap();
    let rec =
        t.s.f
            .store
            .pin_station(NewStation {
                key: identity.public_key(),
                name: "ghost".to_owned(),
                scopes: STATION_SCOPES.into_iter().collect(),
                dial_url: None,
            })
            .await
            .unwrap();
    let info = serde_json::from_value(json!({
        "name": "ghost", "incarnation": "g", "head": 0, "live_sessions": 0,
        "taildrive": clear_report("away")
    }))
    .unwrap();
    t.s.f.store.set_station_info(&rec.station_id, info);
    assert_eq!(t.save_share("away/out").await.status, 200);
    refused(
        t.authorize(&t.caller, &t.project(), "s1", "away/out").await,
        "share_unverified",
        text,
    );
}

#[tokio::test]
async fn a_verdict_that_does_not_arrive_in_five_seconds_is_unverified() {
    let t = art().await;
    assert_eq!(t.save_share(SHARE).await.status, 200);
    t.assign().await;
    *t.exp.share_verdict.lock().unwrap() = ShareReply::Silent;
    let started = std::time::Instant::now();
    refused(
        t.ask().await,
        "share_unverified",
        "The station that exports the share cannot be checked now.",
    );
    let waited = started.elapsed();
    assert!(
        waited >= std::time::Duration::from_secs(5) && waited < std::time::Duration::from_secs(9),
        "{waited:?}"
    );
}

#[tokio::test]
async fn the_authorize_question_is_checked_in_order() {
    let t = art().await;
    assert_eq!(t.save_share(SHARE).await.status, 200);
    t.assign().await;
    let project = t.project();
    // Malformed params: the serde text.
    let bad = t
        .caller
        .control
        .request(
            "fleet_git.artifact_authorize",
            None,
            json!({ "project_id": project }),
        )
        .await
        .unwrap();
    match bad {
        Outcome::Err(e) => {
            assert_eq!(e.code, "invalid_params");
            assert!(e.message.contains("session_id"), "{}", e.message);
        }
        Outcome::Ok(v) => panic!("{v}"),
    }
    // An unknown project comes before the share check.
    refused(
        t.authorize(&t.caller, "nope", "s1", "other/share").await,
        "invalid_params",
        "Unknown project.",
    );
    // Another share than the one of the project.
    refused(
        t.authorize(&t.caller, &project, "s1", "nas1/other").await,
        "invalid_params",
        "The share is not the artifact share of the project.",
    );
    // The session: unknown, not of the project, or of another station.
    let no_session = "The session does not belong to this station and project.";
    refused(
        t.authorize(&t.caller, &project, "nope", SHARE).await,
        "invalid_params",
        no_session,
    );
    refused(
        t.authorize(&t.caller, &project, "s2", SHARE).await,
        "invalid_params",
        no_session,
    );
    refused(
        t.authorize(&t.exp, &project, "s1", SHARE).await,
        "invalid_params",
        no_session,
    );
    // The project keeps no share: any share is "not the artifact share".
    t.s.save(json!({}), None).await;
    refused(
        t.ask().await,
        "invalid_params",
        "The share is not the artifact share of the project.",
    );
    // Nothing reached the exporting station.
    assert!(t.exp.requests("fleet_git.share_verdict").is_empty());
}

// ----- the artifacts file root (§15.6) -----

#[tokio::test]
async fn the_artifacts_root_is_sent_as_the_artifacts_wire_form() {
    let t = art().await;
    let h = t.owner();
    let h: Vec<(&str, &str)> = h.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    let r =
        t.s.c
            .get(
                "/api/stations/fake/files/list?artifacts=s1&path=reports",
                &h,
            )
            .await;
    assert_eq!(r.status, 200, "{}", r.body);
    let got = t.caller.requests("files.list");
    assert_eq!(
        got,
        [json!({ "root": { "artifacts": "s1" }, "path": "reports" })]
    );
    // Read and search take the root too.
    for path in [
        "/api/stations/fake/files/read?artifacts=s1&path=a.txt",
        "/api/stations/fake/files/search?artifacts=s1&q=a",
    ] {
        let r = t.s.c.get(path, &h).await;
        // The fake does not serve these: the deck reports the station error,
        // not an invalid root.
        assert_ne!(r.code(), "invalid_root", "{path}: {}", r.body);
    }
    let read = t.caller.requests("files.read");
    assert_eq!(read[0]["root"], json!({ "artifacts": "s1" }));
}

#[tokio::test]
async fn exactly_one_root_and_the_turn_diff_takes_no_artifacts_root() {
    let t = art().await;
    let h = t.owner();
    let h: Vec<(&str, &str)> = h.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    for query in [
        "session=s1&artifacts=s1",
        "workspace=%2Fp&artifacts=s1",
        "session=s1&workspace=%2Fp&artifacts=s1",
        "artifacts=",
        "path=x",
    ] {
        let r =
            t.s.c
                .get(&format!("/api/stations/fake/files/list?{query}"), &h)
                .await;
        expect(&r, 400, "invalid_root");
    }
    assert_eq!(
        t.s.c
            .get("/api/stations/fake/files/list?artifacts=s1&session=s1", &h)
            .await
            .body["message"],
        "name exactly one root: `session`, `workspace` or `artifacts`"
    );
    let r =
        t.s.c
            .get(
                "/api/stations/fake/sessions/s1/turns/t1/diff?artifacts=s1",
                &h,
            )
            .await;
    expect(&r, 400, "invalid_root");
    assert!(t.caller.requests("files.list").is_empty());
}

#[tokio::test]
async fn a_station_error_of_the_artifacts_root_is_a_409_with_its_own_text() {
    let t = art().await;
    let h = t.owner();
    let h: Vec<(&str, &str)> = h.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    for (code, message) in [
        (
            "artifacts_unavailable",
            "The artifact folder cannot be opened safely now.",
        ),
        (
            "share_exposed",
            "The artifact share exposes protected folders.",
        ),
        (
            "share_unverified",
            "Fleet Deck cannot check the artifact share now.",
        ),
    ] {
        *t.caller.files.lock().unwrap() = Err((code, message));
        let r =
            t.s.c
                .get("/api/stations/fake/files/list?artifacts=s1", &h)
                .await;
        expect(&r, 409, code);
        assert_eq!(r.body["message"], message, "{}", r.body);
    }
}

#[tokio::test]
async fn a_station_without_the_homes_feature_refuses_the_artifacts_root_only() {
    let t = art().await;
    let h = t.owner();
    let h: Vec<(&str, &str)> = h.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    t.s.f.store.set_link_features(&t.s.f.station, 99, vec![]);
    let r =
        t.s.c
            .get("/api/stations/fake/files/list?artifacts=s1", &h)
            .await;
    expect(&r, 409, "fleet_git_unsupported");
    assert_eq!(
        r.body["message"],
        "This station does not support Fleet Git. Update it first."
    );
    assert!(t.caller.requests("files.list").is_empty());
    // The session root still goes through.
    let r =
        t.s.c
            .get("/api/stations/fake/files/list?session=s1", &h)
            .await;
    assert_eq!(r.status, 200, "{}", r.body);
}

#[tokio::test]
async fn a_member_lists_the_artifacts_of_its_shared_session_only() {
    let t = art().await;
    let owner = t.owner();
    let st = t.s.f.station.0.clone();
    let (m, _, _) = join(
        &t.s.c,
        &owner,
        json!({ "kind": "session", "target": format!("{st}/s1"), "role": "viewer" }),
        "Ann",
    )
    .await;
    let m: Vec<(&str, &str)> = m.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    let r =
        t.s.c
            .get("/api/stations/fake/files/list?artifacts=s1", &m)
            .await;
    assert_eq!(r.status, 200, "{}", r.body);
    // The call carries the member flag, so the station hides the deny list.
    let got = t.caller.requests("files.list");
    assert_eq!(got[0]["root"], json!({ "artifacts": "s1" }));
    assert_eq!(got[0][fleetdeck_proto::member_deny::MEMBER_PARAM], true);
    // A session that is not shared with the member is unknown to it.
    let r =
        t.s.c
            .get("/api/stations/fake/files/list?artifacts=s2", &m)
            .await;
    expect(&r, 404, "unknown_session");
    // A workspace root is refused; two roots are an invalid root.
    let r =
        t.s.c
            .get("/api/stations/fake/files/list?workspace=%2Fp%2Fapp", &m)
            .await;
    expect(&r, 403, "not_shared");
    let r =
        t.s.c
            .get(
                "/api/stations/fake/files/list?workspace=%2Fp%2Fapp&artifacts=s1",
                &m,
            )
            .await;
    expect(&r, 400, "invalid_root");
    let r =
        t.s.c
            .get("/api/stations/fake/files/list?session=s1&artifacts=s1", &m)
            .await;
    expect(&r, 400, "invalid_root");
    assert_eq!(t.caller.requests("files.list").len(), 1);
}

// ----- the save refusal (decision D8) -----

#[tokio::test]
async fn an_exposed_share_is_refused_at_save_with_its_labels() {
    let t = art().await;
    t.exp.set_taildrive(report(
        "nas1",
        &[share("out", "exposed", &["ssh", "project_home"], "ok")],
    ));
    t.exp.publish(&t.s.f.store, &t.exp_id);
    let r = t.save_share(SHARE).await;
    expect(&r, 400, "share_exposed");
    assert_eq!(r.body["field"], "artifact_share");
    assert_eq!(
        r.body["message"],
        "This share exposes ~/.ssh, a project home. Pick another share."
    );
    assert_eq!(t.s.app().await.git_revision, 0, "nothing was saved");
    // The machine is compared after lower-casing both.
    t.exp.set_taildrive(report(
        "NAS1",
        &[share("out", "exposed", &["member_deny"], "ok")],
    ));
    t.exp.publish(&t.s.f.store, &t.exp_id);
    let r = t.save_share(SHARE).await;
    expect(&r, 400, "share_exposed");
    assert_eq!(
        r.body["message"],
        "This share exposes a member-hidden path. Pick another share."
    );
}

#[tokio::test]
async fn a_share_that_cannot_be_checked_is_refused_at_save() {
    let t = art().await;
    let text = "Fleet Deck cannot check this share now. Pick another share, or fix the warning on the Stations page.";
    let cases = [
        // The share is not in the report.
        report("nas1", &[share("other", "clear", &[], "ok")]),
        // The path is not ok.
        report("nas1", &[share("out", "clear", &[], "ambiguous")]),
        // The exposure is unknown.
        report("nas1", &[share("out", "unknown", &[], "ok")]),
        // The report itself is not ok.
        json!({ "state": "unavailable", "reason": "off", "machine": "nas1",
                "shares": [], "checked_at": 1, "inventory_revision": 1 }),
    ];
    for (n, case) in cases.into_iter().enumerate() {
        t.exp.set_taildrive(case);
        t.exp.publish(&t.s.f.store, &t.exp_id);
        let r = t.save_share(SHARE).await;
        expect(&r, 400, "share_unverified");
        assert_eq!(r.body["field"], "artifact_share", "case {n}");
        assert_eq!(r.body["message"], text, "case {n}");
    }
    assert_eq!(t.s.app().await.git_revision, 0);
}

#[tokio::test]
async fn no_matching_station_or_report_is_no_refusal_and_a_clear_share_saves() {
    let t = art().await;
    // A clear share saves.
    let r = t.save_share(SHARE).await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(t.s.app().await.git.artifact_share.as_deref(), Some(SHARE));
    // Another machine: nothing reports it (a hint only).
    let t = art().await;
    t.exp
        .set_taildrive(report("nas1", &[share("out", "exposed", &["ssh"], "ok")]));
    t.exp.publish(&t.s.f.store, &t.exp_id);
    assert_eq!(t.save_share("other/out").await.status, 200);
    // A matching machine on a station without a Taildrive report.
    let t = art().await;
    t.exp.set_info("taildrive", Value::Null);
    t.exp.publish(&t.s.f.store, &t.exp_id);
    let station = t.s.f.store.station_info(&t.exp_id).unwrap().0;
    assert!(station.taildrive.is_none());
    assert_eq!(t.save_share(SHARE).await.status, 200);
}

#[tokio::test]
async fn the_share_is_checked_only_when_it_changes_and_after_the_other_rules() {
    let t = art().await;
    assert_eq!(t.save_share(SHARE).await.status, 200);
    // The share becomes exposed: saving another setting keeps the same share.
    t.exp
        .set_taildrive(report("nas1", &[share("out", "exposed", &["aws"], "ok")]));
    t.exp.publish(&t.s.f.store, &t.exp_id);
    let r =
        t.s.save(
            json!({ "artifact_share": SHARE, "check_cmd": "make check" }),
            None,
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    // A new share on the same exposed report is checked, and the check comes
    // before the revision: a stale revision still gets the share refusal.
    t.exp.set_taildrive(report(
        "nas1",
        &[
            share("out", "exposed", &["aws"], "ok"),
            share("more", "exposed", &["gnupg"], "ok"),
        ],
    ));
    t.exp.publish(&t.s.f.store, &t.exp_id);
    let stale =
        t.s.put(
            &t.project(),
            common::fleet_git_fake::body(json!({ "artifact_share": "nas1/more" }), 99, None),
        )
        .await;
    expect(&stale, 400, "share_exposed");
    assert_eq!(
        stale.body["message"],
        "This share exposes ~/.gnupg. Pick another share."
    );
    // Another rule of the settings comes first.
    let r =
        t.s.save(
            json!({ "artifact_share": "nas1/more", "push": "on_turn" }),
            None,
        )
        .await;
    common::fleet_git_fake::invalid(&r, "push", "Turn on sync first.");
    // An unknown project is a 404.
    let r =
        t.s.put(
            "no-such-project",
            common::fleet_git_fake::body(json!({ "artifact_share": "nas1/more" }), 0, None),
        )
        .await;
    expect(&r, 404, "unknown_project");
}
