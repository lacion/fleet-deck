//! The OAuth sign-in of Jira and Linear (DL-173) over loopback HTTPS with a
//! fake station link: the start (Origin, kind, authorize URL, limit,
//! expiry), the callback (one use, headers) and the finish (same login only).

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
#[path = "common/accounts.rs"]
mod harness;

use std::collections::BTreeSet;
use std::time::Duration;

use common::board::{BoardClient, Http};
use common::*;
use fleetdeck_proto::link::{Outcome, Scope, WireError};
use harness::*;
use serde_json::json;

const JIRA_URL: &str = "https://auth.atlassian.com/authorize?audience=api.atlassian.com";

/// Starts a sign-in as `h` and returns the answer and the station request.
async fn start(
    s: &mut Setup,
    h: &Headers,
    kind: &str,
    authorize_url: &str,
) -> (Http, Option<fleetdeck_link::control::InboundRequest>) {
    let headers = refs(h);
    let call = s.c.post(
        "/api/accounts/oauth/start",
        &headers,
        json!({ "station": "fake", "kind": kind }),
    );
    let (r, req) = exchange(
        &mut s.station,
        call,
        ok(json!({ "authorize_url": authorize_url })),
    )
    .await;
    (r, Some(req))
}

async fn start_owner(
    s: &mut Setup,
    kind: &str,
    authorize_url: &str,
) -> (Http, Option<fleetdeck_link::control::InboundRequest>) {
    let h = s.h.clone();
    start(s, &h, kind, authorize_url).await
}

async fn finish(c: &BoardClient, h: &Headers, id: &str) -> Http {
    c.post(
        "/api/accounts/oauth/finish",
        &refs(h),
        json!({ "transaction": id }),
    )
    .await
}

fn state_of(req: &fleetdeck_link::control::InboundRequest) -> String {
    req.params["state"].as_str().unwrap().to_owned()
}

#[tokio::test]
async fn start_checks_the_origin_the_kind_and_the_authorize_url() {
    let mut s = setup().await;
    let (r, req) = start_owner(&mut s, "jira", JIRA_URL).await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body, json!({ "authorize_url": JIRA_URL }));
    let req = req.unwrap();
    assert_eq!(req.method, "accounts.oauth_start");
    assert_eq!(req.params["kind"], "jira");
    assert_eq!(
        req.params["redirect_uri"],
        format!("{}/oauth/callback", s.c.origin())
    );
    assert_eq!(state_of(&req).len(), 43, "32 random bytes as base64url");

    // Linear: the host is linear.app.
    let (r, _) = start_owner(&mut s, "linear", "https://linear.app/oauth/authorize").await;
    assert_eq!(r.status, 200, "{}", r.body);

    // No Origin header.
    let no_origin: Headers = s.h.iter().filter(|(k, _)| k != "Origin").cloned().collect();
    let r =
        s.c.post(
            "/api/accounts/oauth/start",
            &refs(&no_origin),
            json!({ "station": "fake", "kind": "jira" }),
        )
        .await;
    expect(&r, 400, "bad_origin");
    // A foreign Origin is refused by the guard.
    let mut foreign = no_origin.clone();
    foreign.push(("Origin".to_owned(), "https://evil.example".to_owned()));
    let r =
        s.c.post(
            "/api/accounts/oauth/start",
            &refs(&foreign),
            json!({ "station": "fake", "kind": "jira" }),
        )
        .await;
    expect(&r, 403, "origin");
    // A kind without OAuth.
    let r =
        s.c.post(
            "/api/accounts/oauth/start",
            &refs(&s.h),
            json!({ "station": "fake", "kind": "github" }),
        )
        .await;
    expect(&r, 400, "invalid_kind");
    no_request(&mut s.station, Duration::from_millis(300)).await;

    // An authorize URL of another host, with userinfo, or http is refused.
    for bad in [
        "https://evil.example/authorize",
        "http://auth.atlassian.com/authorize",
        "https://auth.atlassian.com@evil.example/authorize",
        "https://auth.atlassian.com.evil.example/authorize",
    ] {
        let (r, _) = start_owner(&mut s, "jira", bad).await;
        expect(&r, 502, "bad_authorize_url");
    }
    // The wrong host for the kind.
    let (r, _) = start_owner(&mut s, "linear", JIRA_URL).await;
    expect(&r, 502, "bad_authorize_url");
}

#[tokio::test]
async fn a_session_has_four_open_sign_ins_and_they_expire_after_ten_minutes() {
    let mut s = setup().await;
    let h = s.h.clone();
    for _ in 0..4 {
        let (r, _) = start(&mut s, &h, "jira", JIRA_URL).await;
        assert_eq!(r.status, 200, "{}", r.body);
    }
    let r =
        s.c.post(
            "/api/accounts/oauth/start",
            &refs(&h),
            json!({ "station": "fake", "kind": "jira" }),
        )
        .await;
    expect(&r, 429, "too_many_oauth");
    no_request(&mut s.station, Duration::from_millis(300)).await;
    // Another login has its own limit.
    let other = owner(&s.c).await;
    let (r, _) = start(&mut s, &other, "jira", JIRA_URL).await;
    assert_eq!(r.status, 200, "{}", r.body);
    // The four expire.
    s.f.det.advance(Duration::from_secs(601));
    let (r, _) = start(&mut s, &h, "jira", JIRA_URL).await;
    assert_eq!(r.status, 200, "{}", r.body);
}

fn location(r: &Http) -> &str {
    r.header("location").unwrap_or_default()
}

fn no_store(r: &Http) {
    assert_eq!(r.header("cache-control"), Some("no-store"));
    assert_eq!(r.header("referrer-policy"), Some("no-referrer"));
}

#[tokio::test]
async fn the_callback_claims_a_state_once_and_sends_the_browser_to_a_clean_url() {
    let mut s = setup().await;
    let h = s.h.clone();
    let (_, req) = start(&mut s, &h, "jira", JIRA_URL).await;
    let state = state_of(&req.unwrap());
    let r =
        s.c.get(&format!("/oauth/callback?code=abc123&state={state}"), &[])
            .await;
    assert_eq!(r.status, 303, "{}", r.text);
    assert!(
        location(&r).starts_with("/#/settings/accounts?finish="),
        "{}",
        location(&r)
    );
    assert!(!location(&r).contains(&state) && !location(&r).contains("abc123"));
    no_store(&r);
    // The second use of the same state.
    let again =
        s.c.get(&format!("/oauth/callback?code=abc123&state={state}"), &[])
            .await;
    assert_eq!(again.status, 303);
    assert_eq!(
        location(&again),
        "/#/settings/accounts?oauth_error=invalid_state"
    );
    no_store(&again);

    // Unknown, missing, long and unclean states; no cookie is needed.
    for query in [
        "code=x&state=unknown".to_owned(),
        "code=x".to_owned(),
        String::new(),
        format!("code=x&state={}", "a".repeat(513)),
        "code=x&state=a%20b".to_owned(),
    ] {
        let r = s.c.get(&format!("/oauth/callback?{query}"), &[]).await;
        assert_eq!(r.status, 303, "{query}");
        assert_eq!(
            location(&r),
            "/#/settings/accounts?oauth_error=invalid_state",
            "{query}"
        );
        no_store(&r);
    }

    // A provider error, and a state without a code, end the transaction.
    for query in ["error=access_denied&code=zzz&state=", "state="] {
        let (_, req) = start(&mut s, &h, "jira", JIRA_URL).await;
        let state = state_of(&req.unwrap());
        let r =
            s.c.get(&format!("/oauth/callback?{query}{state}"), &[])
                .await;
        assert_eq!(
            location(&r),
            "/#/settings/accounts?oauth_error=denied",
            "{query}"
        );
        no_store(&r);
        let r =
            s.c.get(&format!("/oauth/callback?code=x&state={state}"), &[])
                .await;
        assert_eq!(
            location(&r),
            "/#/settings/accounts?oauth_error=invalid_state"
        );
    }
    // An expired transaction.
    let (_, req) = start(&mut s, &h, "jira", JIRA_URL).await;
    let state = state_of(&req.unwrap());
    s.f.det.advance(Duration::from_secs(601));
    let r =
        s.c.get(&format!("/oauth/callback?code=x&state={state}"), &[])
            .await;
    assert_eq!(
        location(&r),
        "/#/settings/accounts?oauth_error=invalid_state"
    );
}

#[tokio::test]
async fn the_full_flow_finishes_for_the_same_login_only() {
    let mut s = setup().await;
    let h = s.h.clone();
    let code = "canary-code-5d90";
    let (_, req) = start(&mut s, &h, "jira", JIRA_URL).await;
    let state = state_of(&req.unwrap());
    let cb =
        s.c.get(&format!("/oauth/callback?code={code}&state={state}"), &[])
            .await;
    let id = location(&cb).split("finish=").nth(1).unwrap().to_owned();
    // Another owner login is not the login that started it.
    let other = owner(&s.c).await;
    expect(&finish(&s.c, &other, &id).await, 403, "wrong_session");
    // A token is refused whatever its scopes.
    let (_, t) =
        s.c.store
            .create_token(
                "ops".into(),
                BTreeSet::from([Scope::Read, Scope::Drive, Scope::Admin]),
                DAY,
            )
            .await
            .unwrap();
    expect(
        &finish(&s.c, &token_headers(t.expose()), &id).await,
        403,
        "owner_only",
    );
    no_request(&mut s.station, Duration::from_millis(300)).await;

    let (r, req) = exchange(&mut s.station, finish(&s.c, &h, &id), ok(account("jira"))).await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body, account("jira"));
    assert_eq!(req.method, "accounts.oauth_finish");
    assert_eq!(req.params, json!({ "state": state, "code": code }));
    // The transaction is used.
    expect(&finish(&s.c, &h, &id).await, 404, "unknown_transaction");
    expect(
        &s.c.post(
            "/api/accounts/oauth/finish",
            &refs(&h),
            json!({ "transaction": "nope" }),
        )
        .await,
        404,
        "unknown_transaction",
    );

    // No audit row and no column of deck.db holds the state or the code.
    let audit = format!("{:?}", s.c.store.audit_log(100).await.unwrap());
    assert!(audit.contains("accounts.oauth_finish"));
    let on_disk = database_text(s.dir.path());
    for canary in [code, state.as_str()] {
        assert!(
            !audit.contains(canary) && !on_disk.contains(canary),
            "{canary}"
        );
    }
    assert!(
        !audit.contains(&id),
        "the transaction id stays out of the audit log"
    );
}

#[tokio::test]
async fn a_finish_needs_the_callback_and_a_fresh_code() {
    let mut s = setup().await;
    let h = s.h.clone();
    let (_, req) = start(&mut s, &h, "linear", "https://linear.app/oauth/authorize").await;
    let state = state_of(&req.unwrap());
    // Run the callback, then let the code expire (120 s).
    let cb =
        s.c.get(&format!("/oauth/callback?code=c1&state={state}"), &[])
            .await;
    let id = location(&cb).split("finish=").nth(1).unwrap().to_owned();
    s.f.det.advance(Duration::from_secs(121));
    let r =
        s.c.post(
            "/api/accounts/oauth/finish",
            &refs(&h),
            json!({ "transaction": id }),
        )
        .await;
    expect(&r, 409, "not_ready");
    // The 10 minute transaction ends too.
    let (_, req) = start(&mut s, &h, "linear", "https://linear.app/oauth/authorize").await;
    let state = state_of(&req.unwrap());
    let cb =
        s.c.get(&format!("/oauth/callback?code=c2&state={state}"), &[])
            .await;
    let id = location(&cb).split("finish=").nth(1).unwrap().to_owned();
    s.f.det.advance(Duration::from_secs(601));
    let r =
        s.c.post(
            "/api/accounts/oauth/finish",
            &refs(&h),
            json!({ "transaction": id }),
        )
        .await;
    expect(&r, 404, "unknown_transaction");
}

/// DL-185: the two codes of the built-in Linear app reach the board as a 409
/// with their fixed text; the station text does not.
#[tokio::test]
async fn the_builtin_app_codes_reach_the_board_with_their_fixed_text() {
    const CANARY: &str = "station-text-canary-185";
    let mut s = setup().await;
    for (code, text) in [
        (
            "builtin_needs_localhost",
            "The built-in Linear app works only from the board on the deck machine at https://127.0.0.1:7420 or https://localhost:7420. Open the board there, or set up your own Linear app.",
        ),
        (
            "app_changed",
            "The Linear app of this account changed. Connect the account again.",
        ),
    ] {
        let h = s.h.clone();
        let headers = refs(&h);
        let call = s.c.post(
            "/api/accounts/oauth/start",
            &headers,
            json!({ "station": "fake", "kind": "linear" }),
        );
        let (r, _) = exchange(
            &mut s.station,
            call,
            Outcome::Err(WireError::new(code, format!("the station says {CANARY}"))),
        )
        .await;
        expect(&r, 409, code);
        assert_eq!(r.body["message"], text, "{}", r.text);
        assert!(!r.text.contains(CANARY), "{}", r.text);
    }
}
