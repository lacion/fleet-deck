//! The tailnet routes (DL-178): owner only, `GET /api/tailnet`, the devices
//! list with the pairing state, and "Use tailnet" as an elevated settings
//! save. A fake `tailscale` script gives the status documents.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
mod settings_support;

use std::collections::BTreeSet;
use std::net::IpAddr;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::Arc;

use common::board::{BoardClient, Extras, board_with_extras};
use common::*;
use fleetdeck_core::Secret;
use fleetdeck_core::identity::Identity;
use fleetdeck_deck::http::tailnet_routes::use_with;
use fleetdeck_deck::store::AuditResult;
use fleetdeck_deck::store::stations::NewStation;
use fleetdeck_proto::link::Scope;
use serde_json::{Value, json};
use settings_support::*;

/// A status with two tailnet addresses, one peer that is paired (dialed at
/// its address), one that is paired by its DNS name, and two that are not.
const STATUS: &str = r#"{
  "BackendState": "Running",
  "Self": {"HostName": "deck-box", "DNSName": "deck-box.tail1234.ts.net.", "OS": "linux",
           "UserID": 1, "TailscaleIPs": ["100.64.0.10", "fd7a:115c:a1e0::a"]},
  "Peer": {
    "nodekey:a": {"HostName": "alpha", "DNSName": "alpha.tail1234.ts.net.", "OS": "linux",
                  "UserID": 1, "TailscaleIPs": ["100.64.0.11"], "Online": true},
    "nodekey:b": {"HostName": "bravo", "DNSName": "bravo.tail1234.ts.net.", "OS": "macOS",
                  "UserID": 1, "TailscaleIPs": ["100.64.0.12", "fd7a:115c:a1e0::c"], "Online": false},
    "nodekey:c": {"HostName": "charlie", "DNSName": "charlie.tail1234.ts.net.",
                  "UserID": 2, "TailscaleIPs": ["100.64.0.13"], "Online": true,
                  "Tags": ["tag:ci"]},
    "nodekey:d": {"HostName": "delta", "DNSName": "delta.tail1234.ts.net.",
                  "UserID": 1, "TailscaleIPs": ["fd7a:115c:a1e0::d"], "Online": true}
  },
  "User": {"1": {"LoginName": "owner@example.com"}, "2": {"LoginName": "tagged-devices"}}
}"#;

const DECK_TOML: &str = "name = \"alpha\"\nlisten = [\"127.0.0.1:7420\", \"192.168.5.5:7420\"]\nlink_listen = [\"127.0.0.1:7421\"]\n";

/// Writes the fake program in `dir` and returns its path. It prints `body`.
fn fake_tailscale(dir: &Path, name: &str, body: &str) -> String {
    let doc = dir.join(format!("{name}.json"));
    std::fs::write(&doc, body).unwrap();
    let path = dir.join(name);
    std::fs::write(&path, format!("#!/bin/sh\ncat '{}'\n", doc.display())).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path.display().to_string()
}

/// A board whose `deck.toml` is `DECK_TOML` plus `extra`.
async fn deck_board(dir: &Path, extra: &str) -> (Fixture, BoardClient) {
    let f = fixture().await;
    let c = board_with_extras(
        Arc::clone(&f.ctx),
        Extras {
            settings: Some(deck_settings(dir, &format!("{DECK_TOML}{extra}"))),
            ..Extras::default()
        },
    )
    .await;
    (f, c)
}

fn toml_bin(path: &str) -> String {
    format!("tailscale_bin = \"{path}\"\n")
}

#[tokio::test]
async fn the_tailnet_routes_are_owner_only() {
    let tmp = tempfile::tempdir().unwrap();
    let (_f, c) = deck_board(tmp.path(), "").await;
    let owner_h = owner(&c).await;
    let routes = [
        ("GET", "/api/tailnet"),
        ("POST", "/api/tailnet/use"),
        ("GET", "/api/tailnet/devices"),
    ];
    // A token with every scope, `admin` included, is refused.
    let (_, t) = c
        .store
        .create_token(
            "ops".into(),
            BTreeSet::from([Scope::Read, Scope::Drive, Scope::Approve, Scope::Admin]),
            86_400_000,
        )
        .await
        .unwrap();
    let auth = format!("Bearer {}", t.expose());
    for (method, path) in routes {
        let body = (method == "POST").then(|| json!({}));
        let r = c
            .req(
                method,
                path,
                &[("Authorization", auth.as_str())],
                body.clone(),
            )
            .await;
        expect(&r, 403, "owner_only");
        // Nobody logged in: 401.
        let r = c.req(method, path, &[], body).await;
        assert_eq!(r.status, 401, "{method} {path}");
    }
    // A member of a shared session is refused too.
    let share = post(
        &c,
        &owner_h,
        "/api/shares",
        json!({ "kind": "board", "role": "approver" }),
    )
    .await;
    assert_eq!(share.status, 200, "{}", share.body);
    let invite = share.body["url"].as_str().unwrap();
    let token = invite.split("#join=").nth(1).unwrap();
    let joined = c
        .post(
            "/api/auth/join",
            &[],
            json!({ "token": token, "display_name": "Ann" }),
        )
        .await;
    assert_eq!(joined.status, 200, "{}", joined.body);
    let cookie = joined
        .header("set-cookie")
        .unwrap()
        .split(';')
        .next()
        .unwrap();
    let csrf = joined.body["csrf"].as_str().unwrap();
    for (method, path) in routes {
        let body = (method == "POST").then(|| json!({}));
        let r = c
            .req(
                method,
                path,
                &[("Cookie", cookie), ("X-CSRF-Token", csrf)],
                body,
            )
            .await;
        expect(&r, 403, "owner_only");
    }
    // The refusals are audited under the names of the table.
    for action in ["tailnet.get", "tailnet.devices", "tailnet.use"] {
        assert!(
            !c.audited(action, AuditResult::Refused).await.is_empty(),
            "{action}"
        );
    }
    // The owner gets through.
    let r = get(&c, &owner_h, "/api/tailnet").await;
    assert_eq!(r.status, 200, "{}", r.body);
}

#[tokio::test]
async fn use_tailnet_needs_the_csrf_token_of_the_login() {
    let tmp = tempfile::tempdir().unwrap();
    let (_f, c) = deck_board(tmp.path(), "").await;
    let h = owner(&c).await;
    let cookie_only = vec![h[0].clone()];
    let r = c
        .post("/api/tailnet/use", &refs(&cookie_only), json!({}))
        .await;
    assert_eq!(r.status, 403, "{}", r.body);
    assert_eq!(r.code(), "csrf");
    let wrong = vec![h[0].clone(), ("X-CSRF-Token".to_owned(), "nope".to_owned())];
    let r = c.post("/api/tailnet/use", &refs(&wrong), json!({})).await;
    assert_eq!((r.status, r.code()), (403, "csrf"));
    // A body with an unknown field, or no body, is a bad request.
    expect(
        &post(&c, &h, "/api/tailnet/use", json!({ "nope": 1 })).await,
        400,
        "invalid_body",
    );
    let r = c.req("POST", "/api/tailnet/use", &refs(&h), None).await;
    expect(&r, 400, "invalid_body");
    // The settings file was not touched.
    let text = std::fs::read_to_string(tmp.path().join("deck.toml")).unwrap();
    assert_eq!(text, DECK_TOML);
}

#[tokio::test]
async fn the_status_route_shows_this_machine_and_the_userspace_flag() {
    let tmp = tempfile::tempdir().unwrap();
    let bin = fake_tailscale(tmp.path(), "tailscale", STATUS);
    let (_f, c) = deck_board(tmp.path(), &toml_bin(&bin)).await;
    let h = owner(&c).await;
    let r = get(&c, &h, "/api/tailnet").await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body["available"], true);
    assert_eq!(r.body["self"]["dns_name"], "deck-box.tail1234.ts.net");
    assert_eq!(
        r.body["self"]["ips"],
        json!(["100.64.0.10", "fd7a:115c:a1e0::a"])
    );
    assert_eq!(r.body["self"]["login"], "owner@example.com");
    assert_eq!(r.body["self"]["os"], "linux");
    // The tailnet addresses are not assigned to an interface of this test
    // machine: Tailscale looks like userspace networking, and nothing is
    // offered.
    assert_eq!(r.body["userspace"], true);
    let current = json!({
        "listen": ["127.0.0.1:7420", "192.168.5.5:7420"],
        "link_listen": ["127.0.0.1:7421"]
    });
    assert_eq!(r.body["current"], current);
    assert_eq!(r.body["managed"]["listen"], current["listen"]);
    assert_eq!(r.body["managed"]["link_listen"], current["link_listen"]);
    assert!(r.body["reason"].is_null());
}

#[tokio::test]
async fn a_tailscale_that_is_missing_or_stopped_gives_a_reason() {
    let tmp = tempfile::tempdir().unwrap();
    let missing = tmp.path().join("no-such-tailscale");
    let (_f, c) = deck_board(tmp.path(), &toml_bin(&missing.display().to_string())).await;
    let h = owner(&c).await;
    let r = get(&c, &h, "/api/tailnet").await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body["available"], false);
    assert_eq!(r.body["reason"], "not installed");
    assert_eq!(r.body["userspace"], false);
    assert!(r.body.get("self").is_none());
    let r = get(&c, &h, "/api/tailnet/devices").await;
    assert_eq!(r.body["available"], false);
    assert_eq!(r.body["reason"], "not installed");
    assert_eq!(r.body["devices"], json!([]));
    // "Use tailnet" says why it cannot work, and writes nothing.
    let r = post(&c, &h, "/api/tailnet/use", json!({})).await;
    expect(&r, 409, "tailnet_unavailable");
    assert!(
        r.body["message"]
            .as_str()
            .unwrap()
            .contains("not installed")
    );
    let text = std::fs::read_to_string(tmp.path().join("deck.toml")).unwrap();
    assert!(!text.contains("100.64"), "{text}");

    // A stopped backend is "not running".
    let dir = tmp.path().join("stopped");
    std::fs::create_dir(&dir).unwrap();
    let stopped = fake_tailscale(&dir, "tailscale", r#"{"BackendState":"Stopped"}"#);
    let (_f2, c2) = deck_board(&dir, &toml_bin(&stopped)).await;
    let h2 = owner(&c2).await;
    let r = get(&c2, &h2, "/api/tailnet").await;
    assert_eq!(
        (r.body["available"].clone(), r.body["reason"].clone()),
        (json!(false), json!("not running"))
    );
}

#[tokio::test]
async fn use_tailnet_in_userspace_networking_is_a_409_and_writes_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let bin = fake_tailscale(tmp.path(), "tailscale", STATUS);
    let (_f, c) = deck_board(tmp.path(), &toml_bin(&bin)).await;
    let h = owner(&c).await;
    let before = std::fs::read_to_string(tmp.path().join("deck.toml")).unwrap();
    let r = post(&c, &h, "/api/tailnet/use", json!({})).await;
    expect(&r, 409, "tailnet_userspace");
    assert!(
        r.body["message"]
            .as_str()
            .unwrap()
            .contains("userspace networking")
    );
    let after = std::fs::read_to_string(tmp.path().join("deck.toml")).unwrap();
    assert_eq!(before, after);
}

#[tokio::test]
async fn without_a_settings_file_the_reads_work_and_use_is_a_503() {
    let f = fixture().await;
    let c = common::board::board(Arc::clone(&f.ctx)).await;
    let h = owner(&c).await;
    let r = get(&c, &h, "/api/tailnet").await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(
        r.body["current"],
        json!({ "listen": [], "link_listen": [] })
    );
    let r = get(&c, &h, "/api/tailnet/devices").await;
    assert_eq!(r.status, 200, "{}", r.body);
    expect(
        &post(&c, &h, "/api/tailnet/use", json!({})).await,
        503,
        "settings_unavailable",
    );
}

#[tokio::test]
async fn the_devices_list_has_the_pairing_state_from_the_deck_store() {
    let tmp = tempfile::tempdir().unwrap();
    let bin = fake_tailscale(tmp.path(), "tailscale", STATUS);
    let (f, c) = deck_board(tmp.path(), &toml_bin(&bin)).await;
    for (name, dial_url) in [
        ("by-ip", Some("100.64.0.11:7421")),
        ("by-name", Some("Bravo.tail1234.ts.net:7421")),
        ("by-v6", None),
        ("lan", Some("192.168.1.9:7421")),
    ] {
        f.store
            .pin_station(NewStation {
                key: Identity::generate().unwrap().public_key(),
                name: name.to_owned(),
                scopes: BTreeSet::from([Scope::Read]),
                dial_url: dial_url.map(str::to_owned),
            })
            .await
            .unwrap();
    }
    let h = owner(&c).await;
    let r = get(&c, &h, "/api/tailnet/devices").await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body["available"], true);
    let devices = r.body["devices"].as_array().unwrap();
    let names: Vec<&str> = devices
        .iter()
        .map(|d| d["host_name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["alpha", "bravo", "charlie", "delta"]);
    let state = |i: usize| devices[i]["state"].as_str().unwrap();
    assert_eq!(
        [state(0), state(1), state(2), state(3)],
        ["paired", "paired", "none", "none"]
    );
    let alpha = &devices[0];
    assert_eq!(alpha["dns_name"], "alpha.tail1234.ts.net");
    assert_eq!(alpha["ips"], json!(["100.64.0.11"]));
    assert_eq!(alpha["os"], "linux");
    assert_eq!(alpha["login"], "owner@example.com");
    assert_eq!(alpha["online"], true);
    assert_eq!(alpha["tags"], json!([]));
    // The copy command comes with a later task.
    assert!(alpha["command"].is_null());
    assert!(alpha.as_object().unwrap().contains_key("command"));
    // An offline peer, and a tagged peer of another user.
    assert_eq!(devices[1]["online"], false);
    assert_eq!(
        devices[1]["ips"],
        json!(["100.64.0.12", "fd7a:115c:a1e0::c"])
    );
    assert_eq!(devices[2]["tags"], json!(["tag:ci"]));
    assert_eq!(devices[2]["login"], "tagged-devices");
    assert!(devices[2].get("os").is_none());
}

/// The login of an owner, from the cookie in `h`.
async fn session_of(c: &BoardClient, h: &Headers) -> fleetdeck_deck::store::auth::LoginSession {
    let cookie = h[0].1.split_once('=').unwrap().1.to_owned();
    c.store
        .login_session(&Secret::new(cookie))
        .await
        .unwrap()
        .unwrap()
}

fn assignable(ip: IpAddr) -> bool {
    ip.is_ipv4() || ip.is_ipv6()
}

#[tokio::test]
async fn use_tailnet_is_an_elevated_save_that_keeps_manual_entries() {
    let tmp = tempfile::tempdir().unwrap();
    let bin = fake_tailscale(tmp.path(), "tailscale", STATUS);
    let (_f, c) = deck_board(tmp.path(), &toml_bin(&bin)).await;
    set_password(&c).await;
    let h = owner(&c).await;
    let session = session_of(&c, &h).await;

    // Without an elevation the new tailnet addresses are a danger change.
    let err = use_with(&c.state, &session, None, assignable)
        .await
        .err()
        .expect("the save needs an elevation");
    assert_eq!((err.status.as_u16(), err.code), (400, "elevation_required"));
    let keys = err.details.as_ref().unwrap()["keys"].clone();
    assert_eq!(keys, json!(["listen", "link_listen"]));
    let text = std::fs::read_to_string(tmp.path().join("deck.toml")).unwrap();
    assert!(!text.contains("100.64"), "{text}");
    assert!(
        c.audited("tailnet.use", AuditResult::Failed)
            .await
            .iter()
            .any(|d| d
                .as_deref()
                .is_some_and(|d| d.starts_with("elevation_required")))
    );

    // With an elevation: both families, the port of the first entry, the
    // LAN entry kept, and the host name set. The listeners need a restart.
    let token = elevate(&c, &h).await;
    let done = use_with(&c.state, &session, Some(token.clone()), assignable)
        .await
        .map_err(|e| e.message)
        .unwrap();
    let answer = serde_json::to_value(&done).unwrap();
    assert_eq!(
        answer,
        json!({
            "saved": true,
            "restart_needed": true,
            "listen": [
                "127.0.0.1:7420", "192.168.5.5:7420",
                "100.64.0.10:7420", "[fd7a:115c:a1e0::a]:7420"
            ],
            "link_listen": [
                "127.0.0.1:7421", "100.64.0.10:7421", "[fd7a:115c:a1e0::a]:7421"
            ],
            "tailnet_hostname": "deck-box.tail1234.ts.net"
        })
    );
    let text = std::fs::read_to_string(tmp.path().join("deck.toml")).unwrap();
    assert!(text.contains("[fd7a:115c:a1e0::a]:7421"), "{text}");
    assert!(text.contains("192.168.5.5:7420"), "{text}");
    // The audit row has the changed keys, never a value.
    let ok = c.audited("tailnet.use", AuditResult::Ok).await;
    assert_eq!(
        ok,
        vec![Some("link_listen,listen,tailnet_hostname".to_owned())]
    );
    let all = format!("{:?}", c.store.audit_log(100).await.unwrap());
    assert!(!all.contains("100.64.0.10"), "{all}");

    // A second run changes nothing: no write, no restart.
    let view = get(&c, &h, "/api/settings/deck").await;
    let revision = view.body["revision"].clone();
    let again = use_with(&c.state, &session, Some(token), assignable)
        .await
        .map_err(|e| e.message)
        .unwrap();
    assert_eq!(
        serde_json::to_value(&again).unwrap()["restart_needed"],
        false
    );
    let view = get(&c, &h, "/api/settings/deck").await;
    assert_eq!(view.body["revision"], revision);

    // The status route now shows the saved lists as `current`.
    let r = get(&c, &h, "/api/tailnet").await;
    assert_eq!(
        r.body["current"]["link_listen"].as_array().unwrap().len(),
        3
    );
}

#[tokio::test]
async fn use_tailnet_with_an_ipv6_only_address_brackets_it() {
    let tmp = tempfile::tempdir().unwrap();
    let only_v6 = r#"{"BackendState":"Running",
        "Self":{"DNSName":"v6.tail1234.ts.net.","TailscaleIPs":["100.64.0.10","fd7a:115c:a1e0::a"]}}"#;
    let bin = fake_tailscale(tmp.path(), "tailscale", only_v6);
    let (_f, c) = deck_board(tmp.path(), &toml_bin(&bin)).await;
    set_password(&c).await;
    let h = owner(&c).await;
    let session = session_of(&c, &h).await;
    let token = elevate(&c, &h).await;
    // Only the IPv6 address is assigned locally: only it is added.
    let done = use_with(&c.state, &session, Some(token), |ip: IpAddr| ip.is_ipv6())
        .await
        .map_err(|e| e.message)
        .unwrap();
    let answer: Value = serde_json::to_value(&done).unwrap();
    assert_eq!(
        answer["listen"],
        json!([
            "127.0.0.1:7420",
            "192.168.5.5:7420",
            "[fd7a:115c:a1e0::a]:7420"
        ])
    );
    assert_eq!(answer["tailnet_hostname"], "v6.tail1234.ts.net");
}
