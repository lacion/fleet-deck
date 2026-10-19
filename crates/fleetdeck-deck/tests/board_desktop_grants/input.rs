//! R13 P7 task F on the deck, over the board API: the input opt-in of a
//! coordinator grant (the class `input`, `allow_input`, its scope and caps),
//! the features of the caller and of the target stations, and the board view.
//! (The store rules are in `src/store/grants_desktop_tests.rs`.)

use super::*;

/// R13 P7 task F: the three input classes of an opted-in grant, and the keys
/// that never go on to a station (the two caps).
const INPUT: [&str; 3] = ["desktop", "capture", "input"];
const INPUT_CAP_KEYS: [&str; 2] = ["input_cap", "input_character_cap"];

#[tokio::test]
async fn the_input_opt_in_follows_the_rules_of_the_contract_and_refuses_before_anything_is_stored()
{
    let mut f = fixture().await;
    let mut station = f.connect().await;
    let c = board(Arc::clone(&f.ctx)).await;
    set_password(&c).await;
    let h = owner(&c).await;
    let token = elevate(&c, &h).await;
    let classes = "allow_input needs the desktop, capture and input classes";
    let cases = [
        (
            json!({"classes": ["desktop", "capture"], "allow_input": true}),
            classes,
        ),
        (
            json!({"classes": ["desktop", "input"], "allow_input": true}),
            classes,
        ),
        (
            json!({"classes": ["capture", "input"], "allow_input": true, "input_cap": 1}),
            classes,
        ),
        (
            json!({"classes": INPUT, "input_scope": "own_windows"}),
            "input_scope needs allow_input",
        ),
        (
            json!({"classes": INPUT, "input_cap": 1}),
            "input caps need allow_input",
        ),
        (
            json!({"classes": INPUT, "input_character_cap": 0}),
            "input caps need allow_input",
        ),
        (
            json!({"classes": INPUT, "allow_input": false, "input_cap": 0}),
            "input caps need allow_input",
        ),
        (
            json!({"classes": INPUT, "allow_input": true, "input_cap": 5001}),
            "input_cap is 0 to 5000",
        ),
        (
            json!({"classes": INPUT, "allow_input": true, "input_character_cap": 1_000_001}),
            "input_character_cap is 0 to 1000000",
        ),
    ];
    for (spec, text) in cases {
        for elevation in [None, Some(token.as_str())] {
            let r = post(&c, &h, START, start_body("i1", spec.clone(), elevation)).await;
            expect(&r, 400, "invalid_grant");
            assert!(r.body.to_string().contains(text), "{text}: {}", r.body);
        }
    }
    // Any scope but `own_windows` fails to decode: nothing is stored.
    for scope in ["any", "OwnWindows", ""] {
        let spec = json!({"classes": INPUT, "allow_input": true, "input_scope": scope});
        let r = post(&c, &h, START, start_body("i2", spec, Some(&token))).await;
        assert_eq!(r.status, 400, "{scope}: {}", r.body);
    }
    // A valid opt-in still needs the owner's elevation; the class alone too.
    for spec in [
        json!({"classes": INPUT, "allow_input": true}),
        json!({"classes": ["input"]}),
    ] {
        let r = post(&c, &h, START, start_body("i3", spec, None)).await;
        expect(&r, 400, "elevation_required");
    }
    no_request(&mut station, Duration::from_millis(200)).await;
    assert!(c.store.grants(10).await.unwrap().is_empty());
}

#[tokio::test]
async fn an_input_grant_gets_the_defaults_or_the_given_caps_and_the_station_never_sees_the_caps() {
    let mut f = fixture().await;
    let mut station = f.connect().await;
    let c = board(Arc::clone(&f.ctx)).await;
    set_password(&c).await;
    let h = owner(&c).await;
    let token = elevate(&c, &h).await;
    // (request, spec, expected caps): the opt-in alone takes 100 and 20,000;
    // given caps and zero are kept; one given cap leaves the other default.
    let asks = [
        ("v1", json!({"allow_input": true}), (100, 20_000)),
        (
            "v2",
            json!({"allow_input": true, "input_cap": 5000, "input_character_cap": 1_000_000}),
            (5000, 1_000_000),
        ),
        (
            "v3",
            json!({"allow_input": true, "input_scope": "own_windows", "input_cap": 0, "input_character_cap": 0}),
            (0, 0),
        ),
        (
            "v4",
            json!({"allow_input": true, "input_cap": 7}),
            (7, 20_000),
        ),
    ];
    for (id, mut spec, caps) in asks {
        spec["classes"] = json!(INPUT);
        let call = post(&c, &h, START, start_body(id, spec, Some(&token)));
        let (r, req) = tokio::join!(call, Fixture::answer(&mut station, accepted));
        assert_eq!(r.status, 200, "{}", r.body);
        // The caller link negotiated the feature: the opt-in and its fixed
        // scope go on; the two caps never do.
        let sent = req.params["command"]["spec"]["coordinator"].clone();
        assert_eq!(sent["allow_input"], true, "{sent}");
        assert_eq!(sent["input_scope"], "own_windows", "{sent}");
        assert_eq!(sent["classes"], json!(INPUT), "{sent}");
        for key in INPUT_CAP_KEYS {
            assert!(sent.get(key).is_none(), "{key}: {sent}");
        }
        let g = c.store.grants(10).await.unwrap().remove(0);
        assert!(g.allow_input && g.is_fleet_control());
        assert_eq!(
            g.input_scope,
            Some(fleetdeck_proto::coordinator_spec::InputScope::OwnWindows)
        );
        assert_eq!(
            (g.input_cap, g.input_character_cap, g.input_used),
            (caps.0, caps.1, 0)
        );
    }
    // A grant without the opt-in sends none of the four fields, and its view
    // has none of the six; the opted-in view shows the caps and the counters.
    let call = post(
        &c,
        &h,
        START,
        start_body("v5", json!({"classes": ["capture"]}), Some(&token)),
    );
    let (r, req) = tokio::join!(call, Fixture::answer(&mut station, accepted));
    assert_eq!(r.status, 200, "{}", r.body);
    let sent = req.params["command"]["spec"]["coordinator"].clone();
    for key in [
        "allow_input",
        "input_scope",
        "input_cap",
        "input_character_cap",
    ] {
        assert!(sent.get(key).is_none(), "{key}: {sent}");
    }
    let none = c.store.grants(10).await.unwrap().remove(0);
    let (all, bare) = (c.store.grants(10).await.unwrap(), none.grant_id);
    let list = get(&c, &h, "/api/coordinator/grants").await;
    let views = list.body.as_array().unwrap();
    assert_eq!(views.len(), 5);
    for view in views {
        let keys = [
            "allow_input",
            "input_scope",
            "input_cap",
            "input_used",
            "input_character_cap",
            "input_character_used",
        ];
        let shown = keys.iter().filter(|k| view.get(**k).is_some()).count();
        assert_eq!(
            shown,
            if view["grant_id"] == bare.as_str() {
                0
            } else {
                6
            },
            "{view}"
        );
    }
    let first = all.last().unwrap();
    let view = views
        .iter()
        .find(|v| v["grant_id"] == first.grant_id.as_str())
        .unwrap();
    assert_eq!(
        (
            &view["allow_input"],
            &view["input_scope"],
            &view["input_cap"],
            &view["input_character_cap"]
        ),
        (
            &json!(true),
            &json!("own_windows"),
            &json!(100),
            &json!(20_000)
        )
    );
    assert_eq!(
        (&view["input_used"], &view["input_character_used"]),
        (&json!(0), &json!(0))
    );
    // The input class alone is fleet control and has no allowance.
    let call = post(
        &c,
        &h,
        START,
        start_body("v6", json!({"classes": ["input"]}), Some(&token)),
    );
    let (r, req) = tokio::join!(call, Fixture::answer(&mut station, accepted));
    assert_eq!(r.status, 200, "{}", r.body);
    assert!(
        req.params["command"]["spec"]["coordinator"]
            .get("allow_input")
            .is_none()
    );
    let bare = c.store.grants(10).await.unwrap().remove(0);
    assert!(!bare.allow_input && bare.input_scope.is_none() && bare.input_cap == 0);
}

#[tokio::test]
async fn an_input_grant_needs_desktop_input_mcp_on_the_caller_and_on_every_other_station() {
    let mut f = fixture().await;
    let mut station = f.connect().await;
    let c = board(Arc::clone(&f.ctx)).await;
    set_password(&c).await;
    let h = owner(&c).await;
    let token = elevate(&c, &h).await;
    let ask = |id: &str, spec: Value| post(&c, &h, START, start_body(id, spec, Some(&token)));
    let input = json!({"classes": INPUT, "allow_input": true});
    // A second live link of the caller with the recording features only.
    let mut caller: Vec<String> = [
        feature::COORDINATOR,
        feature::COORDINATOR_V2,
        feature::DESKTOP_MCP,
        feature::CAPTURE_MCP,
    ]
    .map(str::to_owned)
    .to_vec();
    f.store.set_link_features(&f.station, 98, caller.clone());
    let refused = |r: &Http, what: &str| {
        expect(r, 409, "unsupported");
        assert!(
            r.body.to_string().contains(UNSUPPORTED),
            "{what}: {}",
            r.body
        );
    };
    refused(
        &ask("f1", input.clone()).await,
        "caller without desktop_input_mcp",
    );
    // A grant without the opt-in is not affected.
    let (r, _) = tokio::join!(
        ask("f2", json!({"classes": ["desktop", "capture"]})),
        Fixture::answer(&mut station, accepted)
    );
    assert_eq!(r.status, 200, "{}", r.body);
    caller.push(feature::DESKTOP_INPUT_MCP.to_owned());
    f.store.set_link_features(&f.station, 98, caller);
    let (r, _) = tokio::join!(
        ask("f3", input.clone()),
        Fixture::answer(&mut station, accepted)
    );
    assert_eq!(r.status, 200, "{}", r.body);
    f.store.clear_link_features(&f.station, 98);
    // A target with the capture features only is refused, then accepted with
    // the input feature too.
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
    let with = |st: &StationId| json!({"classes": INPUT, "allow_input": true, "stations": [st]});
    refused(
        &ask("g1", with(&alpha)).await,
        "target without desktop_input_mcp",
    );
    target.push(feature::DESKTOP_INPUT_MCP.to_owned());
    let beta = extra(&f.store, "beta", target).await;
    let (r, _) = tokio::join!(
        ask("g2", with(&beta)),
        Fixture::answer(&mut station, accepted)
    );
    assert_eq!(r.status, 200, "{}", r.body);
    no_request(&mut station, Duration::from_millis(200)).await;
    assert_eq!(c.store.grants(10).await.unwrap().len(), 3);
}

// ----------------------------------------------------------------- R13 P7b

/// The base features of a caller link that can start an input grant.
fn input_caller() -> Vec<String> {
    [
        feature::COORDINATOR,
        feature::COORDINATOR_V2,
        feature::DESKTOP_MCP,
        feature::CAPTURE_MCP,
        feature::DESKTOP_INPUT_MCP,
    ]
    .map(str::to_owned)
    .to_vec()
}

#[tokio::test]
async fn the_click_opt_in_needs_the_input_opt_in_and_refuses_before_anything_is_stored() {
    let mut f = fixture().await;
    let mut station = f.connect().await;
    let c = board(Arc::clone(&f.ctx)).await;
    set_password(&c).await;
    let h = owner(&c).await;
    let token = elevate(&c, &h).await;
    let classes = "allow_input needs the desktop, capture and input classes";
    let cases = [
        (
            json!({"classes": INPUT, "allow_click": true}),
            "allow_click needs allow_input",
        ),
        (
            json!({"classes": INPUT, "allow_click": true, "allow_input": false}),
            "allow_click needs allow_input",
        ),
        (
            json!({"classes": INPUT, "allow_click": true, "input_scope": "any_window"}),
            "input_scope needs allow_input",
        ),
        (
            json!({"classes": ["desktop", "capture"], "allow_click": true, "allow_input": true}),
            classes,
        ),
        (
            json!({"classes": INPUT, "input_scope": "any_window"}),
            "input_scope needs allow_input",
        ),
    ];
    for (spec, text) in cases {
        for elevation in [None, Some(token.as_str())] {
            let r = post(&c, &h, START, start_body("k1", spec.clone(), elevation)).await;
            expect(&r, 400, "invalid_grant");
            assert!(r.body.to_string().contains(text), "{text}: {}", r.body);
        }
    }
    // A click flag that is not a bool, and an unknown scope, fail to decode.
    for spec in [
        json!({"classes": INPUT, "allow_input": true, "allow_click": "yes"}),
        json!({"classes": INPUT, "allow_input": true, "input_scope": "ANY_WINDOW"}),
        json!({"classes": INPUT, "allow_input": true, "input_scope": "any_windows"}),
    ] {
        let r = post(&c, &h, START, start_body("k2", spec, Some(&token))).await;
        assert_eq!(r.status, 400, "{}", r.body);
    }
    // A valid click opt-in still needs the owner's elevation.
    let spec = json!({"classes": INPUT, "allow_input": true, "allow_click": true});
    let r = post(&c, &h, START, start_body("k3", spec, None)).await;
    expect(&r, 400, "elevation_required");
    no_request(&mut station, Duration::from_millis(200)).await;
    assert!(c.store.grants(10).await.unwrap().is_empty());
}

#[tokio::test]
async fn a_click_or_any_window_grant_is_stored_forwarded_and_shown() {
    let mut f = fixture().await;
    let mut station = f.connect().await;
    let c = board(Arc::clone(&f.ctx)).await;
    set_password(&c).await;
    let h = owner(&c).await;
    let token = elevate(&c, &h).await;
    // (request, spec, click, scope, the scope that the station is sent)
    let asks = [
        ("a1", json!({"allow_input": true}), false, "own_windows"),
        (
            "a2",
            json!({"allow_input": true, "allow_click": true}),
            true,
            "own_windows",
        ),
        (
            "a3",
            json!({"allow_input": true, "input_scope": "any_window"}),
            false,
            "any_window",
        ),
        (
            "a4",
            json!({"allow_input": true, "allow_click": true, "input_scope": "any_window"}),
            true,
            "any_window",
        ),
    ];
    for (id, mut spec, click, scope) in asks {
        spec["classes"] = json!(INPUT);
        let call = post(&c, &h, START, start_body(id, spec, Some(&token)));
        let (r, req) = tokio::join!(call, Fixture::answer(&mut station, accepted));
        assert_eq!(r.status, 200, "{id}: {}", r.body);
        let sent = req.params["command"]["spec"]["coordinator"].clone();
        assert_eq!(sent["allow_input"], true, "{sent}");
        assert_eq!(sent["input_scope"], scope, "{sent}");
        // The click flag is written only when set, and the caps never.
        assert_eq!(
            sent.get("allow_click"),
            click.then_some(&json!(true)),
            "{sent}"
        );
        for key in INPUT_CAP_KEYS {
            assert!(sent.get(key).is_none(), "{key}: {sent}");
        }
        let g = c.store.grants(10).await.unwrap().remove(0);
        assert!(g.allow_input && g.is_fleet_control());
        assert_eq!(g.allow_click, click, "{id}");
        assert_eq!(
            g.input_scope,
            Some(if scope == "any_window" {
                fleetdeck_proto::coordinator_spec::InputScope::AnyWindow
            } else {
                fleetdeck_proto::coordinator_spec::InputScope::OwnWindows
            })
        );
        // The row reads back as stored.
        assert_eq!(c.store.grant(g.grant_id.clone()).await.unwrap().unwrap(), g);
    }
    // A grant without input sends and shows no click field.
    let call = post(
        &c,
        &h,
        START,
        start_body("a5", json!({"classes": ["capture"]}), Some(&token)),
    );
    let (r, req) = tokio::join!(call, Fixture::answer(&mut station, accepted));
    assert_eq!(r.status, 200, "{}", r.body);
    assert!(
        req.params["command"]["spec"]["coordinator"]
            .get("allow_click")
            .is_none()
    );
    let plain = c.store.grants(10).await.unwrap().remove(0);
    assert!(!plain.allow_click);
    // The board view: `allow_click` is present for a grant with input only.
    let list = get(&c, &h, "/api/coordinator/grants").await;
    let views = list.body.as_array().unwrap();
    assert_eq!(views.len(), 5);
    for view in views {
        let grant = c
            .store
            .grant(view["grant_id"].as_str().unwrap().to_owned())
            .await
            .unwrap()
            .unwrap();
        if grant.allow_input {
            assert_eq!(view["allow_click"], json!(grant.allow_click), "{view}");
            let scope = if grant.input_scope
                == Some(fleetdeck_proto::coordinator_spec::InputScope::AnyWindow)
            {
                "any_window"
            } else {
                "own_windows"
            };
            assert_eq!(view["input_scope"], scope, "{view}");
        } else {
            assert!(view.get("allow_click").is_none(), "{view}");
        }
    }
    // "Change limits" sends caps only: it never changes the click flag or the
    // scope.
    let clicking = c.store.grants(10).await.unwrap();
    let clicking = clicking
        .iter()
        .find(|g| g.allow_click && g.input_scope.is_some())
        .unwrap();
    let url = format!("/api/coordinator/grants/{}/caps", clicking.grant_id);
    for body in [
        json!({"allow_click": false}),
        json!({"input_scope": "own_windows"}),
        json!({"input_cap": 3, "allow_click": false}),
    ] {
        let r = post(&c, &h, &url, body).await;
        assert_eq!(r.status, 400, "{}", r.body);
    }
    let r = post(&c, &h, &url, json!({"input_cap": 3})).await;
    assert_eq!(r.status, 200, "{}", r.body);
    let after = c
        .store
        .grant(clicking.grant_id.clone())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (after.allow_click, after.input_scope),
        (clicking.allow_click, clicking.input_scope)
    );
    assert_eq!(after.input_cap, 3);
}

#[tokio::test]
async fn a_click_needs_desktop_click_mcp_and_any_window_needs_desktop_input_any() {
    let mut f = fixture().await;
    let mut station = f.connect().await;
    let c = board(Arc::clone(&f.ctx)).await;
    set_password(&c).await;
    let h = owner(&c).await;
    let token = elevate(&c, &h).await;
    let ask = |id: &str, spec: Value| post(&c, &h, START, start_body(id, spec, Some(&token)));
    let plain = json!({"classes": INPUT, "allow_input": true});
    let click = json!({"classes": INPUT, "allow_input": true, "allow_click": true});
    let any = json!({"classes": INPUT, "allow_input": true, "input_scope": "any_window"});
    let both = json!({"classes": INPUT, "allow_input": true, "allow_click": true,
        "input_scope": "any_window"});
    let refused = |r: &Http, what: &str| {
        expect(r, 409, "unsupported");
        assert!(
            r.body.to_string().contains(UNSUPPORTED),
            "{what}: {}",
            r.body
        );
    };
    // A second live link of the caller with the P7 features only.
    let mut caller = input_caller();
    f.store.set_link_features(&f.station, 98, caller.clone());
    refused(
        &ask("c1", click.clone()).await,
        "click without click feature",
    );
    refused(&ask("c2", any.clone()).await, "any without any feature");
    refused(&ask("c3", both.clone()).await, "both without either");
    // Plain P7 input is not affected.
    let (r, _) = tokio::join!(
        ask("c4", plain.clone()),
        Fixture::answer(&mut station, accepted)
    );
    assert_eq!(r.status, 200, "{}", r.body);
    // The click feature alone gives clicks but not the scope.
    caller.push(feature::DESKTOP_CLICK_MCP.to_owned());
    f.store.set_link_features(&f.station, 98, caller.clone());
    let (r, req) = tokio::join!(
        ask("c5", click.clone()),
        Fixture::answer(&mut station, accepted)
    );
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(
        req.params["command"]["spec"]["coordinator"]["allow_click"],
        true
    );
    refused(
        &ask("c6", any.clone()).await,
        "any with the click feature only",
    );
    refused(
        &ask("c7", both.clone()).await,
        "both with the click feature only",
    );
    // The scope feature alone gives the scope but not clicks.
    caller.pop();
    caller.push(feature::DESKTOP_INPUT_ANY.to_owned());
    f.store.set_link_features(&f.station, 98, caller.clone());
    refused(
        &ask("c8", click.clone()).await,
        "click with the any feature only",
    );
    let (r, req) = tokio::join!(
        ask("c9", any.clone()),
        Fixture::answer(&mut station, accepted)
    );
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(
        req.params["command"]["spec"]["coordinator"]["input_scope"],
        "any_window"
    );
    // Both features give both.
    caller.push(feature::DESKTOP_CLICK_MCP.to_owned());
    f.store.set_link_features(&f.station, 98, caller);
    let (r, req) = tokio::join!(
        ask("c10", both.clone()),
        Fixture::answer(&mut station, accepted)
    );
    assert_eq!(r.status, 200, "{}", r.body);
    let sent = &req.params["command"]["spec"]["coordinator"];
    assert_eq!(
        (&sent["allow_click"], &sent["input_scope"]),
        (&json!(true), &json!("any_window"))
    );
    f.store.clear_link_features(&f.station, 98);
    // The same rules for another station of the grant.
    let mut target = target_features();
    target.extend(
        [
            feature::DESKTOP_CAPTURE,
            feature::ARTIFACTS_DELETE,
            feature::CAPTURE_MCP,
            feature::DESKTOP_INPUT_MCP,
        ]
        .map(str::to_owned),
    );
    let alpha = extra(&f.store, "alpha", target.clone()).await;
    let with = |st: &StationId, spec: &Value| {
        let mut spec = spec.clone();
        spec["stations"] = json!([st]);
        spec
    };
    refused(
        &ask("t1", with(&alpha, &click)).await,
        "target without click",
    );
    refused(&ask("t2", with(&alpha, &any)).await, "target without any");
    target.push(feature::DESKTOP_CLICK_MCP.to_owned());
    target.push(feature::DESKTOP_INPUT_ANY.to_owned());
    let beta = extra(&f.store, "beta", target).await;
    let (r, _) = tokio::join!(
        ask("t3", with(&beta, &both)),
        Fixture::answer(&mut station, accepted)
    );
    assert_eq!(r.status, 200, "{}", r.body);
    no_request(&mut station, Duration::from_millis(200)).await;
}
