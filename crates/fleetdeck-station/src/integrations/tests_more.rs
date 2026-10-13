//! Linear, asset and status tests of the integration service.

use base64::Engine;

use super::*;

// ----- Linear -----

#[tokio::test]
async fn linear_values_travel_as_variables_and_a_transition_uses_the_state_id() {
    let tmp = tempfile::tempdir().unwrap();
    let svc = service(
        tmp.path(),
        &[],
        &[],
        &[
            (
                "issueUpdate",
                r#"{"data":{"issueUpdate":{"success":true,"issue":{"url":"https://linear.app/a/issue/ENG-42"}}}}"#,
            ),
            (
                "team { states",
                r#"{"data":{"issue":{"id":"uuid-1","url":"https://linear.app/a/issue/ENG-42","team":{"states":{"nodes":[{"id":"s1","name":"Todo"},{"id":"s2","name":"In Review"}]}}}}}"#,
            ),
            ("viewer { name", r#"{"data":{"viewer":{"name":"Ana L"}}}"#),
            (
                "issues(filter",
                r#"{"data":{"issues":{"nodes":[{"identifier":"ENG-42","title":"Slow","url":"https://linear.app/a/issue/ENG-42","state":{"name":"Todo"},"updatedAt":"2024-05-01T10:00:00.000Z","priorityLabel":"High","labels":{"nodes":[]}}]}}}"#,
            ),
        ],
    );
    let found: IssueList = serde_json::from_value(
        ask(&svc, "issues.search", json!({"binding": serde_json::to_value(linear_binding()).unwrap(), "query": "slow \"} hack", "status": "Todo"}))
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(found.issues[0].r#ref.key, "ENG-42");
    let cfg = log(&tmp.path().join("curl"), "stdin.log");
    assert!(
        cfg.contains(&format!("header = \"Authorization: {TOKEN}\"")),
        "{cfg}"
    );
    assert!(cfg.contains("url = \"https://api.linear.app/graphql\""));
    let done: WriteResult = serde_json::from_value(
        ask(&svc, "issues.transition", json!({"command_id": "l-1", "binding": serde_json::to_value(linear_binding()).unwrap(), "key": "ENG-42", "to": "in review"}))
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        (done.url.as_str(), done.actor.as_deref()),
        ("https://linear.app/a/issue/ENG-42", Some("Ana L"))
    );
    assert!(
        sent_bodies(&tmp.path().join("curl"))
            .iter()
            .any(|b| b["variables"]["state"] == "s2"),
        "the mutation gets the state id as a variable"
    );
    let search = &sent_bodies(&tmp.path().join("curl"))[0];
    assert_eq!(
        search["variables"]["filter"]["or"][0]["title"]["containsIgnoreCase"],
        "slow \"} hack"
    );
    assert!(
        !search["query"].as_str().unwrap().contains("hack"),
        "the query text holds no user value"
    );
}

// ----- Assets -----

#[tokio::test]
async fn an_asset_must_be_an_image_from_a_host_of_the_binding() {
    let tmp = tempfile::tempdir().unwrap();
    let svc = service(tmp.path(), &[], &[], &[("a.png", "<svg/>\ntext/html")]);
    let query =
        |url: &str| json!({"binding": serde_json::to_value(jira_binding()).unwrap(), "url": url});
    // Allowed host, but the bytes are not an image.
    let (c, _) = ask(
        &svc,
        "issues.asset",
        query("https://acme.atlassian.net/a.png"),
    )
    .await
    .unwrap_err();
    assert_eq!(c, code::BAD_ANSWER);
    for url in [
        "https://evil.test/a.png",
        "http://acme.atlassian.net/a.png",
        "https://user:pw@acme.atlassian.net/a.png",
    ] {
        let (c, m) = ask(&svc, "issues.asset", query(url)).await.unwrap_err();
        assert_eq!(c, code::INVALID_PARAMS, "{url}: {m}");
    }
    let curl = tmp.path().join("curl");
    assert_eq!(calls(&curl), 1, "only the first URL reached curl");
    assert!(log(&curl, "stdin.log").contains("header = \"Authorization: Basic "));
    // A forge image is fetched without any credential.
    let forge = json!({"binding": {"type": "forge", "kind": "github", "host": "github.com", "repo": "a/b"},
        "url": "https://user-images.githubusercontent.com/1/a.png"});
    let (c, _) = ask(&svc, "issues.asset", forge).await.unwrap_err();
    assert_eq!(c, code::BAD_ANSWER);
    let cfg = log(&curl, "stdin.log");
    let last = cfg.rsplit("proto = ").next().unwrap();
    assert!(!last.contains("header ="), "{last}");
}

#[tokio::test]
async fn a_real_png_is_returned_as_base64() {
    let tmp = tempfile::tempdir().unwrap();
    let svc = service(tmp.path(), &[], &[], &[]);
    // Replace curl by a script that prints real PNG bytes.
    let curl = tmp.path().join("curl/curl");
    std::fs::write(
        &curl,
        "#!/bin/sh\ncat > /dev/null\nprintf '\\211PNG\\r\\n\\032\\nDATA\\nimage/png'\n",
    )
    .unwrap();
    let got: Asset = serde_json::from_value(
        ask(&svc, "issues.asset", json!({"binding": serde_json::to_value(jira_binding()).unwrap(), "url": "https://acme.atlassian.net/rest/api/3/attachment/content/10"}))
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(got.content_type, "image/png");
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&got.data_base64)
        .unwrap();
    assert!(bytes.starts_with(b"\x89PNG") && bytes.ends_with(b"DATA"));
}

#[tokio::test]
async fn the_status_reports_trackers_and_clis() {
    let tmp = tempfile::tempdir().unwrap();
    let svc = service(
        tmp.path(),
        &[VIEWER_GH],
        &[],
        &[
            ("myself", r#"{"displayName":"Ana Acme"}"#),
            ("viewer { name", r#"{"data":{"viewer":{"name":"Ana L"}}}"#),
        ],
    );
    let status: IntegrationsStatus =
        serde_json::from_value(ask(&svc, "integrations.status", json!({})).await.unwrap()).unwrap();
    assert_eq!(status.trackers.len(), 2);
    assert!(status.trackers.iter().all(|t| t.ok), "{status:?}");
    assert_eq!(status.trackers[0].user.as_deref(), Some("Ana Acme"));
    assert_eq!(
        status.trackers[0].site.as_deref(),
        Some("https://acme.atlassian.net")
    );
    assert!(status.gh.present && status.gh.authenticated);
    assert_eq!(status.gh.user.as_deref(), Some("luis"));
    assert!(
        status.glab.present && !status.glab.authenticated,
        "the fake glab has no answer"
    );
    assert!(!status.glab.message.is_empty());
}
