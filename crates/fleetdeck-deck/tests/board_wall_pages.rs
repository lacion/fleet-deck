//! The wall pages routes over loopback HTTPS (B17 §3.2.4): the owner's list,
//! add (with a fresh elevation) and delete, who is refused, and the audit
//! rows, which hold the host and never the path or the query.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
mod displays_support;
mod share_support;

use common::board::{BoardClient, Http};
use displays_support::*;
use fleetdeck_deck::store::AuditResult;
use fleetdeck_proto::link::Scope;
use serde_json::{Value, json};
use share_support::*;

fn page(label: &str, url: &str) -> Value {
    json!({ "label": label, "url": url, "fit": "screen" })
}

async fn add(c: &BoardClient, h: &Headers, label: &str, url: &str, e: &str) -> Http {
    let mut body = page(label, url);
    body["elevation"] = json!(e);
    post(c, h, "/api/wall-pages", body).await
}

#[tokio::test]
async fn add_needs_elevation() {
    let (_f, c, h, _st) = setup_deck().await;
    // A bad entry answers with its text first, before the elevation.
    let bad = post(&c, &h, "/api/wall-pages", page("", "https://a.example")).await;
    expect(&bad, 400, "invalid_page");
    assert_eq!(bad.body["message"], "Give a name of 1 to 60 characters.");
    // A good entry without an elevation, and with a wrong one.
    for body in [
        page("Grafana", "https://grafana.example/d"),
        json!({ "label": "Grafana", "url": "https://grafana.example/d", "fit": "screen",
                "elevation": "not-a-token" }),
    ] {
        let r = post(&c, &h, "/api/wall-pages", body).await;
        expect(&r, 400, "elevation_required");
        assert_eq!(r.body["keys"], json!(["wall_pages"]));
    }
    let listed = get(&c, &h, "/api/wall-pages").await;
    assert_eq!(listed.status, 200, "{}", listed.body);
    assert_eq!(listed.body, json!({ "pages": [] }));
    // With a fresh elevation the entry is stored.
    let e = elevate(&c, &h).await;
    let ok = add(
        &c,
        &h,
        "  Grafana ",
        "https://Grafana.example:443/d?x=1",
        &e,
    )
    .await;
    assert_eq!(ok.status, 200, "{}", ok.body);
    assert_eq!(ok.body["label"], "Grafana");
    assert_eq!(ok.body["origin"], "https://grafana.example");
    assert_eq!(ok.body["url"], "https://Grafana.example:443/d?x=1");
    assert_eq!(ok.body["fit"], "screen");
    assert_eq!(ok.body["page_id"].as_str().unwrap().len(), 26);
    let listed = get(&c, &h, "/api/wall-pages").await;
    assert_eq!(listed.body["pages"], json!([ok.body.clone()]));
    // The other refusals of the table keep their texts.
    let http = add(&c, &h, "Nas", "http://nas.local", &e).await;
    expect(&http, 400, "invalid_page");
    assert!(
        http.body["message"]
            .as_str()
            .unwrap()
            .starts_with("Use https for this address.")
    );
    let board = add(&c, &h, "Board", &format!("{}/x", c.origin()), &e).await;
    expect(&board, 400, "invalid_page");
    assert_eq!(
        board.body["message"],
        "This is the address of the board. A wall cannot show the board in a frame."
    );
    // A field that does not belong to the body is a parse error.
    let extra = post(
        &c,
        &h,
        "/api/wall-pages",
        json!({ "label": "a", "url": "https://a.example", "fit": "screen", "origin": "x" }),
    )
    .await;
    expect(&extra, 400, "invalid_body");
}

#[tokio::test]
async fn the_fifty_first_page_is_a_409() {
    let (_f, c, h, _st) = setup_deck().await;
    let e = elevate(&c, &h).await;
    for n in 0..50 {
        let r = add(
            &c,
            &h,
            &format!("Page {n}"),
            &format!("https://s{n}.example"),
            &e,
        )
        .await;
        assert_eq!(r.status, 200, "{n}: {}", r.body);
    }
    let r = add(&c, &h, "One more", "https://more.example", &e).await;
    expect(&r, 409, "too_many_pages");
    assert_eq!(r.body["message"], "A deck holds at most 50 wall pages.");
    let listed = get(&c, &h, "/api/wall-pages").await;
    assert_eq!(listed.body["pages"].as_array().unwrap().len(), 50);
}

#[tokio::test]
async fn member_and_display_are_refused() {
    let (_f, c, h, st) = setup_deck().await;
    let e = elevate(&c, &h).await;
    let made = add(&c, &h, "Grafana", "https://grafana.example", &e).await;
    let id = made.body["page_id"].as_str().unwrap().to_owned();
    let share = json!({ "kind": "session", "target": format!("{st}/s1"), "role": "viewer" });
    let (member, _, _) = join(&c, &h, share, "Alice").await;
    let (_, cookie) = display_login(&c, "Wall", &[&st]).await;
    let (_, read_token) = c
        .store
        .create_token(
            "ops".into(),
            [Scope::Read].into_iter().collect(),
            86_400_000,
        )
        .await
        .unwrap();
    let bearer = format!("Bearer {}", read_token.expose());
    let origin = c.origin();
    let delete = format!("/api/wall-pages/{id}/delete");
    let new_page = page("Other", "https://other.example");
    for (method, path, body) in [
        ("GET", "/api/wall-pages".to_owned(), None),
        ("POST", "/api/wall-pages".to_owned(), Some(new_page)),
        ("POST", delete, Some(json!({}))),
    ] {
        let member_headers = refs(&member);
        let by_member = c.req(method, &path, &member_headers, body.clone()).await;
        expect(&by_member, 403, "owner_only");
        let by_display = c
            .req(
                method,
                &path,
                &[("Cookie", &cookie), ("Origin", &origin)],
                body.clone(),
            )
            .await;
        expect(&by_display, 403, "display_read_only");
        let by_token = c
            .req(method, &path, &[("Authorization", &bearer)], body.clone())
            .await;
        assert_eq!(by_token.status, 403, "{path}: {}", by_token.body);
        let anon = c.req(method, &path, &[], body).await;
        assert_eq!(anon.status, 401, "{path}: {}", anon.body);
    }
    // Nothing changed.
    let listed = get(&c, &h, "/api/wall-pages").await;
    assert_eq!(listed.body["pages"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn delete_of_an_unknown_page_is_404() {
    let (_f, c, h, _st) = setup_deck().await;
    let r = post(&c, &h, "/api/wall-pages/NOSUCHPAGE/delete", json!({})).await;
    expect(&r, 404, "unknown_page");
    assert_eq!(r.body["message"], "No such page.");
    // A delete needs no elevation, and answers the number of displays reset.
    let e = elevate(&c, &h).await;
    let made = add(&c, &h, "Grafana", "https://grafana.example", &e).await;
    let id = made.body["page_id"].as_str().unwrap();
    let r = post(&c, &h, &format!("/api/wall-pages/{id}/delete"), json!({})).await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body, json!({ "displays_reset": 0 }));
    assert_eq!(
        get(&c, &h, "/api/wall-pages").await.body,
        json!({ "pages": [] })
    );
    let again = post(&c, &h, &format!("/api/wall-pages/{id}/delete"), json!({})).await;
    expect(&again, 404, "unknown_page");
}

#[tokio::test]
async fn audit_holds_the_host_and_never_the_path() {
    let (_f, c, h, _st) = setup_deck().await;
    let e = elevate(&c, &h).await;
    let url = "https://box.example:8443/SECRETPATH/page?token=SENTINELQUERY#FRAGMENT";
    let made = add(&c, &h, "Box", url, &e).await;
    assert_eq!(made.status, 200, "{}", made.body);
    let id = made.body["page_id"].as_str().unwrap().to_owned();
    let added = c.audited("wall_page.add", AuditResult::Ok).await;
    assert_eq!(added, [Some(format!("page={id} host=box.example"))]);
    // A refused entry is audited with the code only.
    let bad = add(
        &c,
        &h,
        "Nas",
        "http://nas.local/SECRETPATH?token=SENTINELQUERY",
        &e,
    )
    .await;
    expect(&bad, 400, "invalid_page");
    let failed = c.audited("wall_page.add", AuditResult::Failed).await;
    assert_eq!(failed, [Some("invalid_page".to_owned())]);
    // The delete row.
    let r = post(&c, &h, &format!("/api/wall-pages/{id}/delete"), json!({})).await;
    assert_eq!(r.status, 200, "{}", r.body);
    let deleted = c.audited("wall_page.delete", AuditResult::Ok).await;
    assert_eq!(deleted, [Some(format!("page={id} displays_reset=0"))]);
    // No audit row holds any part of the path, the query or the fragment.
    let all = format!("{:?}", c.store.audit_log(200).await.unwrap());
    for secret in ["SECRETPATH", "SENTINELQUERY", "FRAGMENT"] {
        assert!(!all.contains(secret), "{secret} is in the audit: {all}");
    }
}
