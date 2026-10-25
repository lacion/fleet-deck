//! The MCP App view, list and document routes over loopback HTTPS (B15,
//! design `mcp-apps.md` §4.7): the method table, the owner-only rows, the
//! feature check, the view token and the document route with its CSP. The
//! station is a fake that answers the link methods itself.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod apps_support;
mod common;
mod share_support;

use std::collections::BTreeSet;
use std::time::Duration;

use apps_support::*;
use common::*;
use fleetdeck_link::LiveLink;
use fleetdeck_proto::link::{Outcome, Scope, WireError};
use serde_json::{Value, json};
use share_support::*;

const DOC: &str = "<!doctype html><html><body><script>var a=1</script></body></html>";
const BOOTSTRAP_START: &str = "<script>(()=>{\"use strict\";";
const CSP_EMPTY: &str = "sandbox allow-scripts; default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; img-src data: blob:; font-src data:; media-src data: blob:; connect-src 'none'; frame-src 'none'; object-src 'none'; base-uri 'none'; form-action 'none'; frame-ancestors 'self'";
const CSP_DOMAINS: &str = "sandbox allow-scripts; default-src 'none'; script-src 'unsafe-inline' https://cdn.example.com; style-src 'unsafe-inline' https://cdn.example.com; img-src data: blob: https://cdn.example.com; font-src data: https://cdn.example.com; media-src data: blob: https://cdn.example.com; connect-src https://api.example.com wss://live.example.com:8443; frame-src 'none'; object-src 'none'; base-uri 'none'; form-action 'none'; frame-ancestors 'self'";

async fn quiet(station: &mut LiveLink) {
    no_request(station, Duration::from_millis(300)).await;
}

fn err(code: &str) -> Outcome {
    Outcome::Err(WireError::new(code, "The station text."))
}

#[tokio::test]
async fn app_routes_are_in_the_method_table() {
    let (f, mut station, c, h) = setup().await;
    let st = f.station.0.clone();
    let cases = [
        ("GET", view_path(&st, "s1", APP), None),
        (
            "POST",
            call_path(&st, "s1", APP),
            Some(tools_call("bump", true)),
        ),
        ("GET", calls_path(&st, "s1"), None),
        ("POST", release_path(&st, "s1", CALL), None),
    ];
    for (method, path, body) in cases {
        let (r, _) = one(
            &c,
            &h,
            &mut station,
            method,
            &path,
            body,
            err("unknown_app"),
        )
        .await;
        assert_ne!(r.code(), "not_found", "{method} {path}: {}", r.body);
        assert_eq!(r.status, 404, "{method} {path}: {}", r.body);
        assert_eq!(r.code(), "unknown_app", "{method} {path}");
    }
}

#[tokio::test]
async fn view_is_owner_only() {
    let (f, mut station, c, h) = setup().await;
    let st = f.station.0.clone();
    let (_, token) = f
        .store
        .create_token(
            "reader".into(),
            BTreeSet::from([Scope::Read, Scope::Drive, Scope::Admin]),
            86_400_000,
        )
        .await
        .unwrap();
    let auth = format!("Bearer {}", token.expose());
    let share = json!({ "kind": "session", "target": format!("{st}/s1"), "role": "viewer" });
    let (member, _, _) = join(&c, &h, share, "Alice").await;
    let routes: [(&str, String, Option<Value>); 4] = [
        ("GET", view_path(&st, "s1", APP), None),
        (
            "POST",
            call_path(&st, "s1", APP),
            Some(tools_call("bump", true)),
        ),
        ("GET", calls_path(&st, "s1"), None),
        ("POST", release_path(&st, "s1", CALL), None),
    ];
    for (method, path, body) in routes {
        let r = c
            .req(
                method,
                &path,
                &[("Authorization", auth.as_str())],
                body.clone(),
            )
            .await;
        expect(&r, 403, "owner_only");
        let r = c.req(method, &path, &refs(&member), body).await;
        expect(&r, 403, "owner_only");
    }
    quiet(&mut station).await;
}

#[tokio::test]
async fn view_without_feature_is_409() {
    let (f, mut station, c, h) = setup().await;
    let st = f.station.0.clone();
    f.store.set_link_features(&f.station, 98, vec![]);
    let r = get(&c, &h, &view_path(&st, "s1", APP)).await;
    expect(&r, 409, "apps_unsupported");
    let r = get(&c, &h, &calls_path(&st, "s1")).await;
    expect(&r, 409, "apps_unsupported");
    let r = post(&c, &h, &call_path(&st, "s1", APP), tools_call("bump", true)).await;
    expect(&r, 409, "apps_unsupported");
    let r = post(&c, &h, &release_path(&st, "s1", CALL), json!({})).await;
    expect(&r, 409, "apps_unsupported");
    quiet(&mut station).await;
}

#[tokio::test]
async fn view_mints_token_and_document_route_serves_with_app_csp() {
    let (f, mut station, c, h) = setup().await;
    let st = f.station.0.clone();
    let doc = DOC.as_bytes();

    // Non-empty domains; the deck sanitizes them again.
    let csp = json!({
        "connect_domains": ["https://api.example.com", "javascript:alert(1)",
                            "wss://live.example.com:8443"],
        "resource_domains": ["https://cdn.example.com", "https://x.com/path"],
    });
    let (r, req) = one(
        &c,
        &h,
        &mut station,
        "GET",
        &view_path(&st, "s1", APP),
        None,
        Outcome::Ok(meta_json(doc, &csp)),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(req.method, "session.app_meta");
    assert_eq!(req.params, json!({ "session_id": "s1", "app_id": APP }));
    let url = r.body["url"].as_str().unwrap().to_owned();
    assert!(url.starts_with("/app/"), "{url}");
    assert_eq!(url.len(), "/app/".len() + 43);
    assert_eq!(r.body["server"], "demo");
    assert_eq!(r.body["tool"], "show_counter");
    assert_eq!(r.body["resource_uri"], "ui://demo/counter");
    assert_eq!(r.body["prefers_border"], true);
    assert_eq!(r.body["tool_input"], json!({ "start": 1 }));
    assert_eq!(r.body["tool_result"], json!({ "content": [] }));
    assert_eq!(
        r.body["csp"],
        json!({
            "connect_domains": ["https://api.example.com", "wss://live.example.com:8443"],
            "resource_domains": ["https://cdn.example.com"],
        })
    );
    assert!(r.body["expires_at"].as_i64().unwrap() > 0);

    // The document route needs no login; it asks for the chunks.
    let (doc_r, reqs) = exchange(c.get(&url, &[]), &mut station, document_answers(doc, 20)).await;
    assert_eq!(doc_r.status, 200, "{}", doc_r.text);
    assert!(reqs.len() > 1, "the document came in several chunks");
    for (i, req) in reqs.iter().enumerate() {
        assert_eq!(req.method, "session.app_document");
        assert_eq!(
            req.params,
            json!({ "session_id": "s1", "app_id": APP, "offset": i * 20 })
        );
    }
    assert_eq!(
        doc_r.header("content-type"),
        Some("text/html; charset=utf-8")
    );
    assert_eq!(doc_r.header("cache-control"), Some("no-store"));
    assert_eq!(doc_r.header("x-content-type-options"), Some("nosniff"));
    assert_eq!(doc_r.header("referrer-policy"), Some("no-referrer"));
    assert_eq!(doc_r.header("content-security-policy"), Some(CSP_DOMAINS));
    // The doctype comes first, then the bootstrap as the first script.
    let at = "<!doctype html>".len();
    assert!(doc_r.text.starts_with("<!doctype html>"), "{}", doc_r.text);
    assert_eq!(doc_r.text.find("<script>"), Some(at));
    assert!(doc_r.text[at..].starts_with(BOOTSTRAP_START));
    assert!(
        doc_r
            .text
            .ends_with("<script>var a=1</script></body></html>")
    );

    // A reload of the frame loads the document again with the same token.
    let (again, _) = exchange(c.get(&url, &[]), &mut station, document_answers(doc, 4096)).await;
    assert_eq!(again.status, 200);

    // Empty domains: nothing after the sources, `connect-src 'none'`.
    let r = mint(&c, &h, &mut station, &st, doc, &json!({})).await;
    let url = r.body["url"].as_str().unwrap().to_owned();
    let (empty, _) = exchange(c.get(&url, &[]), &mut station, document_answers(doc, 4096)).await;
    assert_eq!(empty.header("content-security-policy"), Some(CSP_EMPTY));
    for directive in CSP_EMPTY.split("; ").chain(CSP_DOMAINS.split("; ")) {
        if !directive.starts_with("frame-ancestors") {
            assert!(!directive.contains("'self'"), "{directive}");
        }
    }
}

#[tokio::test]
async fn document_route_unknown_or_expired_token_404() {
    let (f, mut station, c, h) = setup().await;
    let st = f.station.0.clone();
    for path in [
        format!("/app/{}", "A".repeat(43)),
        "/app/short".to_owned(),
        format!("/app/{}", "!".repeat(43)),
    ] {
        let r = c.get(&path, &[]).await;
        assert_eq!(r.status, 404, "{path}");
    }
    let r = mint(&c, &h, &mut station, &st, DOC.as_bytes(), &json!({})).await;
    let url = r.body["url"].as_str().unwrap().to_owned();
    f.det.advance(Duration::from_secs(601));
    let r = c.get(&url, &[]).await;
    assert_eq!(r.status, 404, "{}", r.text);
    quiet(&mut station).await;
}

#[tokio::test]
async fn document_sha_mismatch_is_502() {
    let (f, mut station, c, h) = setup().await;
    let st = f.station.0.clone();
    let doc = DOC.as_bytes();
    let (r, _) = one(
        &c,
        &h,
        &mut station,
        "GET",
        &view_path(&st, "s1", APP),
        None,
        Outcome::Ok(meta_with_sha(doc, &sha256_hex(b"another document"))),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    let url = r.body["url"].as_str().unwrap().to_owned();
    let (bad, _) = exchange(c.get(&url, &[]), &mut station, document_answers(doc, 4096)).await;
    expect(&bad, 502, "station_error");
    assert!(!bad.text.contains("<script>"), "{}", bad.text);
    // A document over the largest size is refused too.
    let r = mint(&c, &h, &mut station, &st, doc, &json!({})).await;
    let url = r.body["url"].as_str().unwrap().to_owned();
    let big = Outcome::Ok(json!({ "data_b64": "", "eof": false, "total": 3 * 1024 * 1024 }));
    let (bad, _) = exchange(c.get(&url, &[]), &mut station, vec![big]).await;
    expect(&bad, 502, "station_error");
}

#[tokio::test]
async fn list_route_answers_the_station_list() {
    let (f, mut station, c, h) = setup().await;
    let st = f.station.0.clone();
    // One call of a session in the same folder, whose app row is gone.
    let calls = json!({ "calls": [
        open_call(CALL, "s2", "uncertain", Some("station_restart")),
        open_call("01HZX3K5M7Q9R2S4T6V8W0Y1Z5", "s1", "running", None),
    ] });
    let (r, req) = one(
        &c,
        &h,
        &mut station,
        "GET",
        &calls_path(&st, "s1"),
        None,
        Outcome::Ok(calls.clone()),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body, calls);
    assert_eq!(req.method, "session.app_calls");
    assert_eq!(req.params, json!({ "session_id": "s1" }));
    let (r, _) = one(
        &c,
        &h,
        &mut station,
        "GET",
        &calls_path(&st, "s1"),
        None,
        Outcome::Ok(json!({ "calls": [] })),
    )
    .await;
    assert_eq!(r.body, json!({ "calls": [] }));
    let r = get(&c, &h, &calls_path(&st, "nope")).await;
    expect(&r, 404, "unknown_session");
    quiet(&mut station).await;
}
