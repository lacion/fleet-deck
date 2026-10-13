//! Token accounts, the list, the logout, the account-backed trackers (the
//! four auth schemes, the Atlassian gateway, the assets) and
//! `issues.containers`.

use std::path::Path;

use fleetdeck_core::Secret;
use fleetdeck_core::config::{TokenSource, TrackerConfig};
use fleetdeck_proto::accounts::{AccountKind, AccountMethod, AccountStatus, TokenSet};
use fleetdeck_proto::integrations::{Binding, TrackerBinding, TrackerKind};
use fleetdeck_proto::link::Outcome;
use serde_json::{Value, json};

use super::store::{AccountStore, StoredAccount};
use super::test_support::{curl, log, programs};
use super::{Accounts, now_secs};
use crate::integrations::Integrations;
use crate::integrations::tracker::header_for;

const TOKEN: &str = "jira-token-canary-1";
const KEY: &str = "lin_api_canary_2";
const ACCESS: &str = "oauth-access-canary-3";

fn integrations(
    tmp: &Path,
    cases: &[(&str, &str, u16)],
    trackers: Vec<TrackerConfig>,
) -> Integrations {
    let mut p = programs(tmp);
    p.curl = curl(&tmp.join("curl"), cases);
    let svc = Integrations::with_programs(trackers, p);
    svc.accounts()
        .attach(AccountStore::open(&tmp.join("data").join("accounts")).unwrap());
    svc
}

async fn ask(svc: &Integrations, method: &str, params: Value) -> Result<Value, (String, String)> {
    match svc.call_accounts(method, params).await {
        Outcome::Ok(v) => Ok(v),
        Outcome::Err(e) => Err((e.code, e.message)),
    }
}

fn put(svc: &Integrations, id: &str, a: &StoredAccount) {
    svc.accounts().store().unwrap().put_account(id, a).unwrap();
}

fn account(kind: AccountKind, method: AccountMethod) -> StoredAccount {
    StoredAccount {
        kind,
        method,
        site: None,
        cloud_id: None,
        user: "Ana".into(),
        access_token: Secret::new(ACCESS.into()),
        refresh_token: None,
        expires_at: None,
        email: None,
    }
}

fn jira_oauth() -> StoredAccount {
    StoredAccount {
        site: Some("https://acme.atlassian.net".into()),
        cloud_id: Some("cloud-1".into()),
        expires_at: Some(now_secs() + 3600),
        ..account(AccountKind::Jira, AccountMethod::Oauth)
    }
}

fn jira_token() -> StoredAccount {
    StoredAccount {
        site: Some("https://acme.atlassian.net".into()),
        email: Some("ana@acme.test".into()),
        access_token: Secret::new(TOKEN.into()),
        ..account(AccountKind::Jira, AccountMethod::Token)
    }
}

fn binding(id: &str, kind: TrackerKind, key: &str) -> Binding {
    Binding::Tracker(TrackerBinding {
        tracker: id.into(),
        kind,
        key: key.into(),
    })
}

#[test]
fn the_four_auth_schemes_have_their_own_headers() {
    let (h, secrets) = header_for(
        TrackerKind::Jira,
        AccountMethod::Token,
        Some("a@b.c"),
        "tok",
    );
    assert_eq!(h, "Authorization: Basic YUBiLmM6dG9r");
    assert!(secrets.contains(&"tok".to_owned()) && secrets.contains(&"YUBiLmM6dG9r".to_owned()));
    let (h, _) = header_for(
        TrackerKind::Jira,
        AccountMethod::Config,
        Some("a@b.c"),
        "tok",
    );
    assert_eq!(h, "Authorization: Basic YUBiLmM6dG9r");
    let (h, secrets) = header_for(TrackerKind::Jira, AccountMethod::Oauth, None, "acc");
    assert_eq!(h, "Authorization: Bearer acc");
    assert_eq!(secrets, ["acc"]);
    let (h, _) = header_for(TrackerKind::Linear, AccountMethod::Oauth, None, "acc");
    assert_eq!(h, "Authorization: Bearer acc");
    let (h, _) = header_for(TrackerKind::Linear, AccountMethod::Token, None, "lin_api_x");
    assert_eq!(h, "Authorization: lin_api_x");
}

#[tokio::test]
async fn a_jira_token_is_checked_with_one_call_then_stored() {
    let tmp = tempfile::tempdir().unwrap();
    let svc = integrations(
        tmp.path(),
        &[("/rest/api/3/myself", r#"{"displayName":"Ana"}"#, 200)],
        Vec::new(),
    );
    let view = ask(
        &svc,
        "accounts.token_set",
        json!({"kind": "jira", "site": "https://Acme.atlassian.net/", "email": "ana@acme.test", "token": TOKEN}),
    )
    .await
    .unwrap();
    assert_eq!(view["method"], "token");
    assert_eq!(view["site"], "https://acme.atlassian.net");
    assert_eq!(view["user"], "Ana");
    assert!(!view.to_string().contains(TOKEN));
    let id = view["account_id"].as_str().unwrap().to_owned();
    let stored = svc
        .accounts()
        .store()
        .unwrap()
        .account(&id)
        .unwrap()
        .unwrap();
    assert_eq!(stored.access_token.expose(), TOKEN);
    assert_eq!(stored.email.as_deref(), Some("ana@acme.test"));
    let curl_dir = tmp.path().join("curl");
    let config = log(&curl_dir, "stdin.log");
    assert!(config.contains("url = \"https://acme.atlassian.net/rest/api/3/myself\""));
    assert!(config.contains("Authorization: Basic "));
    assert!(!log(&curl_dir, "argv.log").contains(TOKEN));
    assert_eq!(
        log(&curl_dir, "argv.log").lines().count(),
        1,
        "one identity call"
    );
    // The same token again keeps the account id.
    let again = ask(
        &svc,
        "accounts.token_set",
        json!({"kind": "jira", "site": "https://acme.atlassian.net", "email": "ana@acme.test", "token": TOKEN}),
    )
    .await
    .unwrap();
    assert_eq!(again["account_id"], view["account_id"]);
}

#[tokio::test]
async fn a_refused_token_is_not_stored_and_the_error_holds_no_secret() {
    let tmp = tempfile::tempdir().unwrap();
    let svc = integrations(
        tmp.path(),
        &[(
            "/rest/api/3/myself",
            r#"{"message":"body-canary jira-token-canary-1"}"#,
            401,
        )],
        Vec::new(),
    );
    let (code, message) = ask(
        &svc,
        "accounts.token_set",
        json!({"kind": "jira", "site": "https://acme.atlassian.net", "email": "ana@acme.test", "token": TOKEN}),
    )
    .await
    .unwrap_err();
    assert_eq!(code, "unavailable");
    assert!(!message.contains("canary"), "{message}");
    assert!(
        svc.accounts()
            .store()
            .unwrap()
            .account_ids()
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn a_linear_key_is_checked_and_stored_with_its_workspace() {
    let tmp = tempfile::tempdir().unwrap();
    let svc = integrations(
        tmp.path(),
        &[(
            "viewer",
            r#"{"data":{"viewer":{"name":"Ana L","organization":{"urlKey":"acme"}}}}"#,
            200,
        )],
        Vec::new(),
    );
    let view = ask(
        &svc,
        "accounts.token_set",
        json!({"kind": "linear", "token": KEY}),
    )
    .await
    .unwrap();
    assert_eq!(view["site"], "https://linear.app/acme");
    assert_eq!(view["user"], "Ana L");
    let config = log(&tmp.path().join("curl"), "stdin.log");
    assert!(
        config.contains(&format!("header = \"Authorization: {KEY}\"")),
        "{config}"
    );
    assert!(!log(&tmp.path().join("curl"), "argv.log").contains(KEY));
}

#[tokio::test]
async fn token_params_are_validated() {
    let tmp = tempfile::tempdir().unwrap();
    let svc = integrations(tmp.path(), &[], Vec::new());
    for params in [
        json!({"kind": "jira", "site": "http://acme.atlassian.net", "email": "a@b.c", "token": "t"}),
        json!({"kind": "jira", "site": "https://acme.atlassian.net/path", "email": "a@b.c", "token": "t"}),
        json!({"kind": "jira", "site": "https://acme.atlassian.net", "token": "t"}),
        json!({"kind": "jira", "site": "https://acme.atlassian.net", "email": "no-at", "token": "t"}),
        json!({"kind": "jira", "site": "https://acme.atlassian.net", "email": "a@b.c", "token": ""}),
        json!({"kind": "jira", "site": "https://acme.atlassian.net", "email": "a@b.c", "token": "t\nx"}),
        json!({"kind": "linear", "site": "https://x.example", "token": "t"}),
        json!({"kind": "linear", "email": "a@b.c", "token": "t"}),
        json!({"kind": "github", "email": "a@b.c", "token": "t"}),
        json!({"kind": "jira", "token": "t", "extra": 1}),
    ] {
        let (code, message) = ask(&svc, "accounts.token_set", params.clone())
            .await
            .unwrap_err();
        assert_eq!(code, "invalid_params", "{params}");
        assert!(!message.contains("\"t\""), "{message}");
    }
    assert!(log(&tmp.path().join("curl"), "argv.log").is_empty());
}

#[tokio::test]
async fn a_parse_error_never_quotes_a_secret() {
    let tmp = tempfile::tempdir().unwrap();
    let svc = integrations(tmp.path(), &[], Vec::new());
    for (method, params) in [
        (
            "accounts.token_set",
            json!({"kind": "nope", "token": "secret-canary-9"}),
        ),
        (
            "accounts.app_set",
            json!({"kind": "jira", "client_id": 7, "client_secret": "secret-canary-9"}),
        ),
        (
            "accounts.oauth_finish",
            json!({"state": 1, "code": "secret-canary-9"}),
        ),
    ] {
        let (code, message) = ask(&svc, method, params).await.unwrap_err();
        assert_eq!(code, "invalid_params");
        assert!(!message.contains("canary"), "{message}");
    }
}

fn config_tracker(id: &str, kind: TrackerKind, file: &Path) -> TrackerConfig {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(file, "config-token-canary\n").unwrap();
    std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o600)).unwrap();
    TrackerConfig {
        id: id.into(),
        kind,
        site: (kind == TrackerKind::Jira).then(|| "https://cfg.atlassian.net".into()),
        email: (kind == TrackerKind::Jira).then(|| "cfg@acme.test".into()),
        token: TokenSource::File(file.to_owned()),
    }
}

#[tokio::test]
async fn the_list_shows_config_entries_store_accounts_apps_and_env_token_names() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = config_tracker("acme", TrackerKind::Jira, &tmp.path().join("t"));
    let mut svc_programs = programs(tmp.path());
    svc_programs.curl = curl(&tmp.path().join("curl"), &[]);
    let svc = Integrations::with_programs(vec![cfg], svc_programs);
    svc.accounts()
        .attach(AccountStore::open(&tmp.path().join("data").join("accounts")).unwrap());
    put(&svc, "jira-0000000a", &jira_token());
    ask(
        &svc,
        "accounts.app_set",
        json!({"kind": "linear", "client_id": "lin-client", "client_secret": "s"}),
    )
    .await
    .unwrap();
    let list = ask(&svc, "accounts.list", json!({})).await.unwrap();
    let rows = list["accounts"].as_array().unwrap();
    let find = |id: &str| rows.iter().find(|r| r["account_id"] == id).unwrap();
    assert_eq!(find("acme")["method"], "config");
    assert_eq!(find("acme")["site"], "https://cfg.atlassian.net");
    assert_eq!(find("jira-0000000a")["method"], "token");
    assert_eq!(list["apps"][0]["client_id"], "lin-client");
    assert!(list["env_tokens"].is_array());
    let text = list.to_string();
    assert!(!text.contains(TOKEN) && !text.contains("config-token-canary"));
    // The config entry cannot be signed out.
    let (code, _) = ask(&svc, "accounts.logout", json!({"account_id": "acme"}))
        .await
        .unwrap_err();
    assert_eq!(code, "invalid_params");
    // A store account can.
    ask(
        &svc,
        "accounts.logout",
        json!({"account_id": "jira-0000000a"}),
    )
    .await
    .unwrap();
    assert!(svc.accounts().meta("jira-0000000a").unwrap().is_none());
}

const SEARCH: &str = r#"{"issues":[{"key":"PROJ-1","fields":{"summary":"S","status":{"name":"Todo"},"updated":"2024-05-01T12:00:00.000+0200"}}]}"#;

#[tokio::test]
async fn a_jira_oauth_account_uses_bearer_the_gateway_and_the_site_for_display() {
    let tmp = tempfile::tempdir().unwrap();
    let svc = integrations(tmp.path(), &[("search/jql", SEARCH, 200)], Vec::new());
    put(&svc, "jira-0000000b", &jira_oauth());
    let found = svc
        .call(
            "issues.search",
            json!({"binding": binding("jira-0000000b", TrackerKind::Jira, "PROJ")}),
            &|_| None,
        )
        .await;
    let Outcome::Ok(found) = found else {
        panic!("{found:?}");
    };
    // The display URL is on the site, not on the gateway.
    assert_eq!(
        found["issues"][0]["ref"]["url"],
        "https://acme.atlassian.net/browse/PROJ-1"
    );
    let config = log(&tmp.path().join("curl"), "stdin.log");
    assert!(
        config
            .contains("url = \"https://api.atlassian.com/ex/jira/cloud-1/rest/api/3/search/jql\""),
        "{config}"
    );
    assert!(
        config.contains(&format!("header = \"Authorization: Bearer {ACCESS}\"")),
        "{config}"
    );
    assert!(!log(&tmp.path().join("curl"), "argv.log").contains(ACCESS));
}

#[tokio::test]
async fn a_jira_token_account_uses_basic_on_the_site() {
    let tmp = tempfile::tempdir().unwrap();
    let svc = integrations(tmp.path(), &[("search/jql", SEARCH, 200)], Vec::new());
    put(&svc, "jira-0000000c", &jira_token());
    let out = svc
        .call(
            "issues.search",
            json!({"binding": binding("jira-0000000c", TrackerKind::Jira, "PROJ")}),
            &|_| None,
        )
        .await;
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
    let config = log(&tmp.path().join("curl"), "stdin.log");
    assert!(
        config.contains("url = \"https://acme.atlassian.net/rest/api/3/search/jql\""),
        "{config}"
    );
    assert!(config.contains("Authorization: Basic "), "{config}");
    assert!(!config.contains("Bearer"));
}

#[tokio::test]
async fn linear_accounts_use_bearer_for_oauth_and_the_raw_key_for_a_token() {
    let tmp = tempfile::tempdir().unwrap();
    let svc = integrations(
        tmp.path(),
        &[("issues(filter", r#"{"data":{"issues":{"nodes":[]}}}"#, 200)],
        Vec::new(),
    );
    let mut oauth = account(AccountKind::Linear, AccountMethod::Oauth);
    oauth.expires_at = Some(now_secs() + 3600);
    put(&svc, "linear-0000000d", &oauth);
    let mut key = account(AccountKind::Linear, AccountMethod::Token);
    key.access_token = Secret::new(KEY.into());
    put(&svc, "linear-0000000e", &key);
    for id in ["linear-0000000d", "linear-0000000e"] {
        let out = svc
            .call(
                "issues.search",
                json!({"binding": binding(id, TrackerKind::Linear, "ENG")}),
                &|_| None,
            )
            .await;
        assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
    }
    let config = log(&tmp.path().join("curl"), "stdin.log");
    assert!(
        config.contains(&format!("header = \"Authorization: Bearer {ACCESS}\"")),
        "{config}"
    );
    assert!(
        config.contains(&format!("header = \"Authorization: {KEY}\"")),
        "{config}"
    );
}

#[tokio::test]
async fn a_config_entry_wins_over_an_account_and_the_kind_must_match() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = config_tracker("jira-0000000f", TrackerKind::Jira, &tmp.path().join("t"));
    let svc = integrations(tmp.path(), &[("search/jql", SEARCH, 200)], vec![cfg]);
    // A store account with the same id as a config entry: the entry is used.
    put(&svc, "jira-0000000f", &jira_oauth());
    let out = svc
        .call(
            "issues.search",
            json!({"binding": binding("jira-0000000f", TrackerKind::Jira, "PROJ")}),
            &|_| None,
        )
        .await;
    assert!(matches!(out, Outcome::Ok(_)));
    let config = log(&tmp.path().join("curl"), "stdin.log");
    assert!(
        config.contains("url = \"https://cfg.atlassian.net/rest/api/3/search/jql\""),
        "{config}"
    );
    // A Linear binding on a Jira account is refused.
    put(&svc, "jira-00000010", &jira_token());
    let out = svc
        .call(
            "issues.search",
            json!({"binding": binding("jira-00000010", TrackerKind::Linear, "ENG")}),
            &|_| None,
        )
        .await;
    let Outcome::Err(e) = out else {
        panic!("{out:?}")
    };
    assert_eq!(e.code, "invalid_params");
    // An unknown id.
    let out = svc
        .call(
            "issues.search",
            json!({"binding": binding("jira-ffffffff", TrackerKind::Jira, "PROJ")}),
            &|_| None,
        )
        .await;
    let Outcome::Err(e) = out else {
        panic!("{out:?}")
    };
    assert_eq!(e.code, "unavailable");
}

#[tokio::test]
async fn an_expired_oauth_token_without_a_working_refresh_stops_the_call() {
    let tmp = tempfile::tempdir().unwrap();
    let svc = integrations(
        tmp.path(),
        &[("refresh_token", r#"{"error":"invalid_grant"}"#, 400)],
        Vec::new(),
    );
    ask(
        &svc,
        "accounts.app_set",
        json!({"kind": "jira", "client_id": "c", "client_secret": "s"}),
    )
    .await
    .unwrap();
    let mut a = jira_oauth();
    a.refresh_token = Some(Secret::new("old-refresh".into()));
    a.expires_at = Some(now_secs() - 5);
    put(&svc, "jira-00000011", &a);
    let out = svc
        .call(
            "issues.search",
            json!({"binding": binding("jira-00000011", TrackerKind::Jira, "PROJ")}),
            &|_| None,
        )
        .await;
    let Outcome::Err(e) = out else {
        panic!("{out:?}")
    };
    assert_eq!(e.code, "unavailable");
    assert!(!log(&tmp.path().join("curl"), "stdin.log").contains("search/jql"));
    let list = ask(&svc, "accounts.list", json!({})).await.unwrap();
    let row = list["accounts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["account_id"] == "jira-00000011")
        .unwrap();
    assert_eq!(
        row["status"],
        serde_json::to_value(AccountStatus::NeedsLogin).unwrap()
    );
}

#[tokio::test]
async fn an_oauth_asset_is_fetched_through_the_gateway_with_the_header() {
    let tmp = tempfile::tempdir().unwrap();
    let png = "\u{89}PNG";
    let _ = png;
    let svc = integrations(tmp.path(), &[], Vec::new());
    put(&svc, "jira-00000012", &jira_oauth());
    put(&svc, "jira-00000013", &jira_token());
    let asset = |id: &str, url: &str| {
        let params = json!({"binding": binding(id, TrackerKind::Jira, "PROJ"), "url": url});
        let svc = &svc;
        async move { svc.call("issues.asset", params, &|_| None).await }
    };
    // An OAuth account: only the gateway host.
    let out = asset(
        "jira-00000012",
        "https://acme.atlassian.net/rest/api/3/attachment/content/1",
    )
    .await;
    let Outcome::Err(e) = out else {
        panic!("{out:?}")
    };
    assert_eq!(
        e.code, "invalid_params",
        "the site host is not the origin of an OAuth account"
    );
    assert!(log(&tmp.path().join("curl"), "argv.log").is_empty());
    let _ = asset(
        "jira-00000012",
        "https://api.atlassian.com/ex/jira/cloud-1/rest/api/3/attachment/content/1",
    )
    .await;
    let config = log(&tmp.path().join("curl"), "stdin.log");
    assert!(
        config.contains(
            "url = \"https://api.atlassian.com/ex/jira/cloud-1/rest/api/3/attachment/content/1\""
        ),
        "{config}"
    );
    assert!(
        config.contains(&format!("header = \"Authorization: Bearer {ACCESS}\"")),
        "{config}"
    );
    // A token account keeps today's behavior: the site host.
    let _ = asset(
        "jira-00000013",
        "https://acme.atlassian.net/rest/api/3/attachment/content/2",
    )
    .await;
    let config = log(&tmp.path().join("curl"), "stdin.log");
    assert!(
        config.contains("url = \"https://acme.atlassian.net/rest/api/3/attachment/content/2\""),
        "{config}"
    );
    assert!(config.contains("Authorization: Basic "));
}

#[tokio::test]
async fn containers_list_the_jira_projects_and_the_linear_teams() {
    let tmp = tempfile::tempdir().unwrap();
    let svc = integrations(
        tmp.path(),
        &[
            (
                "project/search?maxResults=100",
                r#"{"values":[{"key":"PROJ","name":"Project"},{"key":"OPS","name":"Operations"},{"name":"no key"}]}"#,
                200,
            ),
            (
                "teams(first: 100)",
                r#"{"data":{"teams":{"nodes":[{"key":"ENG","name":"Engineering"}]}}}"#,
                200,
            ),
        ],
        Vec::new(),
    );
    put(&svc, "jira-00000014", &jira_oauth());
    let mut lin = account(AccountKind::Linear, AccountMethod::Token);
    lin.access_token = Secret::new(KEY.into());
    put(&svc, "linear-00000015", &lin);
    let jira = ask(
        &svc,
        "issues.containers",
        json!({"account_id": "jira-00000014"}),
    )
    .await
    .unwrap();
    assert_eq!(
        jira,
        json!({"containers": [{"key": "PROJ", "name": "Project"}, {"key": "OPS", "name": "Operations"}]})
    );
    let config = log(&tmp.path().join("curl"), "stdin.log");
    assert!(config.contains(
        "https://api.atlassian.com/ex/jira/cloud-1/rest/api/3/project/search?maxResults=100"
    ));
    let linear = ask(
        &svc,
        "issues.containers",
        json!({"account_id": "linear-00000015"}),
    )
    .await
    .unwrap();
    assert_eq!(
        linear,
        json!({"containers": [{"key": "ENG", "name": "Engineering"}]})
    );
    // A forge account and an unknown account have no containers.
    for id in ["cli-github-github.com", "jira-ffffffff", "nope"] {
        let (code, _) = ask(&svc, "issues.containers", json!({"account_id": id}))
            .await
            .unwrap_err();
        assert!(
            code == "invalid_params" || code == "unavailable",
            "{id}: {code}"
        );
    }
}

#[tokio::test]
async fn containers_of_a_config_entry_work_too() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = config_tracker("lin", TrackerKind::Linear, &tmp.path().join("t"));
    let svc = integrations(
        tmp.path(),
        &[(
            "teams(first: 100)",
            r#"{"data":{"teams":{"nodes":[{"key":"ENG","name":"Engineering"}]}}}"#,
            200,
        )],
        vec![cfg],
    );
    let out = ask(&svc, "issues.containers", json!({"account_id": "lin"}))
        .await
        .unwrap();
    assert_eq!(out["containers"][0]["key"], "ENG");
}

#[test]
fn the_service_serves_only_its_methods() {
    for m in super::METHODS {
        assert!(Integrations::serves_accounts(m), "{m}");
        assert!(!Integrations::serves(m), "{m}");
    }
    assert!(Integrations::serves_accounts("issues.containers"));
    assert!(!Integrations::serves_accounts("issues.search"));
    let _: fn(&Accounts) = |_| ();
    let _ = TokenSet {
        kind: AccountKind::Jira,
        site: None,
        email: None,
        token: String::new(),
    };
}
