//! R13 P6 task F1 on the deck, over the board API: the grant classes
//! `desktop` and `capture`, their caps, the elevation, the features of the
//! caller and of the target stations, and the board view of the counters.
//! (The cap rules of the store are in `src/store/grants_desktop_tests.rs`.)

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
mod settings_support;

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use common::board::{Http, board};
use common::*;
use fleetdeck_core::identity::Identity;
use fleetdeck_deck::store::DeckStore;
use fleetdeck_deck::store::stations::NewStation;
use fleetdeck_proto::StationId;
use fleetdeck_proto::link::Scope;
use fleetdeck_proto::version::feature;
use serde_json::{Value, json};
use settings_support::*;

const START: &str = "/api/commands/session.start";
const UNSUPPORTED: &str = "The selected peers do not support this desktop grant.";

fn start_body(id: &str, coordinator: Value, elevation: Option<&str>) -> Value {
    let mut spec = json!({"grant_id": "", "classes": ["desktop"], "idle_ttl_ms": 0,
        "expires_at": 0, "control_skill": {"version": "", "text": ""}});
    spec.as_object_mut()
        .unwrap()
        .extend(coordinator.as_object().unwrap().clone());
    let mut body = json!({"station": "fake", "command_id": id, "command": {"op": "start",
        "prompt": null, "spec": {"provider_instance": "claude", "cwd": "/w", "mode": null,
        "model": null, "title": null, "coordinator": spec}}});
    if let Some(e) = elevation {
        body["elevation"] = json!(e);
    }
    body
}

/// The features of a target station that a `desktop` grant needs.
fn target_features() -> Vec<String> {
    [
        feature::COORDINATOR_V2,
        feature::DESKTOP,
        feature::DESKTOP_AUTHORITY,
        feature::DESKTOP_MCP,
    ]
    .map(str::to_owned)
    .to_vec()
}

/// A paired station of the deck with one live link of the given features.
async fn extra(store: &DeckStore, name: &str, features: Vec<String>) -> StationId {
    let rec = store
        .pin_station(NewStation {
            key: Identity::generate().unwrap().public_key(),
            name: name.to_owned(),
            scopes: BTreeSet::from([Scope::Admin]),
            dial_url: None,
        })
        .await
        .unwrap();
    store.set_link_features(&rec.station_id, 1, features);
    rec.station_id
}

#[tokio::test]
async fn the_new_classes_need_the_elevation_and_the_caps_follow_the_texts_of_the_brief() {
    let mut f = fixture().await;
    let mut station = f.connect().await;
    let c = board(Arc::clone(&f.ctx)).await;
    set_password(&c).await;
    let h = owner(&c).await;
    for classes in [["desktop"], ["capture"]] {
        let r = post(
            &c,
            &h,
            START,
            start_body("a1", json!({"classes": classes}), None),
        )
        .await;
        expect(&r, 400, "elevation_required");
    }
    // A bad cap is a bad request with or without the elevation, with the
    // fixed text: the class rule first, then the range, then the multiple.
    let cases = [
        (
            json!({"classes": ["read"], "desktop_cap": 1}),
            "desktop_cap needs the desktop or capture class",
        ),
        (
            json!({"classes": ["desktop"], "capture_cap": 1}),
            "capture_cap and image_byte_cap need the capture class",
        ),
        (
            json!({"classes": ["desktop"], "image_byte_cap": 1_048_576}),
            "capture_cap and image_byte_cap need the capture class",
        ),
        (
            json!({"classes": ["desktop"], "desktop_cap": 5001}),
            "desktop_cap is 0 to 5000",
        ),
        (
            json!({"classes": ["capture"], "capture_cap": 5001}),
            "capture_cap is 0 to 5000",
        ),
        (
            json!({"classes": ["capture"], "image_byte_cap": 1_073_741_825_u64}),
            "image_byte_cap is 0 to 1073741824",
        ),
        (
            json!({"classes": ["capture"], "image_byte_cap": 1_048_577}),
            "image_byte_cap is a multiple of 1048576",
        ),
        // The class rule wins over the range.
        (
            json!({"classes": ["read"], "desktop_cap": 9999, "capture_cap": 1}),
            "desktop_cap needs the desktop or capture class",
        ),
    ];
    let token = elevate(&c, &h).await;
    for (spec, text) in cases {
        for elevation in [None, Some(token.as_str())] {
            let r = post(&c, &h, START, start_body("b1", spec.clone(), elevation)).await;
            expect(&r, 400, "invalid_grant");
            assert!(r.body.to_string().contains(text), "{text}: {}", r.body);
        }
    }
    no_request(&mut station, Duration::from_millis(200)).await;
    assert!(c.store.grants(10).await.unwrap().is_empty());
}

#[tokio::test]
async fn a_desktop_grant_is_stored_with_its_defaults_and_the_station_never_sees_the_caps() {
    let mut f = fixture().await;
    let mut station = f.connect().await;
    let c = board(Arc::clone(&f.ctx)).await;
    set_password(&c).await;
    let h = owner(&c).await;
    let token = elevate(&c, &h).await;
    // `desktop` alone: the action cap defaults to 300.
    let call = post(
        &c,
        &h,
        START,
        start_body("d1", json!({"classes": ["desktop"]}), Some(&token)),
    );
    let (r, req) = tokio::join!(call, Fixture::answer(&mut station, accepted));
    assert_eq!(r.status, 200, "{}", r.body);
    let sent = req.params["command"]["spec"]["coordinator"].clone();
    for key in ["desktop_cap", "capture_cap", "image_byte_cap"] {
        assert!(sent.get(key).is_none(), "{key}: {sent}");
    }
    assert_eq!(sent["classes"], json!(["desktop"]));
    let g = c.store.grants(10).await.unwrap().remove(0);
    assert_eq!(
        (g.desktop_cap, g.capture_cap, g.image_byte_cap),
        (Some(300), None, None)
    );
    assert_eq!(
        (g.desktop_used, g.capture_used, g.image_byte_used),
        (0, 0, 0)
    );
    // `capture` alone brings all three defaults; given values are kept, and
    // the station never sees them.
    let call = post(
        &c,
        &h,
        START,
        start_body("d2", json!({"classes": ["capture"]}), Some(&token)),
    );
    let (r, _) = tokio::join!(call, Fixture::answer(&mut station, accepted));
    assert_eq!(r.status, 200, "{}", r.body);
    let g = c.store.grants(10).await.unwrap().remove(0);
    assert_eq!(
        (g.desktop_cap, g.capture_cap, g.image_byte_cap),
        (Some(300), Some(20), Some(67_108_864))
    );
    let spec = json!({"classes": ["desktop", "capture"], "desktop_cap": 5000,
        "capture_cap": 0, "image_byte_cap": 1_073_741_824_u64});
    let call = post(&c, &h, START, start_body("d3", spec, Some(&token)));
    let (r, req) = tokio::join!(call, Fixture::answer(&mut station, accepted));
    assert_eq!(r.status, 200, "{}", r.body);
    let sent = req.params["command"]["spec"]["coordinator"].clone();
    for key in ["desktop_cap", "capture_cap", "image_byte_cap"] {
        assert!(sent.get(key).is_none(), "{key}: {sent}");
    }
    assert_eq!(sent["classes"], json!(["desktop", "capture"]));
    let newest = c.store.grants(10).await.unwrap().remove(0);
    assert_eq!(
        (
            newest.desktop_cap,
            newest.capture_cap,
            newest.image_byte_cap
        ),
        (Some(5000), Some(0), Some(1_073_741_824))
    );
    // The board list shows a counter only with its own cap.
    let list = get(&c, &h, "/api/coordinator/grants").await;
    let grants = list.body.as_array().unwrap();
    assert_eq!(grants.len(), 3);
    let by_classes = |classes: Value| {
        grants
            .iter()
            .find(|g| g["classes"] == classes)
            .unwrap_or_else(|| panic!("no grant with {classes}"))
    };
    let desktop = by_classes(json!(["desktop"]));
    assert_eq!(desktop["desktop_cap"], 300);
    assert_eq!(desktop["desktop_used"], 0);
    for key in [
        "capture_cap",
        "capture_used",
        "image_byte_cap",
        "image_byte_used",
    ] {
        assert!(desktop.get(key).is_none(), "{key}: {desktop}");
    }
    let both = by_classes(json!(["desktop", "capture"]));
    assert_eq!(both["image_byte_cap"], 1_073_741_824_u64);
    assert_eq!(both["image_byte_used"], 0);
    assert_eq!(both["capture_cap"], 0);
    assert_eq!(both["capture_used"], 0);
    // A grant without the new classes has all six fields absent.
    let call = post(
        &c,
        &h,
        START,
        start_body("d4", json!({"classes": ["read", "inspect"]}), Some(&token)),
    );
    let (r, _) = tokio::join!(call, Fixture::answer(&mut station, accepted));
    assert_eq!(r.status, 200, "{}", r.body);
    let list = get(&c, &h, "/api/coordinator/grants").await;
    let plain = list
        .body
        .as_array()
        .unwrap()
        .iter()
        .find(|g| g["classes"] == json!(["read", "inspect"]))
        .unwrap();
    for key in [
        "desktop_cap",
        "desktop_used",
        "capture_cap",
        "capture_used",
        "image_byte_cap",
        "image_byte_used",
    ] {
        assert!(plain.get(key).is_none(), "{key}: {plain}");
    }
}

#[tokio::test]
async fn the_caller_and_the_target_stations_need_their_features_but_the_callers_own_desktop_does_not()
 {
    let mut f = fixture().await;
    let mut station = f.connect().await;
    let c = board(Arc::clone(&f.ctx)).await;
    set_password(&c).await;
    let h = owner(&c).await;
    let token = elevate(&c, &h).await;
    let desktop = json!({"classes": ["desktop"]});
    let ask = |id: &str, spec: Value| post(&c, &h, START, start_body(id, spec, Some(&token)));
    let refused = |r: &Http, what: &str| {
        expect(r, 409, "unsupported");
        assert!(
            r.body.to_string().contains(UNSUPPORTED),
            "{what}: {}",
            r.body
        );
    };

    // A second live link of the caller without `desktop_mcp.v1` (and without
    // the capture feature for a capture grant).
    let caller_features = vec![
        feature::COORDINATOR.to_owned(),
        feature::COORDINATOR_V2.to_owned(),
    ];
    f.store
        .set_link_features(&f.station, 98, caller_features.clone());
    refused(
        &ask("f1", desktop.clone()).await,
        "caller without desktop_mcp",
    );
    // With `desktop_mcp.v1` the desktop grant passes. The caller's own link
    // needs no target feature: this second link has none of them.
    let mut ok = caller_features.clone();
    ok.push(feature::DESKTOP_MCP.to_owned());
    f.store.set_link_features(&f.station, 98, ok.clone());
    let (r, _) = tokio::join!(
        ask("f2", desktop.clone()),
        Fixture::answer(&mut station, accepted)
    );
    assert_eq!(r.status, 200, "{}", r.body);
    // A capture grant also needs `capture_mcp.v1` on the caller.
    let capture = json!({"classes": ["capture"]});
    refused(
        &ask("f3", capture.clone()).await,
        "caller without capture_mcp",
    );
    ok.push(feature::CAPTURE_MCP.to_owned());
    f.store.set_link_features(&f.station, 98, ok);
    let (r, _) = tokio::join!(
        ask("f4", capture.clone()),
        Fixture::answer(&mut station, accepted)
    );
    assert_eq!(r.status, 200, "{}", r.body);
    f.store.clear_link_features(&f.station, 98);
    let stored = c.store.grants(10).await.unwrap().len();

    // A target station: every target feature of the class, on its link.
    let alpha = extra(&f.store, "alpha", target_features()).await;
    let with_target = |classes: Value| json!({"classes": classes, "stations": [alpha]});
    let (r, _) = tokio::join!(
        ask("g1", with_target(json!(["desktop"]))),
        Fixture::answer(&mut station, accepted)
    );
    assert_eq!(r.status, 200, "{}", r.body);
    // A capture grant needs the capture features on the target too.
    refused(
        &ask("g2", with_target(json!(["capture"]))).await,
        "target without capture features",
    );
    let mut all = target_features();
    all.extend(
        [
            feature::DESKTOP_CAPTURE,
            feature::ARTIFACTS_DELETE,
            feature::CAPTURE_MCP,
        ]
        .map(str::to_owned),
    );
    let beta = extra(&f.store, "beta", all).await;
    let (r, _) = tokio::join!(
        ask("g3", json!({"classes": ["capture"], "stations": [beta]})),
        Fixture::answer(&mut station, accepted)
    );
    assert_eq!(r.status, 200, "{}", r.body);
    // Each target feature is needed: take them away one by one.
    for missing in target_features() {
        let some: Vec<String> = target_features()
            .into_iter()
            .filter(|f| *f != missing)
            .collect();
        let gamma = extra(&f.store, &format!("gamma-{missing}"), some).await;
        if missing == feature::COORDINATOR_V2 {
            // The existing check of every grant station answers first.
            expect(
                &ask("h", json!({"classes": ["desktop"], "stations": [gamma]})).await,
                409,
                "unsupported",
            );
        } else {
            refused(
                &ask("h", json!({"classes": ["desktop"], "stations": [gamma]})).await,
                &missing,
            );
        }
    }
    // A target with no live link is refused too (by the check of every grant
    // station that comes first).
    let down = extra(&f.store, "down", target_features()).await;
    f.store.clear_link_features(&down, 1);
    expect(
        &ask("i", json!({"classes": ["desktop"], "stations": [down]})).await,
        409,
        "unsupported",
    );
    no_request(&mut station, Duration::from_millis(200)).await;
    assert_eq!(c.store.grants(20).await.unwrap().len(), stored + 2);
}
