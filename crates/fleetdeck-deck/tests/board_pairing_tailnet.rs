//! The pairing request routes of the board against a real deck node and a
//! station that asks over loopback (DL-178, tailnet design 5.1, 5.4 and 8):
//! the window with elevation, the pending record, approval and denial, the
//! commit-then-answer record, `PairResume` after a restart, and the devices
//! list. The deck listener is a loopback socket that has the rules of a
//! tailnet listener.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
mod pairing_support;
mod settings_support;

use std::collections::BTreeSet;
use std::time::Duration;

use common::*;
use fleetdeck_core::identity::Identity;
use fleetdeck_deck::Direction;
use fleetdeck_deck::store::AuditResult;
use fleetdeck_deck::store::stations::NewStation;
use fleetdeck_link::node::ListenerKind;
use fleetdeck_link::pair_hub::HubConfig;
use fleetdeck_link::pair_request::CanonicalRequest;
use fleetdeck_link::tls::Role;
use fleetdeck_link::{PairMode, request_pairing};
use fleetdeck_proto::link::Scope;
use serde_json::{Value, json};
use settings_support::*;

use pairing_support::*;
#[tokio::test]
async fn the_pairing_routes_are_owner_only_and_need_a_login_and_csrf() {
    let d = deck(Some(ListenerKind::Tailnet)).await;
    let c = &d.board;
    let h = owner(c).await;
    let routes = [
        ("GET", "/api/pairing/window"),
        ("POST", "/api/pairing/window"),
        ("GET", "/api/pairing/requests"),
        ("POST", "/api/pairing/requests/abc/approve"),
        ("POST", "/api/pairing/requests/abc/deny"),
    ];
    // A token with every scope is refused, `admin` included.
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
        let body = (method == "POST").then(|| json!({ "open": false }));
        let r = c
            .req(
                method,
                path,
                &[("Authorization", auth.as_str())],
                body.clone(),
            )
            .await;
        expect(&r, 403, "owner_only");
        let r = c.req(method, path, &[], body).await;
        assert_eq!(r.status, 401, "{method} {path}");
    }
    // A member of a shared session is refused too.
    let share = post(
        c,
        &h,
        "/api/shares",
        json!({ "kind": "board", "role": "approver" }),
    )
    .await;
    assert_eq!(share.status, 200, "{}", share.body);
    let token = share.body["url"]
        .as_str()
        .unwrap()
        .split("#join=")
        .nth(1)
        .unwrap();
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
        let body = (method == "POST").then(|| json!({ "open": false }));
        let r = c
            .req(
                method,
                path,
                &[("Cookie", cookie), ("X-CSRF-Token", csrf)],
                body,
            )
            .await;
        assert_eq!(r.status, 403, "{method} {path}: {}", r.body);
    }
    // The owner without the CSRF header: refused.
    let (cookie, _) = (h[0].1.clone(), h[1].1.clone());
    let r = c
        .req(
            "POST",
            "/api/pairing/window",
            &[("Cookie", cookie.as_str())],
            Some(json!({ "open": false })),
        )
        .await;
    expect(&r, 403, "csrf");
}

#[tokio::test]
async fn the_window_needs_elevation_to_open_a_listener_and_has_a_generation() {
    let d = deck(Some(ListenerKind::Tailnet)).await;
    let c = &d.board;
    let h = owner(c).await;
    let r = get(c, &h, "/api/pairing/window").await;
    assert_eq!(r.status, 200);
    assert_eq!(
        r.body,
        json!({ "open": false, "generation": 0, "deadline_ms": null })
    );

    // Without an elevation: refused, and the window stays closed.
    let r = post(c, &h, "/api/pairing/window", json!({ "open": true })).await;
    expect(&r, 400, "elevation_required");
    let r = post(
        c,
        &h,
        "/api/pairing/window",
        json!({ "open": true, "elevation": "not an elevation" }),
    )
    .await;
    expect(&r, 400, "elevation_required");
    assert_eq!(get(c, &h, "/api/pairing/window").await.body["open"], false);

    // With one: a window of 10 minutes, generation 1.
    let now = unix_ms();
    let w = open_window(c, &h).await;
    assert_eq!(
        (w["open"].as_bool(), w["generation"].as_u64()),
        (Some(true), Some(1))
    );
    let left = w["deadline_ms"].as_i64().unwrap() - now;
    assert!((590_000..=610_000).contains(&left), "{left}");
    // A reopening is a new generation with a new deadline.
    let w = open_window(c, &h).await;
    assert_eq!(w["generation"], 2);
    // Closing needs no elevation.
    let r = post(c, &h, "/api/pairing/window", json!({ "open": false })).await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(
        r.body,
        json!({ "open": false, "generation": 2, "deadline_ms": null })
    );
    // Unknown fields are refused.
    let r = post(
        c,
        &h,
        "/api/pairing/window",
        json!({ "open": false, "x": 1 }),
    )
    .await;
    expect(&r, 400, "invalid_body");

    let audited = |action: &'static str| c.audited(action, AuditResult::Ok);
    assert_eq!(audited("pairing.window_open").await.len(), 2);
    assert_eq!(audited("pairing.window_close").await.len(), 1);
}

#[tokio::test]
async fn the_window_does_not_open_without_a_tailnet_link_listener() {
    // A plain loopback listener is not a tailnet listener.
    let d = deck(None).await;
    let c = &d.board;
    let h = owner(c).await;
    let elevation = elevate(c, &h).await;
    let r = post(
        c,
        &h,
        "/api/pairing/window",
        json!({ "open": true, "elevation": elevation }),
    )
    .await;
    expect(&r, 409, "no_tailnet_listener");
    assert_eq!(get(c, &h, "/api/pairing/window").await.body["open"], false);
}

#[tokio::test]
async fn a_request_is_listed_approved_and_the_station_gets_the_signed_answer() {
    let d = deck(Some(ListenerKind::Tailnet)).await;
    let c = &d.board;
    let h = owner(c).await;
    open_window(c, &h).await;
    let station = station_config();
    let req = CanonicalRequest::generate("laptop", [Scope::Read, Scope::Drive].into()).unwrap();
    let asked = ask(&d, &station, &req, PairMode::Request);

    let list = listed(c, &h, 1).await;
    let item = &list[0];
    assert_eq!(item["request_id"], req.id_hex());
    assert_eq!(item["requested_name"], "laptop");
    assert_eq!(
        item["station_fingerprint"],
        station.identity.fingerprint().to_string()
    );
    assert_eq!(item["station_fingerprint"].as_str().unwrap().len(), 64);
    assert_eq!(item["offered_scopes"], json!(["read", "drive"]));
    assert_eq!(item["source_ip"], "127.0.0.1");
    assert_eq!(item["digest"].as_str().unwrap().len(), 64);
    let (admitted, expires) = (
        item["admitted_at"].as_i64().unwrap(),
        item["expires_at"].as_i64().unwrap(),
    );
    assert!((590_000..=610_000).contains(&(expires - admitted)));
    // The identity is informational; it comes with a later call.
    assert!(item["tailscale"]["tags"].is_array());
    let deadline = tokio::time::Instant::now() + T;
    let identity = loop {
        let now = listed(c, &h, 1).await.remove(0);
        if now["tailscale"]["error"].is_null() {
            break now["tailscale"].clone();
        }
        assert!(tokio::time::Instant::now() < deadline, "{now}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert_eq!(identity["login"], "owner@example.com");
    assert_eq!(identity["tags"], json!(["tag:dev"]));

    let elevation = elevate(c, &h).await;
    // No elevation: refused. A wrong key or digest: refused. Nothing is stored.
    let mut no_elevation = approve_body(item, &elevation);
    no_elevation.as_object_mut().unwrap().remove("elevation");
    expect(
        &post(c, &h, &approve_path(item), no_elevation).await,
        400,
        "elevation_required",
    );
    let mut wrong_key = approve_body(item, &elevation);
    wrong_key["fingerprint"] = json!(Identity::generate().unwrap().fingerprint().to_string());
    expect(
        &post(c, &h, &approve_path(item), wrong_key).await,
        409,
        "fingerprint_mismatch",
    );
    let mut wrong_digest = approve_body(item, &elevation);
    wrong_digest["digest"] = json!("00".repeat(32));
    expect(
        &post(c, &h, &approve_path(item), wrong_digest).await,
        409,
        "digest_mismatch",
    );
    let mut short = approve_body(item, &elevation);
    short["fingerprint"] = json!("abcd");
    expect(
        &post(c, &h, &approve_path(item), short).await,
        400,
        "invalid_body",
    );
    assert!(c.store.stations().await.unwrap().is_empty());

    // Approved.
    let r = post(c, &h, &approve_path(item), approve_body(item, &elevation)).await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body["approved"], true);
    let station_id = r.body["station_id"].as_str().unwrap().to_owned();
    let got = answer(asked).await.unwrap();
    assert_eq!(got.deck, d.identity.public_key());
    assert_eq!(got.granted_scopes, [Scope::Admin].into());

    // The station row: the requested name, `admin` and a dialing station.
    let stations = c.store.stations().await.unwrap();
    assert_eq!(stations.len(), 1);
    let st = &stations[0];
    assert_eq!(st.station_id.as_str(), station_id);
    assert_eq!(st.name, "laptop");
    assert_eq!(st.key, station.identity.public_key());
    assert_eq!(st.scopes, BTreeSet::from([Scope::Admin]));
    assert_eq!(st.direction, Direction::StationDials);
    assert!(st.dial_url.is_none());
    // The completion record holds the whole approval context.
    let done = c
        .store
        .pairing_completion(req.id_hex())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(done.station_key, station.identity.public_key());
    assert_eq!(done.deck_key, d.identity.public_key());
    assert_eq!(done.request_digest, req.digest());
    assert_eq!(done.nonce, req.nonce);
    assert_eq!(done.requested_name, "laptop");
    assert_eq!(
        done.offered_scopes,
        BTreeSet::from([Scope::Read, Scope::Drive])
    );
    assert_eq!(done.granted_scopes, BTreeSet::from([Scope::Admin]));
    assert_eq!(done.window_generation, 1);
    // The list is empty now, and the audit rows hold ids and fingerprints.
    listed(c, &h, 0).await;
    let rows = c.audited("pairing.approve", AuditResult::Ok).await;
    assert_eq!(rows, vec![Some(station.identity.fingerprint().to_string())]);
    let failed = c.audited("pairing.approve", AuditResult::Failed).await;
    assert_eq!(failed.len(), 4, "{failed:?}");
    // The station can now link: the key is pinned at the deck.
    let link_station = station.clone();
    link_station.pins.pin(d.identity.public_key(), Role::Deck);
    let link = fleetdeck_link::dial(&link_station, d.listen, d.identity.public_key()).await;
    assert!(link.is_ok(), "{:?}", link.err());
}

#[tokio::test]
async fn deny_closes_the_request_with_denied_and_audits_it() {
    let d = deck(Some(ListenerKind::Tailnet)).await;
    let c = &d.board;
    let h = owner(c).await;
    open_window(c, &h).await;
    let station = station_config();
    let req = CanonicalRequest::generate("laptop", [Scope::Read].into()).unwrap();
    let asked = ask(&d, &station, &req, PairMode::Request);
    let list = listed(c, &h, 1).await;
    let path = format!(
        "/api/pairing/requests/{}/deny",
        list[0]["request_id"].as_str().unwrap()
    );
    let r = post(c, &h, &path, json!({})).await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body, json!({ "denied": true }));
    assert_eq!(code(&answer(asked).await), "denied");
    // A second denial and an approval of the same id find nothing.
    expect(&post(c, &h, &path, json!({})).await, 404, "unknown_request");
    let elevation = elevate(c, &h).await;
    let r = post(
        c,
        &h,
        &approve_path(&list[0]),
        approve_body(&list[0], &elevation),
    )
    .await;
    expect(&r, 404, "unknown_request");
    assert!(c.store.stations().await.unwrap().is_empty());
    assert_eq!(c.audited("pairing.deny", AuditResult::Ok).await.len(), 1);
    // The cooldown: the same key is denied again for 10 minutes.
    let again = CanonicalRequest::generate("laptop", [Scope::Read].into()).unwrap();
    assert_eq!(
        code(&answer(ask(&d, &station, &again, PairMode::Request)).await),
        "denied"
    );
}

#[tokio::test]
async fn an_approval_after_a_close_or_a_close_and_reopen_is_refused() {
    let d = deck(Some(ListenerKind::Tailnet)).await;
    let c = &d.board;
    let h = owner(c).await;
    open_window(c, &h).await;
    let station = station_config();
    let req = CanonicalRequest::generate("laptop", [Scope::Read].into()).unwrap();
    let asked = ask(&d, &station, &req, PairMode::Request);
    let item = listed(c, &h, 1).await.remove(0);
    // Close: the station is told; an approval of that request is refused.
    let r = post(c, &h, "/api/pairing/window", json!({ "open": false })).await;
    assert_eq!(r.status, 200);
    assert_eq!(code(&answer(asked).await), "window_closed");
    let elevation = elevate(c, &h).await;
    let r = post(c, &h, &approve_path(&item), approve_body(&item, &elevation)).await;
    expect(&r, 409, "window_closed");
    // Close and reopen: a new generation, the old request is still refused.
    open_window(c, &h).await;
    let r = post(c, &h, &approve_path(&item), approve_body(&item, &elevation)).await;
    expect(&r, 409, "window_closed");
    assert!(c.store.stations().await.unwrap().is_empty());
}

#[tokio::test]
async fn a_duplicate_name_is_refused_and_the_request_stays_for_a_denial() {
    let d = deck(Some(ListenerKind::Tailnet)).await;
    let c = &d.board;
    let h = owner(c).await;
    c.store
        .pin_station(NewStation {
            key: Identity::generate().unwrap().public_key(),
            name: "laptop".into(),
            scopes: BTreeSet::from([Scope::Admin]),
            dial_url: None,
        })
        .await
        .unwrap();
    open_window(c, &h).await;
    let station = station_config();
    let req = CanonicalRequest::generate("laptop", [Scope::Read].into()).unwrap();
    let asked = ask(&d, &station, &req, PairMode::Request);
    let item = listed(c, &h, 1).await.remove(0);
    let elevation = elevate(c, &h).await;
    let r = post(c, &h, &approve_path(&item), approve_body(&item, &elevation)).await;
    expect(&r, 409, "name_taken");
    assert_eq!(listed(c, &h, 1).await.len(), 1, "the request stays pending");
    assert_eq!(c.store.stations().await.unwrap().len(), 1);
    let path = format!(
        "/api/pairing/requests/{}/deny",
        item["request_id"].as_str().unwrap()
    );
    assert_eq!(post(c, &h, &path, json!({})).await.status, 200);
    assert_eq!(code(&answer(asked).await), "denied");
}

#[tokio::test]
async fn a_key_that_became_paired_is_refused_and_never_changed() {
    let d = deck(Some(ListenerKind::Tailnet)).await;
    let c = &d.board;
    let h = owner(c).await;
    open_window(c, &h).await;
    let station = station_config();
    let req = CanonicalRequest::generate("laptop", [Scope::Read].into()).unwrap();
    let asked = ask(&d, &station, &req, PairMode::Request);
    let item = listed(c, &h, 1).await.remove(0);
    // The same key is paired by another way while the request waits.
    c.store
        .pin_station(NewStation {
            key: station.identity.public_key(),
            name: "old-name".into(),
            scopes: BTreeSet::from([Scope::Read]),
            dial_url: Some("100.64.0.9:7421".into()),
        })
        .await
        .unwrap();
    let elevation = elevate(c, &h).await;
    let r = post(c, &h, &approve_path(&item), approve_body(&item, &elevation)).await;
    expect(&r, 409, "already_paired");
    let st = c.store.stations().await.unwrap().remove(0);
    assert_eq!(
        (st.name.as_str(), st.dial_url.as_deref(), st.scopes.clone()),
        (
            "old-name",
            Some("100.64.0.9:7421"),
            BTreeSet::from([Scope::Read])
        )
    );
    assert!(
        c.store
            .pairing_completion(req.id_hex())
            .await
            .unwrap()
            .is_none()
    );
    asked.abort();
}

#[tokio::test]
async fn a_lost_answer_is_resumed_after_a_deck_restart_and_a_removal_deletes_the_record() {
    let d = deck(Some(ListenerKind::Tailnet)).await;
    let c = &d.board;
    let h = owner(c).await;
    open_window(c, &h).await;
    let station = station_config();
    let req = CanonicalRequest::generate("laptop", [Scope::Read].into()).unwrap();
    // The station sends its request by hand and never reads the answer.
    let (mut ws, _, _) = fleetdeck_link::open_pair(&station, d.listen, d.identity.fingerprint())
        .await
        .unwrap();
    let frame = json!({
        "type": "pair_request", "request_id": req.id_hex(), "name": "laptop",
        "nonce": req.nonce_hex(), "offered_scopes": ["read"],
    });
    futures_util::SinkExt::send(
        &mut ws,
        tokio_tungstenite::tungstenite::Message::text(frame.to_string()),
    )
    .await
    .unwrap();
    let item = listed(c, &h, 1).await.remove(0);
    let elevation = elevate(c, &h).await;
    let r = post(c, &h, &approve_path(&item), approve_body(&item, &elevation)).await;
    assert_eq!(r.status, 200, "{}", r.body);
    // The answer is lost: the station drops the connection unread.
    drop(ws);
    assert!(
        c.store
            .pairing_completion(req.id_hex())
            .await
            .unwrap()
            .is_some()
    );
    // The deck restarts: same key, same database.
    d.node.shutdown();
    let node2 = start_node(
        &d.db,
        &d.identity,
        Some(ListenerKind::Tailnet),
        HubConfig::default(),
    )
    .await;
    let addr2 = node2.listen_addrs()[0];
    node2.pair_requests().open_window().await.unwrap();
    let resumed = tokio::time::timeout(
        T,
        request_pairing(
            &station,
            addr2,
            d.identity.fingerprint(),
            &req,
            PairMode::Resume,
            Duration::from_secs(5),
            || Ok(()),
        ),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(resumed.deck, d.identity.public_key());
    // Removing the station deletes its record: a resume finds nothing.
    node2.remove_station("laptop").await.unwrap();
    assert!(
        node2
            .link()
            .store
            .pairing_completion(req.id_hex())
            .await
            .unwrap()
            .is_none()
    );
    let r = tokio::time::timeout(
        T,
        request_pairing(
            &station,
            addr2,
            d.identity.fingerprint(),
            &req,
            PairMode::Resume,
            Duration::from_secs(5),
            || Ok(()),
        ),
    )
    .await
    .unwrap();
    assert_eq!(r.unwrap_err().reason(), Some("unknown_request"));
}

#[tokio::test]
async fn a_shutdown_ends_a_waiting_request_with_cancelled() {
    let d = deck(Some(ListenerKind::Tailnet)).await;
    let c = &d.board;
    let h = owner(c).await;
    open_window(c, &h).await;
    let station = station_config();
    let req = CanonicalRequest::generate("laptop", [Scope::Read].into()).unwrap();
    let asked = ask(&d, &station, &req, PairMode::Request);
    listed(c, &h, 1).await;
    d.node.shutdown();
    assert_eq!(code(&answer(asked).await), "cancelled");
}

#[tokio::test]
async fn the_devices_list_shows_a_waiting_request_and_the_station_command() {
    let d = deck(Some(ListenerKind::Tailnet)).await;
    let c = &d.board;
    let h = owner(c).await;
    let port = d.listen.port();
    let fp = d.identity.fingerprint().to_string();

    // Window closed: no command.
    let r = get(c, &h, "/api/tailnet/devices").await;
    assert_eq!(r.status, 200, "{}", r.body);
    for dev in r.body["devices"].as_array().unwrap() {
        assert_eq!(dev["command"], Value::Null);
        assert_eq!(dev["state"], "none");
    }

    open_window(c, &h).await;
    // A request from the address of `alpha`.
    let hub = d.node.pair_requests();
    let req = CanonicalRequest::generate("alpha", [Scope::Read].into()).unwrap();
    let _held = hub
        .admit(
            req,
            Identity::generate().unwrap().public_key(),
            "100.64.0.11".parse().unwrap(),
            false,
        )
        .await
        .unwrap();
    // A paired device: bravo (dialed at its address).
    c.store
        .pin_station(NewStation {
            key: Identity::generate().unwrap().public_key(),
            name: "bravo-station".into(),
            scopes: BTreeSet::from([Scope::Admin]),
            dial_url: Some("100.64.0.12:7421".into()),
        })
        .await
        .unwrap();
    let r = get(c, &h, "/api/tailnet/devices").await;
    assert_eq!(r.status, 200, "{}", r.body);
    let devices = r.body["devices"].as_array().unwrap();
    let find = |name: &str| devices.iter().find(|d| d["host_name"] == name).unwrap();
    let command = format!(
        "fleetdeck station join --tailnet deck-box.tail1234.ts.net:{port} --deck-fingerprint {fp}"
    );
    assert_eq!(find("alpha")["state"], "request waiting");
    assert_eq!(find("alpha")["command"], command);
    assert_eq!(find("bravo")["state"], "paired");
    assert_eq!(find("bravo")["command"], Value::Null);
    // Closed again: the command goes away.
    post(c, &h, "/api/pairing/window", json!({ "open": false })).await;
    let r = get(c, &h, "/api/tailnet/devices").await;
    assert!(
        r.body["devices"]
            .as_array()
            .unwrap()
            .iter()
            .all(|d| d["command"].is_null())
    );
}

#[tokio::test]
async fn the_command_uses_the_first_tailnet_address_when_the_deck_has_no_dns_name() {
    let d = deck_with(
        Some(ListenerKind::Tailnet),
        HubConfig::default(),
        NO_DNS_STATUS,
    )
    .await;
    let c = &d.board;
    let h = owner(c).await;
    open_window(c, &h).await;
    let r = get(c, &h, "/api/tailnet/devices").await;
    let device = &r.body["devices"][0];
    let want = format!(
        "fleetdeck station join --tailnet [fd7a:115c:a1e0::a]:{}",
        d.listen.port()
    );
    assert!(
        device["command"].as_str().unwrap().starts_with(&want),
        "{}",
        device["command"]
    );
    assert!(
        device["command"]
            .as_str()
            .unwrap()
            .ends_with(&d.identity.fingerprint().to_string())
    );
}
