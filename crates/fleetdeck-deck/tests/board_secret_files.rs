//! The secret file routes of the board (Fleet Sync FS-3, design §3.6): the
//! list, its change and the sync run. Owner's board login only, CSRF on the
//! writes, audited, with the codes of the bad input.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
mod share_support;

use common::fleet_git_fake::{Setup, bearer, expect, setup};
use fleetdeck_deck::store::AuditResult;
use fleetdeck_proto::fleet_git_secrets::{SECRET_HIDDEN_TEXT, SECRET_PATH_TEXT, SecretOutcome};
use fleetdeck_proto::link::Scope;
use serde_json::{Value, json};

fn url(s: &Setup) -> String {
    format!("/api/projects/{}/secret-files", s.app.project_id)
}

async fn put(s: &Setup, paths: Value, command: &str) -> common::board::Http {
    s.c.req(
        "PUT",
        &url(s),
        &s.h(),
        Some(json!({ "paths": paths, "command_id": command })),
    )
    .await
}

#[tokio::test]
async fn a_list_is_saved_cleaned_and_read_back() {
    let s = setup().await;
    let empty = s.c.get(&url(&s), &s.h()).await;
    assert_eq!(empty.status, 200, "{}", empty.body);
    assert_eq!(empty.body, json!({ "paths": [], "files": [] }));
    let r = put(
        &s,
        json!([
            "  .env  ",
            "",
            "apps/web/.env.local",
            ".env",
            "   ",
            "keys/a.pem"
        ]),
        "c1",
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(
        r.body["paths"],
        json!([".env", "apps/web/.env.local", "keys/a.pem"])
    );
    assert_eq!(r.body["files"].as_array().unwrap().len(), 3);
    assert_eq!(r.body["files"][0]["path"], ".env");
    assert_eq!(r.body["files"][0]["outcome"], "not_run");
    assert_eq!(r.body["files"][0]["conflict"], false);
    assert_eq!(r.body["files"][0]["stations"], json!([]));
    let again = s.c.get(&url(&s), &s.h()).await;
    assert_eq!(again.body, r.body);
    let rows = s.c.audited("project.secret_files", AuditResult::Ok).await;
    assert_eq!(rows, [Some("3 files".to_owned())]);
    // A new list keeps the salt of a path that stays.
    let id = s.app.project_id.clone();
    let salt =
        s.f.store
            .secret_file(id.clone(), ".env".into())
            .await
            .unwrap()
            .unwrap()
            .salt;
    let r = put(&s, json!(["keys/a.pem", ".env"]), "c2").await;
    assert_eq!(r.body["paths"], json!(["keys/a.pem", ".env"]));
    let kept =
        s.f.store
            .secret_file(id, ".env".into())
            .await
            .unwrap()
            .unwrap();
    assert_eq!(kept.salt, salt);
    // An empty list is allowed.
    let r = put(&s, json!([]), "c3").await;
    assert_eq!(r.body["paths"], json!([]));
}

#[tokio::test]
async fn a_bad_path_is_refused_with_its_text_and_the_field() {
    let s = setup().await;
    for bad in [
        "../.env",
        "/etc/.env",
        "a//.env",
        "a/./.env",
        ".git/.env",
        "a\\.env",
    ] {
        let r = put(&s, json!([bad]), "c1").await;
        expect(&r, 400, "invalid_secret_path");
        assert_eq!(r.body["field"], "paths", "{bad}");
        assert_eq!(r.body["message"], SECRET_PATH_TEXT, "{bad}");
    }
    let long = format!("{}/.env", "a".repeat(260));
    expect(
        &put(&s, json!([long]), "c1").await,
        400,
        "invalid_secret_path",
    );
    // A path that members can read.
    let r = put(&s, json!([".env", "README.md"]), "c2").await;
    expect(&r, 400, "invalid_secret_path");
    assert_eq!(r.body["message"], SECRET_HIDDEN_TEXT);
    assert_eq!(r.body["field"], "paths");
    // Nothing was saved.
    assert_eq!(s.c.get(&url(&s), &s.h()).await.body["paths"], json!([]));
    let refused =
        s.c.audited("project.secret_files", AuditResult::Refused)
            .await;
    assert_eq!(refused.len(), 8);
    assert!(
        refused
            .iter()
            .all(|d| d.as_deref() == Some("invalid_secret_path"))
    );
}

#[tokio::test]
async fn more_than_twenty_files_are_refused() {
    let s = setup().await;
    let twenty: Vec<String> = (0..20).map(|n| format!(".env.{n}")).collect();
    let r = put(&s, json!(twenty), "c1").await;
    assert_eq!(r.status, 200, "{}", r.body);
    let mut many: Vec<String> = (0..21).map(|n| format!(".env.{n}")).collect();
    let r = put(&s, json!(many), "c2").await;
    expect(&r, 400, "too_many_secret_files");
    assert_eq!(r.body["field"], "paths");
    assert_eq!(
        r.body["message"],
        "A project can list at most 20 secret files."
    );
    // Duplicates and empty lines do not count.
    many.push(String::new());
    many.push(".env.0".to_owned());
    many.truncate(20);
    many.push("  ".to_owned());
    many.push(".env.1".to_owned());
    assert_eq!(put(&s, json!(many), "c3").await.status, 200);
}

#[tokio::test]
async fn the_routes_are_for_the_owner_login_with_a_csrf_token() {
    let s = setup().await;
    let path = url(&s);
    let sync = format!("{path}/sync");
    let admin = bearer(&s.c, "admin", &[Scope::Read, Scope::Admin]).await;
    let admin = [(admin.0.as_str(), admin.1.as_str())];
    let put_body = json!({ "paths": [".env"], "command_id": "c1" });
    let post_body = json!({ "command_id": "c1" });
    // A token is refused whatever its scopes.
    expect(&s.c.get(&path, &admin).await, 403, "owner_only");
    expect(
        &s.c.req("PUT", &path, &admin, Some(put_body.clone())).await,
        403,
        "owner_only",
    );
    expect(
        &s.c.post(&sync, &admin, post_body.clone()).await,
        403,
        "owner_only",
    );
    // A member is refused too.
    let owner = vec![
        ("Cookie".to_owned(), s.cookie.clone()),
        ("X-CSRF-Token".to_owned(), s.csrf.clone()),
    ];
    let (m, _, _) = share_support::join(
        &s.c,
        &owner,
        json!({ "kind": "board", "role": "approver" }),
        "Ann",
    )
    .await;
    let m = share_support::refs(&m);
    expect(&s.c.get(&path, &m).await, 403, "owner_only");
    expect(
        &s.c.req("PUT", &path, &m, Some(put_body.clone())).await,
        403,
        "owner_only",
    );
    expect(
        &s.c.post(&sync, &m, post_body.clone()).await,
        403,
        "owner_only",
    );
    // No CSRF token on the writes, and no login at all.
    let no_csrf = [("Cookie", s.cookie.as_str())];
    assert_eq!(
        s.c.req("PUT", &path, &no_csrf, Some(put_body.clone()))
            .await
            .status,
        403
    );
    assert_eq!(
        s.c.post(&sync, &no_csrf, post_body.clone()).await.status,
        403
    );
    assert_eq!(s.c.get(&path, &[]).await.status, 401);
    assert_eq!(s.c.req("PUT", &path, &[], Some(put_body)).await.status, 401);
    assert_eq!(s.c.get(&path, &s.h()).await.body["paths"], json!([]));
}

#[tokio::test]
async fn the_bodies_and_the_project_are_checked() {
    let s = setup().await;
    // An unknown project.
    let r =
        s.c.req(
            "PUT",
            "/api/projects/nope/secret-files",
            &s.h(),
            Some(json!({ "paths": [], "command_id": "c" })),
        )
        .await;
    expect(&r, 404, "unknown_project");
    expect(
        &s.c.get("/api/projects/nope/secret-files", &s.h()).await,
        404,
        "unknown_project",
    );
    expect(
        &s.c.post(
            "/api/projects/nope/secret-files/sync",
            &s.h(),
            json!({ "command_id": "c" }),
        )
        .await,
        404,
        "unknown_project",
    );
    // A bad command id, an unknown field and a missing field.
    expect(
        &put(&s, json!([]), "bad id!").await,
        400,
        "invalid_command_id",
    );
    let r =
        s.c.req(
            "PUT",
            &url(&s),
            &s.h(),
            Some(json!({ "paths": [], "command_id": "c", "extra": 1 })),
        )
        .await;
    expect(&r, 400, "invalid_body");
    let r =
        s.c.post(&format!("{}/sync", url(&s)), &s.h(), json!({}))
            .await;
    expect(&r, 400, "invalid_body");
    let r =
        s.c.post(
            &format!("{}/sync", url(&s)),
            &s.h(),
            json!({ "command_id": "bad id!" }),
        )
        .await;
    expect(&r, 400, "invalid_command_id");
}

#[tokio::test]
async fn a_sync_without_a_remote_is_refused_and_one_without_files_is_empty() {
    let s = setup().await;
    let sync = format!("{}/sync", url(&s));
    // The project of the fixture has a forge but no remote yet.
    let r = s.c.post(&sync, &s.h(), json!({ "command_id": "s1" })).await;
    expect(&r, 409, "needs_remote");
    assert_eq!(r.body["message"], "Set the forge of the project first.");
    let refused =
        s.c.audited("project.secret_sync_run", AuditResult::Refused)
            .await;
    assert_eq!(refused, [Some("needs_remote".to_owned())]);
    // With a remote and no file the run is empty and audited.
    let saved = s
        .save(
            json!({ "sync": false }),
            Some(common::fleet_git_fake::REMOTE),
        )
        .await;
    assert_eq!(saved.status, 200, "{}", saved.body);
    let r = s.c.post(&sync, &s.h(), json!({ "command_id": "s2" })).await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body, json!({ "files": [] }));
    let ok =
        s.c.audited("project.secret_sync_run", AuditResult::Ok)
            .await;
    assert_eq!(ok, [Some("0 files".to_owned())]);
}

#[test]
fn the_board_methods_are_in_the_method_table() {
    use fleetdeck_deck::http::methods::api;
    for (name, method, path, mutating) in [
        (
            "projects.secret_files",
            "GET",
            "/api/projects/{project}/secret-files",
            false,
        ),
        (
            "projects.secret_files_set",
            "PUT",
            "/api/projects/{project}/secret-files",
            true,
        ),
        (
            "projects.secret_sync",
            "POST",
            "/api/projects/{project}/secret-files/sync",
            true,
        ),
    ] {
        let entry = api()
            .find(|m| m.name == name)
            .unwrap_or_else(|| panic!("{name}"));
        assert_eq!(entry.method.as_str(), method, "{name}");
        assert_eq!(entry.path, path, "{name}");
        assert_eq!(entry.scope, Some(Scope::Admin), "{name}");
        assert_eq!(entry.mutating, mutating, "{name}");
        assert!(!entry.member, "{name}");
    }
}

// ----- the store -----

const ENV: &str = ".env";

#[tokio::test]
async fn the_list_keeps_the_salt_and_the_seen_digest_of_a_kept_path() {
    let s = setup().await;
    let store = &s.f.store;
    let id = s.app.project_id.clone();
    assert_eq!(store.secret_files(id.clone()).await.unwrap(), []);
    store
        .secret_files_set(
            id.clone(),
            vec!["a/.env".into(), ".env".into(), "k.pem".into()],
        )
        .await
        .unwrap()
        .unwrap();
    let rows = store.secret_files(id.clone()).await.unwrap();
    assert_eq!(
        rows.iter().map(|r| r.path.as_str()).collect::<Vec<_>>(),
        ["a/.env", ".env", "k.pem"]
    );
    assert!(rows.iter().all(|r| r.salt.len() == 32));
    // Salts are different for each path.
    assert_ne!(rows[0].salt, rows[1].salt);
    // A row that never ran reads as `not_run`.
    assert_eq!(rows[0].last_report.outcome, SecretOutcome::NotRun);
    assert!(rows[0].last_report.stations.is_empty());
    assert!(!rows[0].last_report.conflict);
    assert_eq!(rows[0].last_report.path, "a/.env");
    assert_eq!(
        (rows[0].last_digest.clone(), rows[0].seen.clone()),
        (None, vec![])
    );
    // Facts of a run.
    let mut report = rows[1].last_report.clone();
    report.outcome = SecretOutcome::Copied;
    let done = store
        .secret_file_done(
            id.clone(),
            ".env".into(),
            rows[1].salt.clone(),
            Some(("dg".into(), "st-1".into())),
            report,
        )
        .await
        .unwrap();
    assert!(done);
    assert!(
        store
            .secret_seen_set(
                id.clone(),
                ".env".into(),
                rows[1].salt.clone(),
                vec![("st-1".into(), "dg".into()), ("st-2".into(), "dg".into())],
            )
            .await
            .unwrap()
    );
    assert!(
        store
            .secret_seen_set(
                id.clone(),
                "k.pem".into(),
                rows[2].salt.clone(),
                vec![("st-1".into(), "kk".into())],
            )
            .await
            .unwrap()
    );
    // A new order, a removed path and a new path.
    store
        .secret_files_set(
            id.clone(),
            vec![".env".into(), "new.key".into(), "a/.env".into()],
        )
        .await
        .unwrap()
        .unwrap();
    let after = store.secret_files(id.clone()).await.unwrap();
    assert_eq!(
        after.iter().map(|r| r.path.as_str()).collect::<Vec<_>>(),
        [".env", "new.key", "a/.env"]
    );
    let kept = &after[0];
    assert_eq!(kept.salt, rows[1].salt);
    assert_eq!(kept.last_digest.as_deref(), Some("dg"));
    assert_eq!(kept.last_source.as_deref(), Some("st-1"));
    assert!(kept.last_synced_at.is_some());
    assert_eq!(kept.last_report.outcome, SecretOutcome::Copied);
    assert_eq!(kept.seen.len(), 2);
    assert_eq!(after[2].salt, rows[0].salt);
    assert_ne!(after[1].salt, rows[2].salt);
    // The removed path took its seen digests with it.
    let again = store.secret_file(id.clone(), "k.pem".into()).await.unwrap();
    assert_eq!(again, None);
    store
        .secret_files_set(id.clone(), vec!["k.pem".into()])
        .await
        .unwrap();
    let back = store
        .secret_file(id.clone(), "k.pem".into())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(back.seen, []);
    assert_ne!(back.salt, rows[2].salt);
    // An unknown project.
    assert_eq!(
        store.secret_files_set("nope".into(), vec![]).await.unwrap(),
        None
    );
    // Removing every path.
    store
        .secret_files_set(id.clone(), vec![])
        .await
        .unwrap()
        .unwrap();
    assert_eq!(store.secret_files(id).await.unwrap(), []);
}

#[tokio::test]
async fn a_changed_salt_stops_the_writes_of_a_run() {
    let s = setup().await;
    let store = &s.f.store;
    let id = s.app.project_id.clone();
    store
        .secret_files_set(id.clone(), vec![ENV.into()])
        .await
        .unwrap();
    let row = store
        .secret_file(id.clone(), ENV.into())
        .await
        .unwrap()
        .unwrap();
    let mut wrong = row.salt.clone();
    wrong[0] ^= 0xff;
    let report = row.last_report.clone();
    assert!(
        !store
            .secret_file_done(
                id.clone(),
                ENV.into(),
                wrong.clone(),
                Some(("dg".into(), "st".into())),
                report,
            )
            .await
            .unwrap()
    );
    assert!(
        !store
            .secret_seen_set(
                id.clone(),
                ENV.into(),
                wrong,
                vec![("st".into(), "dg".into())]
            )
            .await
            .unwrap()
    );
    let same = store
        .secret_file(id.clone(), ENV.into())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(same.last_digest, None);
    assert_eq!(same.seen, []);
    // A row that no longer exists.
    assert!(
        !store
            .secret_seen_set(
                id,
                "gone".into(),
                row.salt,
                vec![("st".into(), "dg".into())]
            )
            .await
            .unwrap()
    );
}
