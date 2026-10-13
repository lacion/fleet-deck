//! The OAuth app, the authorize URL, the code exchange and the refresh,
//! with a fake `curl`. The fake logs its argv: no secret may be in it.

use std::path::Path;

use fleetdeck_core::Secret;
use fleetdeck_proto::accounts::{
    AccountKind, AccountMethod, AccountStatus, AppSet, OAuthFinishParams, OAuthStartParams,
};

use super::store::StoredAccount;
use super::test_support::{curl, log, programs, service};
use super::{Accounts, now_secs};

const SECRET: &str = "client-secret-canary-1";
const CODE: &str = "auth-code-canary-2";
const ACCESS: &str = "access-canary-3";
const REFRESH: &str = "refresh-canary-4";
const ACCESS2: &str = "access-canary-5";
const REFRESH2: &str = "refresh-canary-6";
const STATE: &str = "state_0123456789abcdefghijklmnopqrstu";
const REDIRECT: &str = "https://board.example/oauth/callback";

fn svc(tmp: &Path, cases: &[(&str, &str, u16)]) -> Accounts {
    let mut p = programs(tmp);
    p.curl = curl(&tmp.join("curl"), cases);
    service(tmp, p)
}

fn set_app(a: &Accounts, kind: AccountKind) {
    a.app_set(AppSet {
        kind,
        client_id: "client-1".into(),
        client_secret: SECRET.into(),
    })
    .unwrap();
}

fn start(a: &Accounts, kind: AccountKind) -> String {
    a.oauth_start(OAuthStartParams {
        kind,
        state: STATE.into(),
        redirect_uri: REDIRECT.into(),
    })
    .unwrap()
    .authorize_url
}

fn finish(code: &str) -> OAuthFinishParams {
    OAuthFinishParams {
        state: STATE.into(),
        code: code.into(),
    }
}

fn token_answer() -> String {
    format!(
        r#"{{"access_token":"{ACCESS}","refresh_token":"{REFRESH}","expires_in":3600,"scope":"x"}}"#
    )
}

/// No secret of the flow is in the argv of any curl call.
fn assert_no_secret_in_argv(tmp: &Path) {
    let argv = log(&tmp.join("curl"), "argv.log");
    assert!(!argv.is_empty());
    for canary in [SECRET, CODE, ACCESS, REFRESH, ACCESS2, REFRESH2] {
        assert!(!argv.contains(canary), "{canary} is in argv: {argv}");
    }
    assert!(argv.lines().all(|l| l == "-sS -K -"), "{argv}");
}

#[test]
fn the_app_has_a_generation_that_grows_and_no_secret_in_its_view() {
    let tmp = tempfile::tempdir().unwrap();
    let a = svc(tmp.path(), &[]);
    let first = a
        .app_set(AppSet {
            kind: AccountKind::Jira,
            client_id: "client-1".into(),
            client_secret: SECRET.into(),
        })
        .unwrap();
    assert_eq!((first.generation, first.has_secret), (1, true));
    let second = a
        .app_set(AppSet {
            kind: AccountKind::Jira,
            client_id: "client-2".into(),
            client_secret: SECRET.into(),
        })
        .unwrap();
    assert_eq!(
        (second.generation, second.client_id.as_str()),
        (2, "client-2")
    );
    assert!(!format!("{first:?}{second:?}").contains(SECRET));
    for bad in [AccountKind::Github, AccountKind::Gitlab] {
        let err = a
            .app_set(AppSet {
                kind: bad,
                client_id: "c".into(),
                client_secret: "s".into(),
            })
            .unwrap_err();
        assert_eq!(err.code, "invalid_params");
    }
    let err = a
        .app_set(AppSet {
            kind: AccountKind::Linear,
            client_id: String::new(),
            client_secret: "s".into(),
        })
        .unwrap_err();
    assert_eq!(err.code, "invalid_params");
}

#[test]
fn the_authorize_urls_have_the_documented_form() {
    let tmp = tempfile::tempdir().unwrap();
    let a = svc(tmp.path(), &[]);
    set_app(&a, AccountKind::Jira);
    set_app(&a, AccountKind::Linear);
    let jira = start(&a, AccountKind::Jira);
    assert_eq!(
        jira,
        format!(
            "https://auth.atlassian.com/authorize?audience=api.atlassian.com&client_id=client-1&scope=read%3Ajira-work%20write%3Ajira-work%20read%3Ajira-user%20offline_access&redirect_uri=https%3A%2F%2Fboard.example%2Foauth%2Fcallback&state={STATE}&response_type=code&prompt=consent"
        )
    );
    assert!(!jira.contains(SECRET) && !jira.contains("code_challenge"));
    let linear = start(&a, AccountKind::Linear);
    assert!(linear.starts_with("https://linear.app/oauth/authorize?client_id=client-1&redirect_uri=https%3A%2F%2Fboard.example%2Foauth%2Fcallback&response_type=code&scope=read,write&state="), "{linear}");
    assert!(linear.contains("&code_challenge_method=S256"));
    let challenge = linear
        .split("code_challenge=")
        .nth(1)
        .unwrap()
        .split('&')
        .next()
        .unwrap();
    assert_eq!(challenge.len(), 43, "base64url of a SHA-256: {challenge}");
    assert!(!linear.contains(SECRET));
}

#[test]
fn a_start_needs_an_app_a_valid_state_and_a_valid_redirect() {
    let tmp = tempfile::tempdir().unwrap();
    let a = svc(tmp.path(), &[]);
    let params = |state: &str, redirect: &str| OAuthStartParams {
        kind: AccountKind::Jira,
        state: state.into(),
        redirect_uri: redirect.into(),
    };
    let err = a.oauth_start(params(STATE, REDIRECT)).unwrap_err();
    assert_eq!(err.code, "unavailable");
    set_app(&a, AccountKind::Jira);
    for (s, r) in [
        ("short", REDIRECT),
        (STATE, "https://board.example/other"),
        (STATE, "https://u:p@board.example/oauth/callback"),
        ("bad state with spaces 0123456789", REDIRECT),
    ] {
        assert_eq!(
            a.oauth_start(params(s, r)).unwrap_err().code,
            "invalid_params"
        );
    }
    let err = a
        .oauth_start(OAuthStartParams {
            kind: AccountKind::Github,
            state: STATE.into(),
            redirect_uri: REDIRECT.into(),
        })
        .unwrap_err();
    assert_eq!(err.code, "invalid_params");
}

const RESOURCES: &str = r#"[{"id":"cloud-1","url":"https://acme.atlassian.net","name":"Acme"},{"id":"cloud-2","url":"https://beta.atlassian.net","name":"Beta"},{"id":"bad id","url":"https://x.atlassian.net"},{"id":"cloud-3","url":"http://insecure.example"}]"#;

fn jira_cases() -> Vec<(&'static str, String, u16)> {
    vec![
        // First: its URL holds `/oauth/token` too.
        ("accessible-resources", RESOURCES.to_owned(), 200),
        ("auth.atlassian.com/oauth/token", token_answer(), 200),
        (
            "/ex/jira/cloud-1/rest/api/3/myself",
            r#"{"displayName":"Ana"}"#.to_owned(),
            200,
        ),
    ]
}

fn jira_service(tmp: &Path) -> Accounts {
    let cases = jira_cases();
    let refs: Vec<(&str, &str, u16)> = cases.iter().map(|(p, b, s)| (*p, b.as_str(), *s)).collect();
    svc(tmp, &refs)
}

#[tokio::test]
async fn a_jira_sign_in_makes_one_account_for_each_site_and_keeps_secrets_out_of_argv() {
    let tmp = tempfile::tempdir().unwrap();
    let a = jira_service(tmp.path());
    set_app(&a, AccountKind::Jira);
    start(&a, AccountKind::Jira);
    let view = a.oauth_finish(finish(CODE)).await.unwrap();
    assert_eq!(
        (view.kind, view.method, view.status),
        (AccountKind::Jira, AccountMethod::Oauth, AccountStatus::Ok)
    );
    assert_eq!(view.site.as_deref(), Some("https://acme.atlassian.net"));
    assert_eq!(view.user.as_deref(), Some("Ana"));
    assert!(view.account_id.starts_with("jira-"));
    assert!(!format!("{view:?}").contains(ACCESS));
    let store = a.store().unwrap();
    let ids = store.account_ids().unwrap();
    assert_eq!(ids.len(), 2, "two valid sites: {ids:?}");
    let mut sites: Vec<_> = ids
        .iter()
        .map(|id| {
            let s = store.account(id).unwrap().unwrap();
            assert_eq!(s.access_token.expose(), ACCESS);
            assert_eq!(s.refresh_token.as_ref().unwrap().expose(), REFRESH);
            assert!(s.expires_at.unwrap() > now_secs());
            (s.site.unwrap(), s.cloud_id.unwrap())
        })
        .collect();
    sites.sort();
    assert_eq!(
        sites,
        [
            (
                "https://acme.atlassian.net".to_owned(),
                "cloud-1".to_owned()
            ),
            (
                "https://beta.atlassian.net".to_owned(),
                "cloud-2".to_owned()
            )
        ]
    );
    // The token request carries the secret and the code on stdin only.
    let config = log(&tmp.path().join("curl"), "stdin.log");
    assert!(config.contains("https://auth.atlassian.com/oauth/token"));
    assert!(config.contains(SECRET) && config.contains(CODE));
    assert!(config.contains("authorization_code"));
    assert!(config.contains("Authorization: Bearer access-canary-3"));
    assert_no_secret_in_argv(tmp.path());
    // The list shows both accounts and the app, and no secret.
    let list = a
        .call("accounts.list", serde_json::json!({}), &[])
        .await
        .unwrap();
    let text = list.to_string();
    for canary in [SECRET, ACCESS, REFRESH] {
        assert!(!text.contains(canary), "{text}");
    }
    assert_eq!(list["apps"][0]["client_id"], "client-1");
    assert_eq!(list["apps"][0]["has_secret"], true);
}

#[tokio::test]
async fn a_second_sign_in_updates_the_same_accounts() {
    let tmp = tempfile::tempdir().unwrap();
    let a = jira_service(tmp.path());
    set_app(&a, AccountKind::Jira);
    start(&a, AccountKind::Jira);
    let first = a.oauth_finish(finish(CODE)).await.unwrap();
    start(&a, AccountKind::Jira);
    let second = a.oauth_finish(finish(CODE)).await.unwrap();
    assert_eq!(first.account_id, second.account_id);
    assert_eq!(a.store().unwrap().account_ids().unwrap().len(), 2);
}

#[tokio::test]
async fn a_state_works_once_and_only_for_the_app_that_started_it() {
    let tmp = tempfile::tempdir().unwrap();
    let a = jira_service(tmp.path());
    set_app(&a, AccountKind::Jira);
    start(&a, AccountKind::Jira);
    a.oauth_finish(finish(CODE)).await.unwrap();
    let err = a.oauth_finish(finish(CODE)).await.unwrap_err();
    assert_eq!(err.code, "invalid_params", "a used state");
    // The owner changes the app: the open transaction is dead.
    start(&a, AccountKind::Jira);
    set_app(&a, AccountKind::Jira);
    let err = a.oauth_finish(finish(CODE)).await.unwrap_err();
    assert_eq!(err.code, "invalid_params", "an old generation");
    // An unknown state and a bad code.
    let err = a
        .oauth_finish(OAuthFinishParams {
            state: "unknown_state_0123456789abcdef".into(),
            code: CODE.into(),
        })
        .await
        .unwrap_err();
    assert_eq!(err.code, "invalid_params");
    start(&a, AccountKind::Jira);
    let err = a.oauth_finish(finish("bad code\n")).await.unwrap_err();
    assert_eq!(err.code, "invalid_params");
}

#[tokio::test]
async fn an_expired_transaction_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let mut a = jira_service(tmp.path());
    a.limits.oauth_ttl = std::time::Duration::from_millis(50);
    set_app(&a, AccountKind::Jira);
    start(&a, AccountKind::Jira);
    tokio::time::sleep(std::time::Duration::from_millis(120)).await;
    let err = a.oauth_finish(finish(CODE)).await.unwrap_err();
    assert_eq!(err.code, "invalid_params");
}

#[tokio::test]
async fn a_provider_error_is_a_fixed_code_with_the_status_and_no_body() {
    let tmp = tempfile::tempdir().unwrap();
    let a = svc(
        tmp.path(),
        &[(
            "/oauth/token",
            r#"{"error":"invalid_grant","error_description":"body-text-canary client-secret-canary-1"}"#,
            400,
        )],
    );
    set_app(&a, AccountKind::Jira);
    start(&a, AccountKind::Jira);
    let err = a.oauth_finish(finish(CODE)).await.unwrap_err();
    assert_eq!(err.code, "upstream");
    assert_eq!(err.message, "the server answered HTTP 400");
    assert_no_secret_in_argv(tmp.path());
}

#[tokio::test]
async fn a_linear_sign_in_sends_the_verifier_and_stores_the_workspace() {
    let tmp = tempfile::tempdir().unwrap();
    let a = svc(
        tmp.path(),
        &[
            ("api.linear.app/oauth/token", &token_answer(), 200),
            (
                "viewer",
                r#"{"data":{"viewer":{"name":"Ana L","organization":{"name":"Acme","urlKey":"acme"}}}}"#,
                200,
            ),
        ],
    );
    set_app(&a, AccountKind::Linear);
    let url = start(&a, AccountKind::Linear);
    let view = a.oauth_finish(finish(CODE)).await.unwrap();
    assert_eq!(view.site.as_deref(), Some("https://linear.app/acme"));
    assert_eq!(view.user.as_deref(), Some("Ana L"));
    let config = log(&tmp.path().join("curl"), "stdin.log");
    let body = config
        .lines()
        .find(|l| l.starts_with("data-binary = \"grant_type"))
        .unwrap();
    assert!(body.contains("code_verifier="), "{body}");
    assert!(
        body.contains(&format!("code={CODE}")) && body.contains(&format!("client_secret={SECRET}"))
    );
    assert!(body.contains("redirect_uri=https%3A%2F%2Fboard.example%2Foauth%2Fcallback"));
    assert!(config.contains("Content-Type: application/x-www-form-urlencoded"));
    // The challenge of the URL is the S256 of the verifier that was sent.
    use base64::Engine;
    let verifier = body
        .split("code_verifier=")
        .nth(1)
        .unwrap()
        .split('&')
        .next()
        .unwrap()
        .trim_end_matches('"');
    let expect = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(ring::digest::digest(
        &ring::digest::SHA256,
        verifier.as_bytes(),
    ));
    assert!(
        url.contains(&format!("code_challenge={expect}")),
        "{url} {expect}"
    );
    assert_no_secret_in_argv(tmp.path());
}

fn stored(kind: AccountKind, expires_in: i64, site: &str, cloud: Option<&str>) -> StoredAccount {
    StoredAccount {
        kind,
        method: AccountMethod::Oauth,
        site: Some(site.into()),
        cloud_id: cloud.map(str::to_owned),
        user: "Ana".into(),
        access_token: Secret::new("old-access".into()),
        refresh_token: Some(Secret::new("old-refresh".into())),
        expires_at: Some(now_secs() + expires_in),
        email: None,
    }
}

fn refresh_answer() -> String {
    format!(r#"{{"access_token":"{ACCESS2}","refresh_token":"{REFRESH2}","expires_in":3600}}"#)
}

#[tokio::test]
async fn a_token_that_ends_soon_is_refreshed_and_the_rotated_pair_is_stored_first() {
    let tmp = tempfile::tempdir().unwrap();
    let a = svc(tmp.path(), &[("refresh_token", &refresh_answer(), 200)]);
    set_app(&a, AccountKind::Jira);
    let store = a.store().unwrap();
    store
        .put_account(
            "jira-00000001",
            &stored(AccountKind::Jira, 30, "https://a.atlassian.net", Some("c1")),
        )
        .unwrap();
    // A second site of the same sign-in shares the refresh token.
    store
        .put_account(
            "jira-00000002",
            &stored(AccountKind::Jira, 30, "https://b.atlassian.net", Some("c2")),
        )
        .unwrap();
    // An account of another sign-in does not.
    let mut other = stored(AccountKind::Jira, 30, "https://c.atlassian.net", Some("c3"));
    other.refresh_token = Some(Secret::new("other-refresh".into()));
    store.put_account("jira-00000003", &other).unwrap();
    let token = a.fresh("jira-00000001").await.unwrap();
    assert_eq!(token.expose(), ACCESS2);
    let one = store.account("jira-00000001").unwrap().unwrap();
    assert_eq!(one.refresh_token.unwrap().expose(), REFRESH2);
    let two = store.account("jira-00000002").unwrap().unwrap();
    assert_eq!(two.access_token.expose(), ACCESS2);
    assert_eq!(two.refresh_token.unwrap().expose(), REFRESH2);
    let three = store.account("jira-00000003").unwrap().unwrap();
    assert_eq!(three.access_token.expose(), "old-access");
    // The request has the old refresh token and the secret on stdin.
    let config = log(&tmp.path().join("curl"), "stdin.log");
    assert!(
        config.contains("old-refresh") && config.contains(SECRET),
        "{config}"
    );
    // The second site is fresh now: no second call.
    let calls = log(&tmp.path().join("curl"), "argv.log").lines().count();
    a.fresh("jira-00000002").await.unwrap();
    assert_eq!(
        log(&tmp.path().join("curl"), "argv.log").lines().count(),
        calls
    );
    assert_no_secret_in_argv(tmp.path());
}

#[tokio::test]
async fn a_token_with_time_left_is_not_refreshed() {
    let tmp = tempfile::tempdir().unwrap();
    let a = svc(tmp.path(), &[]);
    a.store()
        .unwrap()
        .put_account(
            "linear-00000001",
            &stored(AccountKind::Linear, 3600, "https://linear.app/acme", None),
        )
        .unwrap();
    assert_eq!(
        a.fresh("linear-00000001").await.unwrap().expose(),
        "old-access"
    );
    assert!(log(&tmp.path().join("curl"), "argv.log").is_empty());
}

#[tokio::test]
async fn a_failed_refresh_marks_the_account_and_a_later_success_clears_it() {
    let tmp = tempfile::tempdir().unwrap();
    let a = svc(
        tmp.path(),
        &[(
            "refresh_token=old-refresh",
            r#"{"error":"invalid_grant"}"#,
            400,
        )],
    );
    set_app(&a, AccountKind::Linear);
    a.store()
        .unwrap()
        .put_account(
            "linear-00000001",
            &stored(AccountKind::Linear, -10, "https://linear.app/acme", None),
        )
        .unwrap();
    let err = a.fresh("linear-00000001").await.unwrap_err();
    assert_eq!(err.code, "unavailable");
    assert!(!err.message.contains("invalid_grant"), "{err:?}");
    let list = a
        .call("accounts.list", serde_json::json!({}), &[])
        .await
        .unwrap();
    let row = list["accounts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["account_id"] == "linear-00000001")
        .unwrap();
    assert_eq!(row["status"], "needs_login");
    // A new sign-in replaces the account and clears the mark.
    a.set_needs_login("linear-00000001", false);
    assert_eq!(
        a.view(
            "linear-00000001",
            &stored(AccountKind::Linear, 1, "x", None)
        )
        .status,
        AccountStatus::Ok
    );
}

#[tokio::test]
async fn two_calls_at_once_refresh_once() {
    let tmp = tempfile::tempdir().unwrap();
    let a = std::sync::Arc::new(svc(
        tmp.path(),
        &[("refresh_token", &refresh_answer(), 200)],
    ));
    set_app(&a, AccountKind::Jira);
    a.store()
        .unwrap()
        .put_account(
            "jira-00000001",
            &stored(AccountKind::Jira, 30, "https://a.atlassian.net", Some("c1")),
        )
        .unwrap();
    let (x, y) = tokio::join!(a.fresh("jira-00000001"), a.fresh("jira-00000001"));
    assert_eq!(x.unwrap().expose(), ACCESS2);
    assert_eq!(y.unwrap().expose(), ACCESS2);
    let calls = log(&tmp.path().join("curl"), "argv.log").lines().count();
    assert_eq!(calls, 1);
}
