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

/// The two caps never go on to a station; the flag `allow_recording` goes on
/// only with `capture_recording_mcp.v1` on the caller's link (R13 P5b-2,
/// decision O12-22).
const RECORDING_KEYS: [&str; 2] = ["recording_cap", "recording_second_cap"];

#[tokio::test]
async fn the_recording_opt_in_follows_the_rules_of_the_brief_and_refuses_before_anything_is_stored()
{
    let mut f = fixture().await;
    let mut station = f.connect().await;
    let c = board(Arc::clone(&f.ctx)).await;
    set_password(&c).await;
    let h = owner(&c).await;
    let token = elevate(&c, &h).await;
    // The class rule, the opt-in rule and the ranges, with or without the
    // elevation, in this order.
    let cases = [
        (
            json!({"classes": ["desktop"], "allow_recording": true}),
            "allow_recording needs the capture class",
        ),
        (
            json!({"classes": ["read"], "allow_recording": true, "recording_cap": 1}),
            "allow_recording needs the capture class",
        ),
        (
            json!({"classes": ["capture"], "recording_cap": 1}),
            "recording caps need allow_recording",
        ),
        (
            json!({"classes": ["capture"], "recording_second_cap": 60}),
            "recording caps need allow_recording",
        ),
        (
            json!({"classes": ["capture"], "allow_recording": false, "recording_cap": 0}),
            "recording caps need allow_recording",
        ),
        (
            json!({"classes": ["capture"], "allow_recording": true, "recording_cap": 5001}),
            "recording_cap is 0 to 5000",
        ),
        (
            json!({"classes": ["capture"], "allow_recording": true,
                "recording_second_cap": 600_001}),
            "recording_second_cap is 0 to 600000",
        ),
    ];
    for (spec, text) in cases {
        for elevation in [None, Some(token.as_str())] {
            let r = post(&c, &h, START, start_body("r1", spec.clone(), elevation)).await;
            expect(&r, 400, "invalid_grant");
            assert!(r.body.to_string().contains(text), "{text}: {}", r.body);
        }
    }
    // A valid recording grant still needs the owner's elevation.
    let r = post(
        &c,
        &h,
        START,
        start_body(
            "r2",
            json!({"classes": ["capture"], "allow_recording": true}),
            None,
        ),
    )
    .await;
    expect(&r, 400, "elevation_required");
    no_request(&mut station, Duration::from_millis(200)).await;
    assert!(c.store.grants(10).await.unwrap().is_empty());
}

#[tokio::test]
async fn a_recording_grant_gets_the_defaults_or_the_given_caps_and_the_station_never_sees_them() {
    let mut f = fixture().await;
    let mut station = f.connect().await;
    let c = board(Arc::clone(&f.ctx)).await;
    set_password(&c).await;
    let h = owner(&c).await;
    let token = elevate(&c, &h).await;
    // `allow_recording` alone: the defaults 5 and 600.
    let call = post(
        &c,
        &h,
        START,
        start_body(
            "w1",
            json!({"classes": ["capture"], "allow_recording": true}),
            Some(&token),
        ),
    );
    let (r, req) = tokio::join!(call, Fixture::answer(&mut station, accepted));
    assert_eq!(r.status, 200, "{}", r.body);
    let sent = req.params["command"]["spec"]["coordinator"].clone();
    for key in RECORDING_KEYS {
        assert!(sent.get(key).is_none(), "{key}: {sent}");
    }
    // The caller's link negotiated the feature: the flag goes on.
    assert_eq!(sent["allow_recording"], true, "{sent}");
    let g = c.store.grants(10).await.unwrap().remove(0);
    assert_eq!(
        (
            g.recording_cap,
            g.recording_used,
            g.recording_second_cap,
            g.recording_second_used
        ),
        (5, 0, 600, 0)
    );
    // Given caps are kept, with the range limits and zero.
    let mut given = Vec::new();
    for (id, cap, seconds) in [("w2", 5000, 600_000), ("w3", 0, 0), ("w4", 7, 90)] {
        let spec = json!({"classes": ["desktop", "capture"], "allow_recording": true,
            "recording_cap": cap, "recording_second_cap": seconds});
        let call = post(&c, &h, START, start_body(id, spec, Some(&token)));
        let (r, req) = tokio::join!(call, Fixture::answer(&mut station, accepted));
        assert_eq!(r.status, 200, "{}", r.body);
        let sent = req.params["command"]["spec"]["coordinator"].clone();
        for key in RECORDING_KEYS {
            assert!(sent.get(key).is_none(), "{key}: {sent}");
        }
        assert_eq!(sent["allow_recording"], true, "{sent}");
        let g = c.store.grants(10).await.unwrap().remove(0);
        assert_eq!((g.recording_cap, g.recording_second_cap), (cap, seconds));
        given.push(g.grant_id);
    }
    // One explicit cap: the other one takes its default.
    let spec = json!({"classes": ["capture"], "allow_recording": true, "recording_cap": 2});
    let call = post(&c, &h, START, start_body("w5", spec, Some(&token)));
    let (r, _) = tokio::join!(call, Fixture::answer(&mut station, accepted));
    assert_eq!(r.status, 200, "{}", r.body);
    let g = c.store.grants(10).await.unwrap().remove(0);
    assert_eq!((g.recording_cap, g.recording_second_cap), (2, 600));
    // A screenshot-only grant has zero caps and no recording field in the view.
    let call = post(
        &c,
        &h,
        START,
        start_body("w6", json!({"classes": ["capture"]}), Some(&token)),
    );
    let (r, req) = tokio::join!(call, Fixture::answer(&mut station, accepted));
    assert_eq!(r.status, 200, "{}", r.body);
    // A grant without the opt-in never sends the flag.
    let sent = req.params["command"]["spec"]["coordinator"].clone();
    assert!(sent.get("allow_recording").is_none(), "{sent}");
    let shot = c.store.grants(10).await.unwrap().remove(0);
    assert_eq!((shot.recording_cap, shot.recording_second_cap), (0, 0));
    let list = get(&c, &h, "/api/coordinator/grants").await;
    let grants = list.body.as_array().unwrap();
    assert_eq!(grants.len(), 6);
    let view_of = |id: &str| {
        grants
            .iter()
            .find(|g| g["grant_id"] == id)
            .unwrap_or_else(|| panic!("no grant {id}"))
    };
    let shot_view = view_of(&shot.grant_id);
    for key in [
        "recording_cap",
        "recording_used",
        "recording_second_cap",
        "recording_second_used",
    ] {
        assert!(shot_view.get(key).is_none(), "{key}: {shot_view}");
    }
    let defaults = c
        .store
        .grants(10)
        .await
        .unwrap()
        .into_iter()
        .last()
        .unwrap();
    let defaults_view = view_of(&defaults.grant_id);
    assert_eq!(defaults_view["recording_cap"], 5);
    assert_eq!(defaults_view["recording_used"], 0);
    assert_eq!(defaults_view["recording_second_cap"], 600);
    assert_eq!(defaults_view["recording_second_used"], 0);
    // A recording grant with caps of zero shows no recording field either.
    let zero_view = view_of(&given[1]);
    assert!(zero_view.get("recording_cap").is_none(), "{zero_view}");
    assert!(zero_view.get("recording_used").is_none(), "{zero_view}");
    assert_eq!(view_of(&given[0])["recording_second_cap"], 600_000);
    assert_eq!(view_of(&given[2])["recording_cap"], 7);
}

#[tokio::test]
async fn a_recording_grant_needs_capture_recording_mcp_on_the_caller_and_on_every_other_station() {
    let mut f = fixture().await;
    let mut station = f.connect().await;
    let c = board(Arc::clone(&f.ctx)).await;
    set_password(&c).await;
    let h = owner(&c).await;
    let token = elevate(&c, &h).await;
    let ask = |id: &str, spec: Value| post(&c, &h, START, start_body(id, spec, Some(&token)));
    let refused = |r: &Http, what: &str| {
        expect(r, 409, "unsupported");
        assert!(
            r.body.to_string().contains(UNSUPPORTED),
            "{what}: {}",
            r.body
        );
    };
    let recording = json!({"classes": ["capture"], "allow_recording": true});
    // A second live link of the caller with the screenshot features only.
    let mut caller = vec![
        feature::COORDINATOR.to_owned(),
        feature::COORDINATOR_V2.to_owned(),
        feature::DESKTOP_MCP.to_owned(),
        feature::CAPTURE_MCP.to_owned(),
    ];
    f.store.set_link_features(&f.station, 98, caller.clone());
    refused(
        &ask("m1", recording.clone()).await,
        "caller without capture_recording_mcp",
    );
    // The screenshot-only grant of the same caller is not affected.
    let (r, _) = tokio::join!(
        ask("m2", json!({"classes": ["capture"]})),
        Fixture::answer(&mut station, accepted)
    );
    assert_eq!(r.status, 200, "{}", r.body);
    caller.push(feature::CAPTURE_RECORDING_MCP.to_owned());
    f.store.set_link_features(&f.station, 98, caller);
    let (r, _) = tokio::join!(
        ask("m3", recording.clone()),
        Fixture::answer(&mut station, accepted)
    );
    assert_eq!(r.status, 200, "{}", r.body);
    f.store.clear_link_features(&f.station, 98);
    let stored = c.store.grants(10).await.unwrap().len();

    // A target station with the screenshot features only is refused for a
    // recording grant, and accepted once it has the recording feature.
    let mut target = target_features();
    target.extend(
        [
            feature::DESKTOP_CAPTURE,
            feature::ARTIFACTS_DELETE,
            feature::CAPTURE_MCP,
        ]
        .map(str::to_owned),
    );
    let alpha = extra(&f.store, "alpha", target.clone()).await;
    let with_target = json!({"classes": ["capture"], "allow_recording": true,
        "stations": [alpha]});
    refused(
        &ask("n1", with_target.clone()).await,
        "target without capture_recording_mcp",
    );
    let (r, _) = tokio::join!(
        ask("n2", json!({"classes": ["capture"], "stations": [alpha]})),
        Fixture::answer(&mut station, accepted)
    );
    assert_eq!(r.status, 200, "{}", r.body);
    target.push(feature::CAPTURE_RECORDING_MCP.to_owned());
    let beta = extra(&f.store, "beta", target).await;
    let (r, _) = tokio::join!(
        ask(
            "n3",
            json!({"classes": ["capture"], "allow_recording": true, "stations": [beta]})
        ),
        Fixture::answer(&mut station, accepted)
    );
    assert_eq!(r.status, 200, "{}", r.body);
    no_request(&mut station, Duration::from_millis(200)).await;
    assert_eq!(c.store.grants(20).await.unwrap().len(), stored + 2);
}
