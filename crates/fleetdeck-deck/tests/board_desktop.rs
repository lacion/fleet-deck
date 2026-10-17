//! The owner's desktop relay over loopback HTTPS (R13 P3b relay, §6D items 6
//! to 10): the authority of an action, the switch, the features, the error
//! mapping, the input checks, the audit rows and the info route. The wall
//! actions and the wake actor are in `board_desktop_wall.rs`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
mod desktop_support;
mod share_support;

use std::collections::BTreeSet;

use common::next_request;
use desktop_support::*;
use fleetdeck_deck::store::AuditResult;
use fleetdeck_link::desktop_authority::action_hash;
use fleetdeck_proto::desktop::DesktopAction;
use fleetdeck_proto::desktop_authority::AUTHORITY_TTL_MS;
use fleetdeck_proto::link::{Outcome, Scope};
use fleetdeck_proto::version::feature;
use serde_json::json;
use share_support::*;

const SENTINEL: &str = "SENTINEL-TEXT-4711";

#[tokio::test]
async fn an_owner_action_reaches_the_station_with_a_bound_authority() {
    let (f, mut station, c, h) = setup().await;
    enable(&c, true);
    let st = f.station.0.clone();
    let (r, req) = exchange(
        &c,
        &h,
        &mut station,
        &st,
        json!({ "action": "screen_dnd", "on": true }),
        ok(None),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body["info"]["kind"], "omarchy");
    // The flat action and the authority key.
    assert_eq!(req.params["action"], "screen_dnd");
    assert_eq!(req.params["on"], true);
    let a = authority(&req);
    let node = f.station_node.identity.fingerprint().to_string();
    let action = DesktopAction::ScreenDnd { on: true };
    assert_eq!(a.action_hash, action_hash(&action, &node));
    assert_eq!(
        a.desktop_epoch,
        station
            .control
            .params()
            .local_desktop_epoch
            .clone()
            .unwrap()
    );
    assert_eq!(a.deadline_ms - a.deck_now_ms, AUTHORITY_TTL_MS);
    assert_eq!(a.deck_now_ms, f.store.now_ms());
    assert_eq!(a.quiet_hours, None);
    assert_eq!(a.action_id.len(), 26);
}

#[tokio::test]
async fn two_actions_have_two_action_ids() {
    let (f, mut station, c, h) = setup().await;
    enable(&c, true);
    let st = f.station.0.clone();
    let body = json!({ "action": "screen_lock" });
    let (_, one) = exchange(&c, &h, &mut station, &st, body.clone(), ok(None)).await;
    let (_, two) = exchange(&c, &h, &mut station, &st, body, ok(None)).await;
    assert_ne!(authority(&one).action_id, authority(&two).action_id);
}

#[tokio::test]
async fn a_success_updates_only_the_cached_desktop_data() {
    let (f, mut station, c, h) = setup().await;
    enable(&c, true);
    let st = f.station.0.clone();
    let cached: fleetdeck_proto::link::StationInfo = serde_json::from_value(json!({
        "name": "fake", "incarnation": "inc", "head": 7, "live_sessions": 2
    }))
    .unwrap();
    f.store.set_station_info(&f.station, cached.clone());
    let (r, _) = exchange(
        &c,
        &h,
        &mut station,
        &st,
        json!({ "action": "screen_lock" }),
        ok(None),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    let (now, _) = f.store.station_info(&f.station).unwrap();
    assert_eq!(now.desktop, Some(info()));
    assert_eq!(
        (now.name.as_str(), now.head, now.live_sessions),
        ("fake", 7, 2)
    );
    let mut same = cached;
    same.desktop = Some(info());
    assert_eq!(now, same);
}

#[tokio::test]
async fn the_switch_off_blocks_both_routes_and_the_station_sees_nothing() {
    let (f, mut station, c, h) = setup().await;
    let st = f.station.0.clone();
    let r = call(&c, &h, &st, json!({ "action": "screen_lock" })).await;
    expect(&r, 409, "desktop_control_off");
    expect(
        &get(&c, &h, &info_path(&st)).await,
        409,
        "desktop_control_off",
    );
    quiet(&mut station).await;
    // The refusal has its own audit row.
    let rows = c.audited("desktop.screen_lock", AuditResult::Refused).await;
    assert_eq!(rows, vec![Some("desktop_control_off".to_owned())]);
    // On: it runs. Off again: it stops at once.
    enable(&c, true);
    let (r, _) = exchange(
        &c,
        &h,
        &mut station,
        &st,
        json!({ "action": "screen_lock" }),
        ok(None),
    )
    .await;
    assert_eq!(r.status, 200);
    enable(&c, false);
    expect(
        &call(&c, &h, &st, json!({ "action": "screen_lock" })).await,
        409,
        "desktop_control_off",
    );
    quiet(&mut station).await;
}

#[tokio::test]
async fn a_station_without_a_feature_or_a_link_is_refused_before_a_send() {
    let (f, mut station, c, h) = setup().await;
    enable(&c, true);
    let st = f.station.0.clone();
    let body = json!({ "action": "screen_lock" });
    // Another link of the station without the authority feature.
    f.store
        .set_link_features(&f.station, 98, vec![feature::DESKTOP.to_owned()]);
    expect(
        &call(&c, &h, &st, body.clone()).await,
        409,
        "desktop_unsupported",
    );
    // Without desktop.v1 as well.
    f.store.set_link_features(&f.station, 98, vec![]);
    expect(
        &call(&c, &h, &st, body.clone()).await,
        409,
        "desktop_unsupported",
    );
    expect(
        &get(&c, &h, &info_path(&st)).await,
        409,
        "desktop_unsupported",
    );
    quiet(&mut station).await;
    f.store.clear_link_features(&f.station, 98);
    // No link at all.
    f.drop_link(station).await;
    expect(&call(&c, &h, &st, body).await, 409, "station_unreachable");
    expect(
        &get(&c, &h, &info_path(&st)).await,
        409,
        "station_unreachable",
    );
}

#[tokio::test]
async fn the_station_answers_map_to_fixed_codes_and_its_text_never_leaves() {
    let (f, mut station, c, h) = setup().await;
    enable(&c, true);
    let st = f.station.0.clone();
    let cases = [
        ("unsupported", "x", 409, "desktop_unsupported"),
        ("forbidden", "x", 403, "desktop_forbidden"),
        ("invalid_params", "x", 400, "invalid_desktop_action"),
        ("not_found", "x", 404, "desktop_not_found"),
        ("conflict", "x", 409, "desktop_conflict"),
        ("failed", "x", 502, "desktop_failed"),
        (
            "failed",
            "The authorization of this desktop action expired.",
            502,
            "desktop_failed",
        ),
        (
            "failed",
            "Quiet hours are active on this station.",
            502,
            "desktop_failed",
        ),
        (
            "failed",
            "The desktop action did not finish in time.",
            504,
            "desktop_timeout",
        ),
        ("too_many_requests", "x", 503, "desktop_overloaded"),
        ("timeout", "x", 504, "desktop_timeout"),
        ("something_new", "x", 502, "desktop_failed"),
    ];
    for (code, text, status, want) in cases {
        let message = format!("{text} {SENTINEL} /home/owner/secret");
        let message = if code == "failed" && text.starts_with("The ") {
            text.to_owned()
        } else {
            message
        };
        let (r, _) = exchange(
            &c,
            &h,
            &mut station,
            &st,
            json!({ "action": "screen_lock" }),
            err(code, &message),
        )
        .await;
        expect(&r, status, want);
        assert!(!r.text.contains(SENTINEL), "{}", r.text);
        assert!(!r.text.contains("/home/owner"), "{}", r.text);
        // The deck never retries by itself.
        quiet(&mut station).await;
    }
    // An answer that does not parse is a failure.
    let (r, _) = exchange(
        &c,
        &h,
        &mut station,
        &st,
        json!({ "action": "screen_lock" }),
        Outcome::Ok(json!({ "nonsense": true })),
    )
    .await;
    expect(&r, 502, "desktop_failed");
}

#[tokio::test]
async fn a_token_and_a_member_get_owner_only_with_an_audit_row() {
    let (f, mut station, c, h) = setup().await;
    enable(&c, true);
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
    let r = c
        .post(
            &act_path(&st),
            &[("Authorization", auth.as_str())],
            json!({ "action": "screen_lock" }),
        )
        .await;
    expect(&r, 403, "owner_only");
    let r = c
        .get(&info_path(&st), &[("Authorization", auth.as_str())])
        .await;
    expect(&r, 403, "owner_only");
    let share = json!({ "kind": "session", "target": format!("{st}/s1"), "role": "viewer" });
    let (member, _, _) = join(&c, &h, share, "Alice").await;
    let r = post(
        &c,
        &member,
        &act_path(&st),
        json!({ "action": "screen_lock" }),
    )
    .await;
    expect(&r, 403, "owner_only");
    quiet(&mut station).await;
    // The token reached the handler; the guard may refuse the member first.
    assert!(
        !c.audited("desktop.act", AuditResult::Refused)
            .await
            .is_empty()
    );
}

#[tokio::test]
async fn an_unknown_station_and_a_bad_body_stop_before_the_relay() {
    let (f, mut station, c, h) = setup().await;
    enable(&c, true);
    let st = f.station.0.clone();
    expect(
        &call(
            &c,
            &h,
            "no-such-station",
            json!({ "action": "screen_lock" }),
        )
        .await,
        404,
        "unknown_station",
    );
    let bad = [
        json!({ "action": "screen_dnd", "on": true, "x": 1 }),
        json!({ "action": "exec", "cmd": "id" }),
        json!({}),
        json!({ "action": "screen_dnd", "on": "yes" }),
        // The body of the station-side `wall_open` has no URL.
        json!({ "action": "wall_open", "display_id": "d", "monitor": "DP-1", "url": "https://x" }),
    ];
    for body in bad {
        let r = call(&c, &h, &st, body).await;
        expect(&r, 400, "invalid_body");
    }
    // More than 16 KiB.
    let big =
        json!({ "action": "notify", "title": "t", "body": "b".repeat(20_000), "urgency": "low" });
    expect(&call(&c, &h, &st, big).await, 400, "invalid_body");
    quiet(&mut station).await;
    // The unknown station, the five bad bodies and the big one.
    assert_eq!(
        c.audited("desktop.act", AuditResult::Refused).await.len(),
        7
    );
}

#[tokio::test]
async fn the_deck_checks_monitor_names_display_ids_and_the_session_station() {
    let (f, mut station, c, h) = setup().await;
    enable(&c, true);
    let st = f.station.0.clone();
    for body in [
        json!({ "action": "screen_power", "monitor": "DP 1;rm", "on": true }),
        json!({ "action": "workspace_show", "monitor": "", "workspace": 2 }),
        json!({ "action": "wall_open", "display_id": "a b", "monitor": "DP-1", "workspace": null }),
        json!({ "action": "wall_open", "display_id": "d", "monitor": "x/y", "workspace": null }),
        json!({ "action": "wall_close", "display_id": "../x" }),
        json!({ "action": "browser_open", "url": "https://example.org", "monitor": "bad name", "session": null }),
        json!({ "action": "browser_open", "url": "https://example.org", "monitor": null, "session": "no-such-session" }),
    ] {
        let r = call(&c, &h, &st, body.clone()).await;
        assert_eq!(
            (r.status, r.code()),
            (400, "invalid_desktop_action"),
            "{body}"
        );
    }
    quiet(&mut station).await;
    // A session of this station passes; the station checks the URL.
    let (r, req) = exchange(
        &c,
        &h,
        &mut station,
        &st,
        json!({ "action": "browser_open", "url": "https://example.org", "monitor": "DP-1", "session": "s1" }),
        ok(Some("0xabc")),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body["address"], "0xabc");
    assert_eq!(req.params["session"], "s1");
}

#[tokio::test]
async fn the_audit_rows_name_the_action_and_the_station_and_never_a_text() {
    let (f, mut station, c, h) = setup().await;
    enable(&c, true);
    let st = f.station.0.clone();
    let (r, _) = exchange(
        &c,
        &h,
        &mut station,
        &st,
        json!({ "action": "notify", "title": SENTINEL, "body": SENTINEL, "urgency": "low" }),
        ok(None),
    )
    .await;
    assert_eq!(r.status, 200);
    let (r, _) = exchange(
        &c,
        &h,
        &mut station,
        &st,
        json!({ "action": "browser_open", "url": format!("https://{SENTINEL}.example"), "monitor": null, "session": "s1" }),
        err("forbidden", SENTINEL),
    )
    .await;
    expect(&r, 403, "desktop_forbidden");
    assert_eq!(
        c.audited("desktop.notify", AuditResult::Ok).await,
        vec![None]
    );
    assert_eq!(
        c.audited("desktop.browser_open", AuditResult::Refused)
            .await,
        vec![Some("desktop_forbidden".to_owned())]
    );
    let rows = c.store.audit_log(100).await.unwrap();
    let row = rows.iter().find(|e| e.action == "desktop.notify").unwrap();
    assert_eq!(
        (row.actor.as_str(), row.target.as_deref()),
        ("owner", Some(st.as_str()))
    );
    assert!(!audit_text(&c).await.contains(SENTINEL));
}

#[tokio::test]
async fn the_info_route_asks_for_a_cached_read_and_the_relay_can_ask_for_a_fresh_one() {
    let (f, mut station, c, h) = setup().await;
    enable(&c, true);
    let st = f.station.0.clone();
    let path = info_path(&st);
    let call = get(&c, &h, &path);
    let answer = async {
        let req = next_request(&mut station).await;
        station
            .control
            .respond(req.id, Outcome::Ok(serde_json::to_value(info()).unwrap()))
            .await
            .unwrap();
        req
    };
    let (r, req) = tokio::join!(call, answer);
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body["monitors"][0]["name"], "DP-1");
    assert_eq!(req.method, "desktop.info");
    assert_eq!(req.params, json!({ "fresh": false }));
    // The relay asks for a fresh read (the wake does).
    let ctx = relay_ctx(&f, &c);
    let ask = fleetdeck_deck::desktop_relay::info(&ctx, f.station.clone(), true);
    let answer = async {
        let req = next_request(&mut station).await;
        station
            .control
            .respond(req.id, Outcome::Ok(serde_json::to_value(info()).unwrap()))
            .await
            .unwrap();
        req
    };
    let (got, req) = tokio::join!(ask, answer);
    assert_eq!(got.unwrap(), info());
    assert_eq!(req.params, json!({ "fresh": true }));
    // The station timeout text is a timeout; a read writes no audit row.
    let ask = fleetdeck_deck::desktop_relay::info(&ctx, f.station.clone(), false);
    let answer = async {
        let req = next_request(&mut station).await;
        station
            .control
            .respond(
                req.id,
                err("failed", "The desktop info did not finish in time."),
            )
            .await
            .unwrap();
    };
    let (got, ()) = tokio::join!(ask, answer);
    assert_eq!(
        got.unwrap_err(),
        fleetdeck_deck::desktop_relay::RelayError::Timeout
    );
    assert!(!audit_text(&c).await.contains("desktop."));
}
