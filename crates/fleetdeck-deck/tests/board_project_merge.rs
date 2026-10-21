//! Merge into project (Fleet Sync §2) through the board route:
//! `POST /api/projects/{id}/merge { from, command_id }` (`project.merge`).

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
mod share_support;

use common::board::Http;
use common::fleet_git_fake::{Setup, bearer, expect, input, setup};
use fleetdeck_deck::store::AuditResult;
use fleetdeck_proto::integrations::{ForgeBinding, ForgeKind, Project};
use fleetdeck_proto::link::Scope;
use serde_json::{Value, json};

/// A second project `Mate` of the same repository on another station.
async fn mate(s: &Setup) -> Project {
    s.f.store
        .create_project("station-two".into(), input("Mate", "/srv/mate"))
        .await
        .unwrap()
}

async fn merge(s: &Setup, target: &str, body: Value) -> Http {
    s.c.post(&format!("/api/projects/{target}/merge"), &s.h(), body)
        .await
}

#[tokio::test]
async fn the_owner_merges_a_project_into_another_and_a_repeat_is_unknown() {
    let s = setup().await;
    let mate = mate(&s).await;
    let id = s.app.project_id.clone();
    let r = merge(
        &s,
        &id,
        json!({ "from": mate.project_id, "command_id": "m1" }),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body["project_id"], json!(id));
    assert_eq!(r.body["name"], "App");
    let homes = r.body["homes"].as_array().unwrap();
    assert_eq!(homes.len(), 2, "{}", r.body);
    assert_eq!(homes[0]["station_id"], json!(s.f.station.0));
    assert_eq!(homes[0]["position"], 0);
    assert_eq!(homes[1]["station_id"], "station-two");
    assert_eq!(homes[1]["position"], 1);
    assert_eq!(homes[1]["path"], "/srv/mate");
    assert_eq!(homes[1]["state"], "ready");
    // The source ended.
    assert!(
        s.f.store
            .project(mate.project_id.clone())
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(s.app().await.homes.len(), 2);
    // One audit row with the source and the number of homes.
    assert_eq!(
        s.c.audited("project.merge", AuditResult::Ok).await,
        vec![Some(format!("from {} homes 1", mate.project_id))]
    );
    // A repeat answers 404 for the source.
    let again = merge(
        &s,
        &id,
        json!({ "from": mate.project_id, "command_id": "m2" }),
    )
    .await;
    expect(&again, 404, "unknown_project");
    assert_eq!(again.body["message"], "Unknown project.");
    assert_eq!(
        s.c.audited("project.merge", AuditResult::Refused).await,
        vec![Some("unknown_project".to_owned())]
    );
}

#[tokio::test]
async fn another_repository_is_refused_with_409() {
    let s = setup().await;
    let mut other = input("Other", "/srv/other");
    other.forge = Some(ForgeBinding {
        kind: ForgeKind::Github,
        host: "github.com".into(),
        repo: "acme/tools".into(),
    });
    let other =
        s.f.store
            .create_project("station-two".into(), other)
            .await
            .unwrap();
    let r = merge(
        &s,
        &s.app.project_id,
        json!({ "from": other.project_id, "command_id": "m1" }),
    )
    .await;
    expect(&r, 409, "not_same_repo");
    assert_eq!(
        r.body["message"],
        "Other uses github.com/acme/tools, not github.com/acme/app."
    );
    assert_eq!(s.app().await.homes.len(), 1);
    assert!(s.f.store.project(other.project_id).await.unwrap().is_some());
}

#[tokio::test]
async fn only_the_owner_with_a_csrf_token_merges_and_the_body_is_checked() {
    let s = setup().await;
    let mate = mate(&s).await;
    let id = s.app.project_id.clone();
    let path = format!("/api/projects/{id}/merge");
    let body = json!({ "from": mate.project_id, "command_id": "m1" });
    // A token is refused, also with the admin scope.
    let admin = bearer(&s.c, "admin", &[Scope::Read, Scope::Admin]).await;
    let admin = [(admin.0.as_str(), admin.1.as_str())];
    let r = s.c.req("POST", &path, &admin, Some(body.clone())).await;
    expect(&r, 403, "owner_only");
    // A member is refused.
    let owner = vec![
        ("Cookie".to_owned(), s.cookie.clone()),
        ("X-CSRF-Token".to_owned(), s.csrf.clone()),
    ];
    let (member, _, _) = share_support::join(
        &s.c,
        &owner,
        json!({ "kind": "board", "role": "approver" }),
        "Ann",
    )
    .await;
    let r =
        s.c.req(
            "POST",
            &path,
            &share_support::refs(&member),
            Some(body.clone()),
        )
        .await;
    assert_eq!(r.status, 403, "{}", r.body);
    // A request without the CSRF token is refused.
    let no_csrf = [("Cookie", s.cookie.as_str())];
    let r = s.c.req("POST", &path, &no_csrf, Some(body)).await;
    assert_eq!(r.status, 403, "{}", r.body);
    // Nothing merged.
    assert!(
        s.f.store
            .project(mate.project_id.clone())
            .await
            .unwrap()
            .is_some()
    );
    // The body: a bad command id, a missing or an unknown field, the same
    // project, an unknown project.
    expect(
        &merge(
            &s,
            &id,
            json!({ "from": mate.project_id, "command_id": "bad id!" }),
        )
        .await,
        400,
        "invalid_command_id",
    );
    expect(
        &merge(&s, &id, json!({ "command_id": "m2" })).await,
        400,
        "invalid_body",
    );
    expect(
        &merge(
            &s,
            &id,
            json!({ "from": mate.project_id, "command_id": "m2", "extra": 1 }),
        )
        .await,
        400,
        "invalid_body",
    );
    let same = merge(&s, &id, json!({ "from": id, "command_id": "m3" })).await;
    expect(&same, 400, "invalid_body");
    assert_eq!(same.body["message"], "Choose another project.");
    expect(
        &merge(
            &s,
            "nope",
            json!({ "from": mate.project_id, "command_id": "m4" }),
        )
        .await,
        404,
        "unknown_project",
    );
    assert!(
        s.c.audited("project.merge", AuditResult::Ok)
            .await
            .is_empty()
    );
}
