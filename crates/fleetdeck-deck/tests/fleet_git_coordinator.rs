//! The architect across the fleet, deck side (T4.4a, design §13.1, §13.2,
//! §13.3; FG-10): the branches and fetch routes of the board, the coordinator
//! queries `project_branches` and `project_fetch` (class `inspect`, fenced
//! Git text, the target rule) and the op `land` (class `manage`, the stored
//! authority). A fake station answers `project.branches`, `project.fetch`
//! and the land methods.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Duration;

use common::fleet::{HOSTILE, bound, bound_edit, call, one_fence, query, refusal};
use common::fleet_git_fake::{
    FakeGit, REMOTE, Setup, bearer, expect, input, setup_scopes, station_scopes,
};
use common::fleet_git_land::GitReply;
use fleetdeck_deck::store::{AuditResult, GrantClass};
use fleetdeck_proto::command::RETRY_WINDOW_MS;
use fleetdeck_proto::event::SessionState;
use fleetdeck_proto::fleet_git::RadarResult;
use fleetdeck_proto::link::{Outcome, Scope};
use fleetdeck_proto::mutation::{
    CoordinatorBinding, CoordinatorCall, CoordinatorCallEnvelope, CoordinatorOp, CoordinatorQuery,
};
use fleetdeck_proto::untrusted::fence;
use fleetdeck_proto::{CommandId, Receipt, RejectCode, SessionSummary};
use serde_json::{Value, json};

fn session(id: &str, state: SessionState) -> SessionSummary {
    let mut summary = SessionSummary::new(id.into());
    summary.state = state;
    summary.cwd = Some("/work/app".to_owned());
    summary.worktree = Some(format!("/work/app--fd-{id}"));
    summary.branch = Some(format!("fd/fake/{id}"));
    summary.callsign = Some(id.to_owned());
    summary.title = Some(format!("Title of {id} {HOSTILE}"));
    summary
}

/// A synced project (reserved Git name `fake`), the sessions `s1` and `s2`
/// (ended), `busy` (starting), and the coordinators `coord-1` and `coord-2`;
/// fleet control is on and the station may coordinate.
async fn world() -> (Setup, Arc<FakeGit>) {
    let mut scopes = station_scopes();
    scopes.insert(Scope::Coordinate);
    let mut s = setup_scopes(scopes).await;
    let r = s.save(json!({ "sync": true }), Some(REMOTE)).await;
    assert_eq!(r.status, 200, "{}", r.body);
    let fake = FakeGit::connect(&mut s.f, "fake").await;
    s.home("ready").await;
    s.f.store.set_fleet_control(true);
    let station = s.f.station.0.clone();
    s.f.store
        .db()
        .call(move |conn| {
            conn.execute(
                "INSERT INTO slug_reservations (repo, slug, station_id, first_used_at)
                 VALUES ('github.com/acme/app', 'fake', ?1, 1)",
                [station],
            )
            .map_err(Into::into)
        })
        .await
        .unwrap();
    let mut cards = vec![
        session("s1", SessionState::Exited),
        session("s2", SessionState::Exited),
        session("busy", SessionState::Starting),
    ];
    for id in ["coord-1", "coord-2"] {
        let mut c = SessionSummary::new(id.into());
        c.state = SessionState::Running;
        cards.push(c);
    }
    s.f.store
        .apply_snapshot(s.f.station.clone(), s.f.inc.clone(), 0, cards)
        .await
        .unwrap();
    (s, fake)
}

/// A second paired station that is not a home of the project.
async fn other_station(f: &common::Fixture) -> fleetdeck_proto::StationId {
    f.store
        .pin_station(fleetdeck_deck::store::stations::NewStation {
            key: fleetdeck_core::identity::Identity::generate()
                .unwrap()
                .public_key(),
            name: "beta".to_owned(),
            scopes: station_scopes(),
            dial_url: None,
        })
        .await
        .unwrap()
        .station_id
}

fn branch(name: &str, slug: Option<&str>, subject: &str) -> Value {
    let mut b = json!({
        "name": name, "callsign": name.rsplit('/').next().unwrap(),
        "head": "a".repeat(40), "ahead": 2, "behind": 1, "at": 1_000, "subject": subject
    });
    if let Some(slug) = slug {
        b["slug"] = json!(slug);
    }
    b
}

/// The station's answer: a reserved-slug branch of `s1`, one of an
/// unreserved slug, and a legacy branch.
fn listing() -> GitReply {
    GitReply::Answer(json!({
        "remote": "origin", "default_branch": "main", "default_oid": "d".repeat(40),
        "fetched_at": 4_000, "truncated": false,
        "branches": [
            branch("fd/fake/s1", Some("fake"), HOSTILE),
            branch("fd/ghost/x", Some("ghost"), "ghost"),
            branch("fd/old", None, "old"),
        ]
    }))
}

/// `s1` and `s2` will conflict on `a.rs` and `b.rs`.
async fn radar_rows(s: &Setup) {
    let result: RadarResult = serde_json::from_value(json!({
        "checked_at": 5_000, "skipped": 0,
        "pairs": [{ "a": "s1", "b": "s2", "conflict": true,
                    "files": ["b.rs", "a.rs"], "files_truncated": false }]
    }))
    .unwrap();
    let station = s.f.station.0.clone();
    let sides = BTreeMap::from([
        ("s1".to_owned(), station.clone()),
        ("s2".to_owned(), station),
    ]);
    s.f.store
        .radar_replace(s.app.project_id.clone(), result, sides)
        .await
        .unwrap();
}

/// A coordinator `coord-1` with `classes` over the fixture station.
async fn coordinator(s: &Setup, classes: &[GrantClass]) -> CoordinatorBinding {
    bound(&s.f, "coord-1", classes, true).await
}

/// `coordinator.query` from the station, over the link: the deck asks the
/// fake station and answers the value (or `{"reject": ..}`).
async fn ask(s: &Setup, fake: &FakeGit, b: &CoordinatorBinding, q: CoordinatorQuery) -> Value {
    let env = query(&s.f, b, q);
    match fake
        .control
        .request("coordinator.query", None, json!(env))
        .await
        .unwrap()
    {
        Outcome::Ok(v) => v,
        Outcome::Err(e) => panic!("{e}"),
    }
}

fn branches_query(s: &Setup) -> CoordinatorQuery {
    CoordinatorQuery::ProjectBranches {
        project_id: s.app.project_id.clone(),
    }
}

fn fetch_query(s: &Setup) -> CoordinatorQuery {
    CoordinatorQuery::ProjectFetch {
        project_id: s.app.project_id.clone(),
    }
}

fn land_op(s: &Setup, session: &str) -> CoordinatorOp {
    CoordinatorOp::Land {
        station: s.f.station.clone(),
        session_id: session.into(),
    }
}

fn refused(r: &Receipt, code: RejectCode) -> String {
    let reject = r
        .reject
        .as_ref()
        .unwrap_or_else(|| panic!("accepted: {r:?}"));
    assert_eq!(reject.code, code, "{}", reject.message);
    reject.message.clone()
}

// ----- the coordinator queries -----

#[tokio::test]
async fn project_branches_answers_the_mapped_rows_with_the_subject_fenced() {
    let (s, fake) = world().await;
    *fake.branches.lock().unwrap() = listing();
    radar_rows(&s).await;
    let b = coordinator(&s, &[GrantClass::Inspect]).await;
    let got = ask(&s, &fake, &b, branches_query(&s)).await;
    assert_eq!(
        fake.requests("project.branches"),
        [json!({ "project_id": s.app.project_id })]
    );
    assert_eq!(got["remote"], "origin");
    assert_eq!(got["fetched_at"], 4_000);
    let rows = got["branches"].as_array().unwrap();
    assert_eq!(rows.len(), 3);
    let first = &rows[0];
    assert_eq!(first["station_name"], "fake");
    assert_eq!(first["station_id"], json!(s.f.station));
    assert_eq!(first["session"]["session_id"], "s1");
    assert_eq!(first["conflict"], "will_conflict");
    assert_eq!(first["files"], json!(["a.rs", "b.rs"]));
    // The title of a session is a worker text: it is not sent.
    assert!(first["session"].get("title").is_none(), "{first}");
    // The subject is fenced once and the hostile closing tag stays inert.
    one_fence(&first["subject"]);
    let subject = first["subject"].as_str().unwrap();
    assert!(subject.contains("<\u{200B}/untrusted>"), "{subject}");
    assert_eq!(
        subject,
        fence(&format!("git:{}/fd/fake/s1", s.app.project_id), HOSTILE)
    );
    // The names stay outside the fence.
    assert_eq!(first["name"], "fd/fake/s1");
    // A slug without a reservation: no station, no session, not legacy.
    assert_eq!(rows[1]["legacy"], false);
    assert!(rows[1].get("station_id").is_none() && rows[1].get("session").is_none());
    // A legacy branch: no station, and no session (no session has the branch).
    assert_eq!(rows[2]["legacy"], true);
    assert!(rows[2].get("session").is_none());
}

#[tokio::test]
async fn project_fetch_asks_the_radar_home_and_fences_the_text_of_a_failure() {
    let (s, fake) = world().await;
    let b = coordinator(&s, &[GrantClass::Inspect]).await;
    let got = ask(&s, &fake, &b, fetch_query(&s)).await;
    assert_eq!(
        got,
        json!({ "project_id": s.app.project_id, "fetched_at": 7 })
    );
    assert_eq!(
        fake.requests("project.fetch"),
        [json!({ "project_id": s.app.project_id })]
    );
    *fake.fetch.lock().unwrap() = GitReply::Answer(json!({
        "code": "fetch_failed", "text": HOSTILE
    }));
    let got = ask(&s, &fake, &b, fetch_query(&s)).await;
    assert_eq!(got["code"], "fetch_failed");
    assert!(got.get("fetched_at").is_none());
    one_fence(&got["text"]);
    assert_eq!(
        got["text"],
        json!(fence(&format!("git:fetch/{}", s.app.project_id), HOSTILE))
    );
    // A link error of the station reaches the tool, its text fenced.
    *fake.fetch.lock().unwrap() = GitReply::Error("git_failed");
    let got = ask(&s, &fake, &b, fetch_query(&s)).await;
    assert_eq!(got["reject"]["code"], "invalid_state");
    assert!(
        got["reject"]["message"]
            .as_str()
            .unwrap()
            .starts_with("git_failed: <untrusted"),
        "{got}"
    );
}

#[tokio::test]
async fn the_queries_need_inspect_the_whole_project_in_the_grant_and_the_kill_switch_on() {
    let (s, _fake) = world().await;
    let f = &s.f;
    // Without `inspect`; a grant that does not hold the station of a home;
    // a good grant.
    let weak = bound(f, "reader", &[GrantClass::Read, GrantClass::Mail], true).await;
    let beta = other_station(f).await;
    let outside = bound_edit(f, &f.station, "outside", &[GrantClass::Inspect], |g| {
        g.stations = BTreeSet::from([beta.clone()]);
    })
    .await;
    let good = coordinator(&s, &[GrantClass::Inspect]).await;
    for q in [branches_query(&s), fetch_query(&s)] {
        let r = refusal(f, &weak, q.clone()).await;
        assert_eq!(r.code, RejectCode::Forbidden);
        assert_eq!(r.message, "the grant has no class for this query");
        let r = refusal(f, &outside, q.clone()).await;
        assert_eq!(r.code, RejectCode::Forbidden);
        assert_eq!(
            r.message,
            "a home of the project is on a station outside the grant"
        );
        // The kill switch of the deck.
        f.store.set_fleet_control(false);
        let r = refusal(f, &good, q).await;
        assert_eq!(r.code, RejectCode::Forbidden);
        assert!(r.message.starts_with("fleet_control_off:"), "{}", r.message);
        f.store.set_fleet_control(true);
    }
}

#[tokio::test]
async fn the_queries_refuse_an_unknown_project_sync_off_and_a_link_without_the_features() {
    let (s, _fake) = world().await;
    let f = &s.f;
    let b = coordinator(&s, &[GrantClass::Inspect]).await;
    let unknown = CoordinatorQuery::ProjectBranches {
        project_id: "nope".to_owned(),
    };
    let r = refusal(f, &b, unknown).await;
    assert_eq!(
        (r.code, r.message.as_str()),
        (RejectCode::UnknownRequest, "no such project")
    );
    // A project that does not sync.
    let plain = f
        .store
        .create_project(f.station.0.clone(), input("Plain", "/work/plain"))
        .await
        .unwrap();
    let r = refusal(
        f,
        &b,
        CoordinatorQuery::ProjectFetch {
            project_id: plain.project_id,
        },
    )
    .await;
    assert_eq!(r.code, RejectCode::InvalidState);
    assert_eq!(r.message, "sync_off: This project does not sync with Git.");
    // A link that did not negotiate `coordinator.v2`.
    f.store.set_link_features(&f.station, 98, vec![]);
    let r = refusal(f, &b, branches_query(&s)).await;
    assert_eq!(r.code, RejectCode::Unsupported);
    assert!(r.message.contains("coordinator.v2"), "{}", r.message);
    // A link with `coordinator.v2` and without `fleet_git.branches.v1`.
    f.store
        .set_link_features(&f.station, 98, vec!["coordinator.v2".to_owned()]);
    let r = refusal(f, &b, fetch_query(&s)).await;
    assert_eq!(r.code, RejectCode::Unsupported);
    assert!(r.message.contains("fleet_git.branches.v1"), "{}", r.message);
}

// ----- the coordinator op `land` -----

#[tokio::test]
async fn land_with_manage_queues_a_land_with_the_stored_authority_and_a_replay_repeats() {
    let (s, _fake) = world().await;
    let f = &s.f;
    let b = coordinator(&s, &[GrantClass::Manage]).await;
    let first = call(f, &b, "k1", land_op(&s, "s1")).await;
    assert!(first.reject.is_none(), "{first:?}");
    let lands = f
        .store
        .project_lands(s.app.project_id.clone())
        .await
        .unwrap();
    let queue = lands.unwrap().queue;
    assert_eq!(queue.len(), 1);
    assert_eq!(queue[0].state, "queued");
    assert_eq!(queue[0].requested_by, format!("coordinator:{}", b.grant_id));
    let row = f
        .store
        .land_row(queue[0].land_id.clone())
        .await
        .unwrap()
        .unwrap();
    let authority = row.authority.unwrap();
    assert_eq!(authority.grant_id, b.grant_id);
    assert_eq!(authority.generation, 1);
    assert_eq!(authority.caller_session.as_str(), "coord-1");
    assert_eq!(row.authority_expires_at, Some(f.store.now_ms() + 600_000));
    // The same call again: the first receipt and one row.
    let again = call(f, &b, "k1", land_op(&s, "s1")).await;
    assert_eq!(again, first);
    let lands = f
        .store
        .project_lands(s.app.project_id.clone())
        .await
        .unwrap();
    assert_eq!(lands.unwrap().queue.len(), 1);
    // A second land of the session is refused by the queue.
    let open = call(f, &b, "k2", land_op(&s, "s1")).await;
    assert!(refused(&open, RejectCode::InvalidState).starts_with("land_open: "));
}

#[tokio::test]
async fn land_refuses_the_targets_of_the_adr_008_rules_a_session_that_cannot_land_and_the_switch() {
    let (s, _fake) = world().await;
    let f = &s.f;
    let b = coordinator(&s, &[GrantClass::Manage]).await;
    // Another coordinator (a mutating grant): rule 3.
    bound(f, "coord-2", &[GrantClass::Drive], false).await;
    let r = call(f, &b, "k1", land_op(&s, "coord-2")).await;
    assert!(refused(&r, RejectCode::Forbidden).contains("never acts for another"));
    // The caller itself.
    let r = call(f, &b, "k2", land_op(&s, "coord-1")).await;
    assert!(refused(&r, RejectCode::Forbidden).contains("own session"));
    // An unknown session.
    let r = call(f, &b, "k3", land_op(&s, "ghost")).await;
    refused(&r, RejectCode::UnknownSession);
    // A session that is not idle or ended: the land request refuses it.
    let r = call(f, &b, "k4", land_op(&s, "busy")).await;
    assert!(refused(&r, RejectCode::InvalidState).starts_with("land_not_ready: "));
    // A grant that does not hold the station.
    let beta = other_station(f).await;
    let outside = bound_edit(f, &f.station, "coord-3", &[GrantClass::Manage], |g| {
        g.scope = fleetdeck_proto::command::GrantScope::Fleet;
        g.stations = BTreeSet::from([beta.clone()]);
    })
    .await;
    let r = call(f, &outside, "k5", land_op(&s, "s1")).await;
    assert!(refused(&r, RejectCode::Forbidden).contains("not on a station of the grant"));
    // The kill switch of the deck.
    f.store.set_fleet_control(false);
    let r = call(f, &b, "k6", land_op(&s, "s1")).await;
    assert!(refused(&r, RejectCode::Forbidden).starts_with("fleet_control_off:"));
    f.store.set_fleet_control(true);
    // A grant without `manage`.
    let weak = bound(f, "coord-4", &[GrantClass::Drive], true).await;
    let r = call(f, &weak, "k7", land_op(&s, "s1")).await;
    refused(&r, RejectCode::Forbidden);
    // Nothing was queued.
    let lands = f
        .store
        .project_lands(s.app.project_id.clone())
        .await
        .unwrap();
    assert!(lands.unwrap().queue.is_empty());
}

#[tokio::test]
async fn land_needs_coordinator_v2_on_the_link() {
    let (s, _fake) = world().await;
    let f = &s.f;
    let b = coordinator(&s, &[GrantClass::Manage]).await;
    f.store.set_link_features(&f.station, 98, vec![]);
    let r = call(f, &b, "k1", land_op(&s, "s1")).await;
    refused(&r, RejectCode::Unsupported);
}

async fn row_of_queue(s: &Setup) -> fleetdeck_deck::store::land_queue::LandRow {
    let lands =
        s.f.store
            .project_lands(s.app.project_id.clone())
            .await
            .unwrap();
    let id = lands.unwrap().queue.remove(0).land_id;
    s.f.store.land_row(id).await.unwrap().unwrap()
}

async fn ended(s: &Setup, land: &str) -> (String, Option<String>) {
    let row = s.f.store.land_row(land.to_owned()).await.unwrap().unwrap();
    (row.state, row.code)
}

#[tokio::test]
async fn a_renewal_a_revocation_and_the_ten_minutes_end_a_queued_land_at_admission() {
    // A renewal (a new generation of the grant).
    let (s, _fake) = world().await;
    let b = coordinator(&s, &[GrantClass::Manage]).await;
    assert!(
        call(&s.f, &b, "k1", land_op(&s, "s1"))
            .await
            .reject
            .is_none()
    );
    let land = row_of_queue(&s).await.land_id;
    let sql = "UPDATE tokens SET bound_generation = 2 WHERE grant_id = ?1";
    let id = b.grant_id.clone();
    let moved =
        s.f.store
            .db()
            .call(move |c| Ok(c.execute(sql, [id])?))
            .await;
    assert_eq!(moved.unwrap(), 1);
    s.f.store.land_admit_next().await.unwrap();
    assert_eq!(
        ended(&s, &land).await,
        ("failed".to_owned(), Some("authority_lost".to_owned()))
    );

    // A revocation.
    let (s, _fake) = world().await;
    let b = coordinator(&s, &[GrantClass::Manage]).await;
    assert!(
        call(&s.f, &b, "k1", land_op(&s, "s1"))
            .await
            .reject
            .is_none()
    );
    let land = row_of_queue(&s).await.land_id;
    let id = b.grant_id.clone();
    assert!(
        s.f.store
            .revoke_grant(id, "owner".to_owned())
            .await
            .unwrap()
    );
    s.f.store.land_admit_next().await.unwrap();
    let row = s.f.store.land_row(land).await.unwrap().unwrap();
    assert_eq!(
        (row.state.as_str(), row.code.as_deref()),
        ("failed", Some("authority_lost"))
    );
    assert!(row.message.unwrap().contains("grant_revoked"));

    // The 10 minutes of the test clock.
    let (s, _fake) = world().await;
    let b = coordinator(&s, &[GrantClass::Manage]).await;
    assert!(
        call(&s.f, &b, "k1", land_op(&s, "s1"))
            .await
            .reject
            .is_none()
    );
    let land = row_of_queue(&s).await.land_id;
    s.f.det.advance(Duration::from_millis(600_001));
    s.f.store.land_admit_next().await.unwrap();
    assert_eq!(
        ended(&s, &land).await,
        ("failed".to_owned(), Some("authority_expired".to_owned()))
    );
}

#[tokio::test]
async fn a_coordinator_land_over_the_link_wakes_admission_and_is_sent_with_its_authority() {
    let (s, fake) = world().await;
    let b = coordinator(&s, &[GrantClass::Manage]).await;
    let stop = common::fleet_git_land::spawn_land(&s.f);
    // The call comes over the link as a station sends it; the link handler
    // kicks admission after the accepted call.
    let id = CommandId::from("k1");
    let envelope = CoordinatorCallEnvelope {
        command_id: id.clone(),
        target_incarnation: s.f.store.incarnation().clone(),
        retry_until: s.f.store.now_ms() + RETRY_WINDOW_MS,
        call: CoordinatorCall {
            caller: "coord-1".into(),
            binding: b.clone(),
            idempotency_key: None,
            op: land_op(&s, "s1"),
        },
    };
    let out = fake
        .control
        .request("coordinator.call", Some(&id), json!(envelope))
        .await
        .unwrap();
    let Outcome::Ok(receipt) = out else {
        panic!("{out:?}");
    };
    assert_eq!(receipt["state"], "accepted", "{receipt}");
    let sent = fake.wait_for("session.land", 1).await;
    assert_eq!(sent[0]["origin"], "coordinator");
    assert_eq!(sent[0]["authority"]["grant_id"], b.grant_id.as_str());
    let _ = stop.send(true);
}

// ----- the board routes -----

fn path(s: &Setup, tail: &str) -> String {
    format!("/api/projects/{}/{tail}", s.app.project_id)
}

#[tokio::test]
async fn the_branches_route_maps_the_rows_and_the_fetch_route_is_audited() {
    let (s, fake) = world().await;
    *fake.branches.lock().unwrap() = listing();
    radar_rows(&s).await;
    let r = s.c.get(&path(&s, "branches"), &s.h()[..1]).await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body["default_branch"], "main");
    let rows = r.body["branches"].as_array().unwrap();
    assert_eq!(rows.len(), 3);
    // The board gets the plain subject and the title of the session.
    assert_eq!(rows[0]["subject"], HOSTILE);
    assert_eq!(rows[0]["session"]["session_id"], "s1");
    assert!(
        rows[0]["session"]["title"]
            .as_str()
            .unwrap()
            .contains("Title of s1")
    );
    assert_eq!(rows[0]["station_name"], "fake");
    assert_eq!(rows[0]["conflict"], "will_conflict");
    assert_eq!(rows[0]["files"], json!(["a.rs", "b.rs"]));
    assert_eq!(rows[1]["conflict"], "none");
    assert_eq!(rows[2]["legacy"], true);

    let r = s.c.req("POST", &path(&s, "fetch"), &s.h(), None).await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body, json!({ "fetched_at": 7 }));
    let ok = s.c.audited("project.fetch", AuditResult::Ok).await;
    assert_eq!(ok, [Some(s.f.station.to_string())]);
    // A failed fetch is an answer with a code.
    *fake.fetch.lock().unwrap() = GitReply::Answer(json!({
        "code": "fetch_failed", "text": "could not fetch"
    }));
    let r = s.c.req("POST", &path(&s, "fetch"), &s.h(), None).await;
    assert_eq!(r.body["code"], "fetch_failed");
}

#[tokio::test]
async fn the_routes_need_their_scopes_and_are_closed_to_members_and_the_errors_are_coded() {
    let (s, fake) = world().await;
    let read = bearer(&s.c, "reader", &[Scope::Read]).await;
    let drive = bearer(&s.c, "driver", &[Scope::Read, Scope::Drive]).await;
    fn h(p: &(String, String)) -> [(&str, &str); 1] {
        [(p.0.as_str(), p.1.as_str())]
    }
    let branches = path(&s, "branches");
    assert_eq!(s.c.get(&branches, &h(&read)).await.status, 200);
    let r = s.c.req("POST", &path(&s, "fetch"), &h(&read), None).await;
    expect(&r, 403, "forbidden");
    assert_eq!(
        s.c.req("POST", &path(&s, "fetch"), &h(&drive), None)
            .await
            .status,
        200
    );
    // A POST with a cookie needs the CSRF token.
    let r = s.c.req("POST", &path(&s, "fetch"), &s.h()[..1], None).await;
    assert_eq!(r.status, 403, "{}", r.body);
    // A member of a share is refused.
    let share =
        s.c.post(
            "/api/shares",
            &s.h(),
            json!({ "kind": "session", "target": "fake/s1", "role": "collaborator" }),
        )
        .await;
    assert_eq!(share.status, 200, "{}", share.body);
    let url = share.body["url"].as_str().unwrap();
    let join = url.split("#join=").nth(1).unwrap();
    let j =
        s.c.post(
            "/api/auth/join",
            &[],
            json!({ "token": join, "display_name": "Ann" }),
        )
        .await;
    assert_eq!(j.status, 200, "{}", j.body);
    let cookie = j
        .header("set-cookie")
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let csrf = j.body["csrf"].as_str().unwrap().to_owned();
    let member = [("Cookie", cookie.as_str()), ("X-CSRF-Token", csrf.as_str())];
    expect(&s.c.get(&branches, &member).await, 403, "owner_only");
    expect(
        &s.c.req("POST", &path(&s, "fetch"), &member, None).await,
        403,
        "owner_only",
    );

    // The coded refusals.
    let r = s.c.get("/api/projects/nope/branches", &s.h()[..1]).await;
    expect(&r, 404, "unknown_project");
    let plain =
        s.f.store
            .create_project(s.f.station.0.clone(), input("Plain", "/work/plain"))
            .await
            .unwrap();
    let r =
        s.c.get(
            &format!("/api/projects/{}/branches", plain.project_id),
            &s.h()[..1],
        )
        .await;
    expect(&r, 409, "sync_off");
    assert_eq!(r.body["message"], "This project does not sync with Git.");
    *fake.branches.lock().unwrap() = GitReply::Error("git_failed");
    let r = s.c.get(&branches, &s.h()[..1]).await;
    expect(&r, 502, "station_error");
    assert_eq!(r.body["message"], "git_failed: the fake says no");
    let f = &s.f;
    f.store.set_link_features(&f.station, 98, vec![]);
    let r = s.c.get(&branches, &s.h()[..1]).await;
    expect(&r, 409, "fleet_git_unsupported");
    // An audit row of the refusal.
    let r = s.c.req("POST", &path(&s, "fetch"), &s.h(), None).await;
    expect(&r, 409, "fleet_git_unsupported");
    assert_eq!(
        s.c.audited("project.fetch", AuditResult::Failed).await,
        [Some("fleet_git_unsupported".to_owned())]
    );
}
