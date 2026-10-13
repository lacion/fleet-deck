//! The accounts routes and the credential relay (DL-173) over loopback
//! HTTPS with a fake station link: the owner-only rule, the relay rules
//! (fixed errors, no value in the audit log, the answers or any column of
//! `deck.db`).

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
#[path = "common/accounts.rs"]
mod harness;

use std::collections::BTreeSet;
use std::time::Duration;

use common::*;
use fleetdeck_deck::store::AuditResult;
use fleetdeck_proto::link::{Outcome, Scope, WireError};
use harness::*;
use serde_json::json;

const OWNER_ROUTES: [(&str, &str); 8] = [
    ("GET", "/api/stations/fake/accounts"),
    ("POST", "/api/stations/fake/accounts/forge/login"),
    ("GET", "/api/stations/fake/accounts/forge/login/l1"),
    ("POST", "/api/stations/fake/accounts/acc-1/logout"),
    ("POST", "/api/secrets/stations/fake/accounts/apps"),
    ("POST", "/api/secrets/stations/fake/accounts/token"),
    ("POST", "/api/accounts/oauth/start"),
    ("POST", "/api/accounts/oauth/finish"),
];

#[tokio::test]
async fn a_token_a_member_and_a_missing_csrf_are_refused_on_every_account_route() {
    let mut s = setup().await;
    let (_, t) =
        s.c.store
            .create_token(
                "ops".into(),
                BTreeSet::from([Scope::Read, Scope::Drive, Scope::Admin]),
                DAY,
            )
            .await
            .unwrap();
    let bearer = token_headers(t.expose());
    // A member of a shared session.
    let share =
        s.c.post(
            "/api/shares",
            &refs(&s.h),
            json!({ "kind": "board", "role": "approver" }),
        )
        .await;
    assert_eq!(share.status, 200, "{}", share.body);
    let invite = share.body["url"].as_str().unwrap().to_owned();
    let join =
        s.c.post(
            "/api/auth/join",
            &[],
            json!({ "token": invite.split("#join=").nth(1).unwrap(), "display_name": "Ann" }),
        )
        .await;
    assert_eq!(join.status, 200, "{}", join.body);
    let member: Headers = vec![
        (
            "Cookie".to_owned(),
            join.header("set-cookie")
                .unwrap()
                .split(';')
                .next()
                .unwrap()
                .to_owned(),
        ),
        (
            "X-CSRF-Token".to_owned(),
            join.body["csrf"].as_str().unwrap().to_owned(),
        ),
    ];
    for (method, path) in OWNER_ROUTES {
        let body = (method == "POST").then(|| json!({}));
        for who in [&bearer, &member] {
            let r = s.c.req(method, path, &refs(who), body.clone()).await;
            expect(&r, 403, "owner_only");
        }
    }
    // The containers route is `read`: a token reads it, a member never.
    let containers = "/api/stations/fake/accounts/acc-1/containers";
    let r = s.c.get(containers, &refs(&member)).await;
    expect(&r, 403, "owner_only");
    // The owner needs the CSRF header on a POST.
    let no_csrf = vec![s.h[0].clone()];
    for (method, path) in OWNER_ROUTES.iter().filter(|(m, _)| *m == "POST") {
        let r =
            s.c.req(method, path, &refs(&no_csrf), Some(json!({})))
                .await;
        expect(&r, 403, "csrf");
    }
    no_request(&mut s.station, Duration::from_millis(300)).await;
    // A token with `read` reads the containers.
    let (_, reader) =
        s.c.store
            .create_token("reader".into(), BTreeSet::from([Scope::Read]), DAY)
            .await
            .unwrap();
    let reader = token_headers(reader.expose());
    let (r, req) = exchange(
        &mut s.station,
        s.c.get(containers, &refs(&reader)),
        ok(json!({ "containers": [{ "key": "PROJ", "name": "Project" }] })),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body["containers"][0]["key"], "PROJ");
    assert_eq!(req.method, "issues.containers");
    assert_eq!(req.params, json!({ "account_id": "acc-1" }));
}

#[tokio::test]
async fn the_owner_lists_accounts_logs_in_with_a_device_code_and_logs_out() {
    let mut s = setup().await;
    let list = json!({
        "accounts": [account("jira")],
        "apps": [{ "kind": "jira", "client_id": "cid", "has_secret": true, "generation": 2 }],
        "env_tokens": ["GH_TOKEN"],
    });
    let (r, req) = exchange(
        &mut s.station,
        s.c.get("/api/stations/fake/accounts", &refs(&s.h)),
        ok(list.clone()),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body, list);
    assert_eq!(req.method, "accounts.list");

    let login = json!({ "login_id": "l1", "code": "ABCD-1234",
        "url": "https://github.com/login/device", "expires_in": 900 });
    let (r, req) = exchange(
        &mut s.station,
        s.c.post(
            "/api/stations/fake/accounts/forge/login",
            &refs(&s.h),
            json!({ "kind": "github", "host": "github.com" }),
        ),
        ok(login.clone()),
    )
    .await;
    assert_eq!(r.body, login, "{}", r.body);
    assert_eq!(req.method, "accounts.forge_login");
    assert_eq!(
        req.params,
        json!({ "kind": "github", "host": "github.com" })
    );

    let (r, req) = exchange(
        &mut s.station,
        s.c.get("/api/stations/fake/accounts/forge/login/l1", &refs(&s.h)),
        ok(json!({ "state": "done", "user": "ana" })),
    )
    .await;
    assert_eq!(r.body, json!({ "state": "done", "user": "ana" }));
    assert_eq!(req.params, json!({ "login_id": "l1" }));

    let (r, req) = exchange(
        &mut s.station,
        s.c.post(
            "/api/stations/fake/accounts/acc-1/logout",
            &refs(&s.h),
            json!({}),
        ),
        ok(json!({})),
    )
    .await;
    assert_eq!((r.status, &r.body), (200, &json!({})));
    assert_eq!(req.method, "accounts.logout");
    assert_eq!(req.params, json!({ "account_id": "acc-1" }));

    // A kind that is not a forge, a bad host, a bad id and a missing station
    // never reach the station.
    for (path, body, code) in [
        (
            "/api/stations/fake/accounts/forge/login",
            json!({ "kind": "jira", "host": "x.com" }),
            "invalid_kind",
        ),
        (
            "/api/stations/fake/accounts/forge/login",
            json!({ "kind": "github", "host": "GitHub.com" }),
            "invalid_field",
        ),
        (
            "/api/stations/fake/accounts/forge/login",
            json!({ "kind": "github" }),
            "invalid_body",
        ),
    ] {
        expect(&s.c.post(path, &refs(&s.h), body).await, 400, code);
    }
    expect(
        &s.c.post(
            "/api/stations/nope/accounts/forge/login",
            &refs(&s.h),
            json!({ "kind": "github", "host": "github.com" }),
        )
        .await,
        404,
        "unknown_station",
    );
    no_request(&mut s.station, Duration::from_millis(300)).await;
    let rows =
        s.c.audited("accounts.forge_login", AuditResult::Refused)
            .await;
    assert_eq!(rows.len(), 4, "{rows:?}");
}

#[tokio::test]
async fn the_relay_forwards_a_secret_once_and_leaves_no_trace_of_it() {
    let mut s = setup().await;
    let secret = "canary-secret-8f31";
    let token = "canary-token-77c2";
    let (r, req) = exchange(
        &mut s.station,
        s.c.post(
            "/api/secrets/stations/fake/accounts/apps",
            &refs(&s.h),
            json!({ "kind": "jira", "client_id": "cid", "client_secret": secret }),
        ),
        ok(json!({ "kind": "jira", "client_id": "cid", "has_secret": true, "generation": 1 })),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(req.method, "accounts.app_set");
    assert_eq!(req.params["client_secret"], secret);
    assert!(!r.text.contains(secret));

    let (r, req) = exchange(
        &mut s.station,
        s.c.post(
            "/api/secrets/stations/fake/accounts/token",
            &refs(&s.h),
            json!({ "kind": "jira", "site": "https://acme.atlassian.net",
                    "email": "ana@acme.test", "token": token }),
        ),
        ok(
            json!({ "account_id": "acc-2", "kind": "jira", "site": "https://acme.atlassian.net",
                   "user": "ana", "method": "token", "status": "ok", "message": "connected" }),
        ),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(req.params["token"], token);
    assert!(!r.text.contains(token));

    // A forge token gets the default host.
    let (r, req) = exchange(
        &mut s.station,
        s.c.post(
            "/api/secrets/stations/fake/accounts/token",
            &refs(&s.h),
            json!({ "kind": "github", "token": token }),
        ),
        ok(
            json!({ "account_id": "acc-3", "kind": "github", "site": "github.com",
                   "method": "token", "status": "ok", "message": "connected" }),
        ),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(req.params["site"], "github.com");

    // A station error text, which holds the secret, never reaches the answer.
    let err = Outcome::Err(WireError::new(
        "internal",
        format!("bad value {secret} and {token}"),
    ));
    let (r, _) = exchange(
        &mut s.station,
        s.c.post(
            "/api/secrets/stations/fake/accounts/apps",
            &refs(&s.h),
            json!({ "kind": "linear", "client_id": "cid", "client_secret": secret }),
        ),
        err,
    )
    .await;
    expect(&r, 502, "station_error");
    assert!(
        !r.text.contains(secret) && !r.text.contains(token),
        "{}",
        r.text
    );

    // The refusals before the station: no request, no echo.
    for (path, body, status, code) in [
        (
            "apps",
            json!({ "kind": "github", "client_id": "c", "client_secret": secret }),
            400,
            "invalid_kind",
        ),
        (
            "apps",
            json!({ "kind": "jira", "client_id": "", "client_secret": secret }),
            400,
            "invalid_field",
        ),
        (
            "apps",
            json!({ "kind": "jira", "client_id": "c", "client_secret": secret, "x": 1 }),
            400,
            "invalid_body",
        ),
        (
            "token",
            json!({ "kind": "jira", "token": token }),
            400,
            "invalid_field",
        ),
        (
            "token",
            json!({ "kind": "github", "site": "Bad Host", "token": token }),
            400,
            "invalid_field",
        ),
        (
            "token",
            json!({ "kind": "linear", "token": format!("{token}\n") }),
            400,
            "invalid_field",
        ),
        (
            "token",
            json!({ "kind": "linear", "token": token.repeat(300) }),
            400,
            "invalid_field",
        ),
        (
            "token",
            json!({ "kind": "linear", "token": 5 }),
            400,
            "invalid_body",
        ),
        // A value of the wrong type or kind is never quoted back.
        (
            "token",
            json!({ "kind": "linear", "token": [token] }),
            400,
            "invalid_body",
        ),
        (
            "token",
            json!({ "kind": token, "token": secret }),
            400,
            "invalid_body",
        ),
        (
            "apps",
            json!({ "kind": "jira", "client_id": { "x": secret }, "client_secret": token }),
            400,
            "invalid_body",
        ),
        (
            "apps",
            json!({ "kind": secret, "client_id": "c", "client_secret": token }),
            400,
            "invalid_body",
        ),
    ] {
        let r =
            s.c.post(
                &format!("/api/secrets/stations/fake/accounts/{path}"),
                &refs(&s.h),
                body,
            )
            .await;
        expect(&r, status, code);
        assert!(
            !r.text.contains(secret) && !r.text.contains(token),
            "{}",
            r.text
        );
    }
    // A body above 16 KiB.
    let r =
        s.c.post(
            "/api/secrets/stations/fake/accounts/token",
            &refs(&s.h),
            json!({ "kind": "linear", "token": "a".repeat(20_000) }),
        )
        .await;
    assert_eq!(r.status, 413, "{}", r.text);
    no_request(&mut s.station, Duration::from_millis(300)).await;

    // No audit row, command, outbox item or column of deck.db holds a value.
    let audit = format!("{:?}", s.c.store.audit_log(100).await.unwrap());
    assert!(audit.contains("accounts.app_set") && audit.contains("accounts.token_set"));
    assert!(!audit.contains(secret) && !audit.contains(token), "{audit}");
    assert!(s.c.store.commands().await.unwrap().is_empty());
    let on_disk = database_text(s.dir.path());
    assert!(
        on_disk.contains("accounts.app_set"),
        "the scan reads the audit table"
    );
    for canary in [secret, token] {
        assert!(!on_disk.contains(canary), "{canary} is in deck.db");
    }
}
