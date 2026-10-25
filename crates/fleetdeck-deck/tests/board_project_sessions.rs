//! The project of a session (DL-176): the assign, clear and auto routes need
//! `drive`, the CSRF token of a cookie login and are audited; a member never
//! reaches them; a project of another station is refused; the cards of
//! `GET /api/sessions` carry the `project_id`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::BTreeSet;
use std::sync::Arc;

use common::board::{BoardClient, Http, board};
use common::*;
use fleetdeck_deck::store::AuditResult;
use fleetdeck_proto::SessionSummary;
use fleetdeck_proto::event::{ProviderKind, SessionState};
use fleetdeck_proto::integrations::{Project, ProjectInput};
use fleetdeck_proto::link::Scope;
use serde_json::{Value, json};

const DAY: i64 = 24 * 60 * 60 * 1000;

fn card(id: &str, cwd: &str) -> SessionSummary {
    let mut s = SessionSummary::new(id.into());
    s.state = SessionState::Running;
    s.cwd = Some(cwd.to_owned());
    s.provider = Some(ProviderKind::Claude);
    s
}

fn input(path: &str) -> ProjectInput {
    ProjectInput {
        name: "App".into(),
        station: "fake".into(),
        path: path.into(),
        forge: None,
        trackers: vec![],
        prompt_template: None,
        review_prompt: None,
    }
}

async fn bearer(c: &BoardClient, name: &str, scopes: &[Scope]) -> (String, String) {
    let (_, t) = c
        .store
        .create_token(
            name.into(),
            scopes.iter().copied().collect::<BTreeSet<_>>(),
            DAY,
        )
        .await
        .unwrap();
    ("Authorization".to_owned(), format!("Bearer {}", t.expose()))
}

struct Setup {
    f: Fixture,
    c: BoardClient,
    app: Project,
    other: Project,
}

async fn setup() -> Setup {
    let f = fixture().await;
    f.store
        .apply_snapshot(
            f.station.clone(),
            f.inc.clone(),
            0,
            vec![card("s1", "/work/app/src"), card("s2", "/elsewhere")],
        )
        .await
        .unwrap();
    let app = f
        .store
        .create_project(f.station.0.clone(), input("/work/app"))
        .await
        .unwrap();
    let other = f
        .store
        .create_project("another-station".into(), input("/work/app"))
        .await
        .unwrap();
    let c = board(Arc::clone(&f.ctx)).await;
    Setup { f, c, app, other }
}

async fn project_ids(c: &BoardClient, h: &[(&str, &str)]) -> Vec<(String, Option<String>)> {
    let r = c.get("/api/sessions", h).await;
    assert_eq!(r.status, 200, "{}", r.body);
    r.body
        .as_array()
        .unwrap()
        .iter()
        .map(|s| {
            (
                s["summary"]["session_id"].as_str().unwrap().to_owned(),
                s["project_id"].as_str().map(str::to_owned),
            )
        })
        .collect()
}

fn expect(r: &Http, status: u16, code: &str) {
    assert_eq!((r.status, r.code()), (status, code), "{}", r.body);
}

#[tokio::test]
async fn the_owner_assigns_clears_and_resets_the_project_of_a_session() {
    let Setup { f, c, app, .. } = setup().await;
    let st = f.station.0.clone();
    let (cookie, csrf) = c.login().await;
    let h = [("Cookie", cookie.as_str()), ("X-CSRF-Token", csrf.as_str())];
    let id = app.project_id.clone();

    // The folder decides at first.
    assert_eq!(
        project_ids(&c, &h[..1]).await,
        [("s1".into(), Some(id.clone())), ("s2".into(), None)]
    );

    // A manual assignment puts s2 in the project.
    let r = c
        .post(
            &format!("/api/projects/{id}/sessions/assign"),
            &h,
            json!({ "station": st, "session_id": "s2" }),
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body["project_id"], json!(id));
    assert_eq!(
        project_ids(&c, &h[..1]).await,
        [
            ("s1".into(), Some(id.clone())),
            ("s2".into(), Some(id.clone()))
        ]
    );

    // "No project" overrides the folder.
    let r = c
        .post(
            &format!("/api/sessions/{st}/s1/project/clear"),
            &h,
            json!({}),
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(
        project_ids(&c, &h[..1]).await,
        [("s1".into(), None), ("s2".into(), Some(id.clone()))]
    );

    // Automatic: back to the folder.
    for session in ["s1", "s2"] {
        let r = c
            .post(
                &format!("/api/sessions/{st}/{session}/project/auto"),
                &h,
                json!({}),
            )
            .await;
        assert_eq!(r.status, 200, "{}", r.body);
    }
    assert_eq!(
        project_ids(&c, &h[..1]).await,
        [("s1".into(), Some(id)), ("s2".into(), None)]
    );

    assert_eq!(
        c.audited("sessions.project_assign", AuditResult::Ok)
            .await
            .len(),
        1
    );
    assert_eq!(
        c.audited("sessions.project_clear", AuditResult::Ok)
            .await
            .len(),
        1
    );
    assert_eq!(
        c.audited("sessions.project_auto", AuditResult::Ok)
            .await
            .len(),
        2
    );
}

#[tokio::test]
async fn the_routes_refuse_a_wrong_station_an_unknown_target_and_a_bad_body() {
    let Setup { f, c, app, other } = setup().await;
    let st = f.station.0.clone();
    let (cookie, csrf) = c.login().await;
    let h = [("Cookie", cookie.as_str()), ("X-CSRF-Token", csrf.as_str())];

    // A project of another station.
    let r = c
        .post(
            &format!("/api/projects/{}/sessions/assign", other.project_id),
            &h,
            json!({ "station": st, "session_id": "s1" }),
        )
        .await;
    expect(&r, 400, "wrong_station");
    assert_eq!(
        r.body["message"],
        "This project has no home on that station."
    );
    let r = c
        .post(
            "/api/projects/nope/sessions/assign",
            &h,
            json!({ "station": st, "session_id": "s1" }),
        )
        .await;
    expect(&r, 404, "unknown_project");
    let r = c
        .post(
            &format!("/api/projects/{}/sessions/assign", app.project_id),
            &h,
            json!({ "station": st, "session_id": "nope" }),
        )
        .await;
    expect(&r, 404, "unknown_session");
    let r = c
        .post(
            &format!("/api/projects/{}/sessions/assign", app.project_id),
            &h,
            json!({ "station": st }),
        )
        .await;
    expect(&r, 400, "invalid_body");
    let r = c
        .post(
            &format!("/api/sessions/{st}/nope/project/clear"),
            &h,
            json!({}),
        )
        .await;
    expect(&r, 404, "unknown_session");
    assert!(f.store.project_assignments().await.unwrap().is_empty());
    assert_eq!(
        c.audited("sessions.project_assign", AuditResult::Failed)
            .await
            .len(),
        4
    );
}

#[tokio::test]
async fn a_drive_token_may_assign_and_a_read_token_or_a_missing_csrf_may_not() {
    let Setup { f, c, app, .. } = setup().await;
    let st = f.station.0.clone();
    let path = format!("/api/projects/{}/sessions/assign", app.project_id);
    let body = json!({ "station": st, "session_id": "s2" });

    let read = bearer(&c, "reader", &[Scope::Read]).await;
    let read = [(read.0.as_str(), read.1.as_str())];
    assert_eq!(c.post(&path, &read, body.clone()).await.status, 403);

    let (cookie, _) = c.login().await;
    let no_csrf = [("Cookie", cookie.as_str())];
    assert_eq!(c.post(&path, &no_csrf, body.clone()).await.status, 403);
    assert!(f.store.project_assignments().await.unwrap().is_empty());

    let drive = bearer(&c, "driver", &[Scope::Read, Scope::Drive]).await;
    let drive = [(drive.0.as_str(), drive.1.as_str())];
    let r = c.post(&path, &drive, body).await;
    assert_eq!(r.status, 200, "{}", r.body);
    let r = c
        .post(
            &format!("/api/sessions/{st}/s2/project/auto"),
            &drive,
            json!({}),
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
}

#[tokio::test]
async fn a_member_reaches_none_of_the_routes_and_sees_no_project_id() {
    let Setup { f, c, app, .. } = setup().await;
    let st = f.station.0.clone();
    let (cookie, csrf) = c.login().await;
    let owner = [("Cookie", cookie.as_str()), ("X-CSRF-Token", csrf.as_str())];
    let r = c
        .post(
            "/api/shares",
            &owner,
            json!({ "kind": "board", "role": "approver" }),
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    let token = r.body["url"]
        .as_str()
        .unwrap()
        .split("#join=")
        .nth(1)
        .unwrap()
        .to_owned();
    let j = c
        .post(
            "/api/auth/join",
            &[],
            json!({ "token": token, "display_name": "Ann" }),
        )
        .await;
    assert_eq!(j.status, 200, "{}", j.body);
    let mcookie = j
        .header("set-cookie")
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let mcsrf = j.body["csrf"].as_str().unwrap().to_owned();
    let m = [
        ("Cookie", mcookie.as_str()),
        ("X-CSRF-Token", mcsrf.as_str()),
    ];

    for path in [
        format!("/api/projects/{}/sessions/assign", app.project_id),
        format!("/api/sessions/{st}/s1/project/clear"),
        format!("/api/sessions/{st}/s1/project/auto"),
    ] {
        let r = c
            .post(&path, &m, json!({ "station": st, "session_id": "s1" }))
            .await;
        expect(&r, 403, "owner_only");
    }
    assert!(f.store.project_assignments().await.unwrap().is_empty());

    let ids = project_ids(&c, &m[..1]).await;
    assert_eq!(ids.len(), 2);
    assert!(ids.iter().all(|(_, p)| p.is_none()), "{ids:?}");
    // The owner still sees the project.
    assert_eq!(
        project_ids(&c, &owner[..1]).await[0].1.as_deref(),
        Some(app.project_id.as_str())
    );
    let _: Value = json!(null);
}
