//! The wall actions of the desktop relay and the wake actor (R13 P3b relay,
//! §6D items 1, 2, 4 and 10): the launch URL, the binding steps, the display
//! lock and the limits of the wake. The generic relay is in
//! `board_desktop.rs`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
mod desktop_support;
mod share_support;

use std::collections::BTreeSet;

use common::board::BoardClient;
use common::*;
use desktop_support::*;
use fleetdeck_core::identity::Identity;
use fleetdeck_deck::desktop_relay::{Actor, RelayError, act};
use fleetdeck_deck::store::AuditResult;
use fleetdeck_deck::store::displays::DisplayChange;
use fleetdeck_deck::store::stations::NewStation;
use fleetdeck_link::control::InboundRequest;
use fleetdeck_link::desktop_authority::action_hash;
use fleetdeck_proto::desktop::DesktopAction;
use fleetdeck_proto::desktop_authority::split_params;
use fleetdeck_proto::display::DisplayBinding;
use fleetdeck_proto::link::Scope;
use serde_json::json;
use share_support::*;

const TIMEOUT_TEXT: &str = "The desktop action did not finish in time.";

async fn bind(f: &Fixture, c: &BoardClient, id: &str, monitor: &str) {
    c.store
        .display_set_binding(
            id.to_owned(),
            Some(DisplayBinding {
                station_id: f.station.clone(),
                monitor: monitor.to_owned(),
            }),
        )
        .await
        .unwrap();
}

async fn record(c: &BoardClient, id: &str) -> fleetdeck_deck::store::displays::DisplayRecord {
    c.store.display_get(id.to_owned()).await.unwrap().unwrap()
}

fn url_of(req: &InboundRequest) -> String {
    req.params["url"].as_str().unwrap().to_owned()
}

fn open_body(id: &str) -> serde_json::Value {
    json!({ "action": "wall_open", "display_id": id, "monitor": "DP-1", "workspace": null })
}

#[tokio::test]
async fn a_wall_open_sends_the_pair_page_and_commits_the_binding_after_the_answer() {
    let (f, mut station, c, h) = setup().await;
    enable(&c, true);
    let st = f.station.0.clone();
    let id = display(&f, &c, "Wall").await;
    let (r, req) = exchange(&c, &h, &mut station, &st, open_body(&id), ok(Some("0x1"))).await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body["address"], "0x1");
    let d = record(&c, &id).await;
    // The pair page is on the board address of the request.
    let url = url_of(&req);
    let prefix = "https://deck.tail1234.ts.net:";
    assert!(url.starts_with(prefix), "{url}");
    let tail = format!(
        "/display#/display-pair?expect={id}&op={}",
        d.binding_revision
    );
    assert!(url.ends_with(&tail), "{url}");
    assert_eq!(
        d.binding,
        Some(DisplayBinding {
            station_id: f.station.clone(),
            monitor: "DP-1".to_owned()
        })
    );
    // The authority covers the whole action, the launch URL included.
    let (flat, _) = split_params(req.params.clone()).unwrap();
    let action: DesktopAction = serde_json::from_value(flat).unwrap();
    let node = f.station_node.identity.fingerprint().to_string();
    assert_eq!(authority(&req).action_hash, action_hash(&action, &node));
    assert_eq!(
        c.audited("desktop.wall_open", AuditResult::Ok).await,
        vec![None]
    );
}

#[tokio::test]
async fn a_wall_needs_a_board_address_that_is_not_loopback() {
    let (f, mut station, c, h) = setup().await;
    enable(&c, true);
    let st = f.station.0.clone();
    let id = display(&f, &c, "Wall").await;
    // This test deck listens on loopback only, so a loopback origin has no
    // other address to fall back to.
    let loopback = c.origin();
    for body in [
        open_body(&id),
        json!({ "action": "wall_reload", "display_id": id }),
    ] {
        let r = call_from(&c, &h, &st, body, &loopback).await;
        expect(&r, 409, "desktop_no_endpoint");
    }
    quiet(&mut station).await;
    assert_eq!(record(&c, &id).await.binding_revision, 0);
    // A close needs no address.
    let (r, _) = exchange(
        &c,
        &h,
        &mut station,
        &st,
        json!({ "action": "wall_close", "display_id": id }),
        ok(None),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
}

#[tokio::test]
async fn an_open_that_fails_or_times_out_leaves_the_binding_and_is_never_retried() {
    let (f, mut station, c, h) = setup().await;
    enable(&c, true);
    let st = f.station.0.clone();
    let id = display(&f, &c, "Wall").await;
    for (reply, status, code) in [
        (err("failed", "boom"), 502, "desktop_failed"),
        (err("failed", TIMEOUT_TEXT), 504, "desktop_timeout"),
        (err("forbidden", "no"), 403, "desktop_forbidden"),
    ] {
        let (r, _) = exchange(&c, &h, &mut station, &st, open_body(&id), reply).await;
        expect(&r, status, code);
        assert_eq!(record(&c, &id).await.binding, None);
        quiet(&mut station).await;
    }
    // A missing display is a 404 and the station sees nothing.
    let r = call(&c, &h, &st, open_body("no-such-display")).await;
    expect(&r, 404, "desktop_not_found");
    quiet(&mut station).await;
}

#[tokio::test]
async fn an_open_for_a_display_bound_to_another_station_is_a_conflict_without_a_send() {
    let (f, mut station, c, h) = setup().await;
    enable(&c, true);
    let st = f.station.0.clone();
    let other = f
        .store
        .pin_station(NewStation {
            key: Identity::generate().unwrap().public_key(),
            name: "other".to_owned(),
            scopes: BTreeSet::from([Scope::Read]),
            dial_url: None,
        })
        .await
        .unwrap();
    let id = display(&f, &c, "Wall").await;
    c.store
        .display_set_binding(
            id.clone(),
            Some(DisplayBinding {
                station_id: other.station_id.clone(),
                monitor: "DP-2".to_owned(),
            }),
        )
        .await
        .unwrap();
    let r = call(&c, &h, &st, open_body(&id)).await;
    expect(&r, 409, "desktop_conflict");
    quiet(&mut station).await;
    let d = record(&c, &id).await;
    assert_eq!(d.binding.unwrap().station_id, other.station_id);
    // A reload on this station of a display that is bound elsewhere too.
    let r = call(
        &c,
        &h,
        &st,
        json!({ "action": "wall_reload", "display_id": id }),
    )
    .await;
    expect(&r, 409, "desktop_conflict");
    quiet(&mut station).await;
}

#[tokio::test]
async fn a_second_wall_action_of_one_display_conflicts_while_the_first_runs() {
    let (f, mut station, c, h) = setup().await;
    enable(&c, true);
    let st = f.station.0.clone();
    let id = display(&f, &c, "Wall").await;
    let (seen_tx, seen_rx) = tokio::sync::oneshot::channel::<()>();
    let (done_tx, done_rx) = tokio::sync::oneshot::channel::<()>();
    let first = call(&c, &h, &st, open_body(&id));
    let second = async {
        seen_rx.await.unwrap();
        let r = call(
            &c,
            &h,
            &st,
            json!({ "action": "wall_close", "display_id": id }),
        )
        .await;
        done_tx.send(()).unwrap();
        r
    };
    let station_side = async {
        let req = next_request(&mut station).await;
        seen_tx.send(()).unwrap();
        done_rx.await.unwrap();
        station
            .control
            .respond(req.id, ok(Some("0x1")))
            .await
            .unwrap();
    };
    let (first, second, ()) = tokio::join!(first, second, station_side);
    assert_eq!(first.status, 200, "{}", first.body);
    expect(&second, 409, "desktop_conflict");
    // The lock is free again.
    quiet(&mut station).await;
    let (r, req) = exchange(
        &c,
        &h,
        &mut station,
        &st,
        json!({ "action": "wall_close", "display_id": id }),
        ok(None),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(req.params["action"], "wall_close");
}

#[tokio::test]
async fn a_reload_is_a_new_launch_on_the_bound_monitor_and_keeps_the_binding() {
    let (f, mut station, c, h) = setup().await;
    enable(&c, true);
    let st = f.station.0.clone();
    let id = display(&f, &c, "Wall").await;
    // No binding: a conflict, no send.
    let reload = json!({ "action": "wall_reload", "display_id": id });
    expect(
        &call(&c, &h, &st, reload.clone()).await,
        409,
        "desktop_conflict",
    );
    quiet(&mut station).await;
    expect(
        &call(
            &c,
            &h,
            &st,
            json!({ "action": "wall_reload", "display_id": "none" }),
        )
        .await,
        404,
        "desktop_not_found",
    );
    bind(&f, &c, &id, "DP-1").await;
    let before = record(&c, &id).await;
    let (r, req) = exchange(&c, &h, &mut station, &st, reload, ok(None)).await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(req.params["action"], "wall_reload");
    let after = record(&c, &id).await;
    assert!(after.binding_revision > before.binding_revision);
    assert!(url_of(&req).ends_with(&format!(
        "/display#/display-pair?expect={id}&op={}",
        after.binding_revision
    )));
    assert_eq!(after.binding, before.binding);
}

#[tokio::test]
async fn a_close_clears_the_binding_only_after_a_successful_close() {
    let (f, mut station, c, h) = setup().await;
    enable(&c, true);
    let st = f.station.0.clone();
    let id = display(&f, &c, "Wall").await;
    bind(&f, &c, &id, "DP-1").await;
    let close = json!({ "action": "wall_close", "display_id": id });
    let (r, _) = exchange(&c, &h, &mut station, &st, close.clone(), err("failed", "x")).await;
    expect(&r, 502, "desktop_failed");
    assert!(record(&c, &id).await.binding.is_some());
    let (r, _) = exchange(&c, &h, &mut station, &st, close.clone(), ok(None)).await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(record(&c, &id).await.binding, None);
    // A revoked display can still be closed on the station.
    let gone = display(&f, &c, "Gone").await;
    c.store.display_revoke(gone.clone()).await.unwrap();
    let (r, req) = exchange(
        &c,
        &h,
        &mut station,
        &st,
        json!({ "action": "wall_close", "display_id": gone }),
        ok(None),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(req.params["display_id"], gone.as_str());
    // A close on this station keeps a binding of another station.
    let other = f
        .store
        .pin_station(NewStation {
            key: Identity::generate().unwrap().public_key(),
            name: "other".to_owned(),
            scopes: BTreeSet::from([Scope::Read]),
            dial_url: None,
        })
        .await
        .unwrap();
    let elsewhere = DisplayBinding {
        station_id: other.station_id.clone(),
        monitor: "DP-2".to_owned(),
    };
    c.store
        .display_set_binding(id.clone(), Some(elsewhere.clone()))
        .await
        .unwrap();
    let (r, _) = exchange(&c, &h, &mut station, &st, close, ok(None)).await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(record(&c, &id).await.binding, Some(elsewhere));
}

async fn wake(
    f: &Fixture,
    c: &BoardClient,
    action: DesktopAction,
    display_id: &str,
) -> Result<(), RelayError> {
    let ctx = relay_ctx(f, c);
    act(
        &ctx,
        f.station.clone(),
        action,
        Actor::Wake {
            display_id: display_id.to_owned(),
        },
    )
    .await
    .map(|_| ())
}

fn power(monitor: &str, on: bool) -> DesktopAction {
    DesktopAction::ScreenPower {
        monitor: Some(monitor.to_owned()),
        on,
    }
}

#[tokio::test]
async fn the_wake_sends_screen_power_on_for_the_bound_monitor_with_the_quiet_hours() {
    let (f, mut station, c, _h) = setup().await;
    enable(&c, true);
    let id = display(&f, &c, "Wall").await;
    bind(&f, &c, &id, "DP-1").await;
    c.store
        .display_update(
            id.clone(),
            DisplayChange {
                quiet_hours: Some(Some("22:00-07:00".to_owned())),
                ..DisplayChange::default()
            },
        )
        .await
        .unwrap();
    let answer = async {
        let req = next_request(&mut station).await;
        station.control.respond(req.id, ok(None)).await.unwrap();
        req
    };
    let (result, req) = tokio::join!(wake(&f, &c, power("DP-1", true), &id), answer);
    result.unwrap();
    assert_eq!(req.params["action"], "screen_power");
    assert_eq!(req.params["monitor"], "DP-1");
    assert_eq!(authority(&req).quiet_hours.as_deref(), Some("22:00-07:00"));
    let rows = c.store.audit_log(100).await.unwrap();
    let row = rows.iter().find(|e| e.action == "desktop.wake").unwrap();
    assert_eq!(row.actor, "wake");
    assert_eq!(row.result, AuditResult::Ok);
    assert_eq!(row.target.as_deref(), Some(f.station.as_str()));
    assert_eq!(
        row.detail.as_deref(),
        Some(format!("display={id} monitor=DP-1 code=ok").as_str())
    );
}

#[tokio::test]
async fn the_wake_is_refused_without_a_send_for_anything_but_the_bound_monitor_on() {
    let (f, mut station, c, _h) = setup().await;
    enable(&c, true);
    let id = display(&f, &c, "Wall").await;
    let unbound = display(&f, &c, "Free").await;
    bind(&f, &c, &id, "DP-1").await;
    let cases = [
        (power("DP-2", true), id.as_str(), RelayError::Conflict),
        (power("DP-1", false), id.as_str(), RelayError::Invalid),
        (
            DesktopAction::ScreenPower {
                monitor: None,
                on: true,
            },
            id.as_str(),
            RelayError::Invalid,
        ),
        (DesktopAction::ScreenLock, id.as_str(), RelayError::Invalid),
        (power("DP-1", true), unbound.as_str(), RelayError::Conflict),
        (power("DP-1", true), "no-such-display", RelayError::NotFound),
    ];
    for (action, display_id, want) in cases {
        assert_eq!(
            wake(&f, &c, action.clone(), display_id).await,
            Err(want),
            "{action:?}"
        );
    }
    quiet(&mut station).await;
    // A refusal is an audit row with the code.
    let rows = c.audited("desktop.wake", AuditResult::Refused).await;
    assert_eq!(rows.len(), 6);
    assert!(rows.contains(&Some(format!(
        "display={id} monitor=DP-2 code=desktop_conflict"
    ))));
    // The switch off stops the wake too.
    enable(&c, false);
    assert_eq!(
        wake(&f, &c, power("DP-1", true), &id).await,
        Err(RelayError::ControlOff)
    );
    quiet(&mut station).await;
}

#[tokio::test]
async fn the_public_act_refuses_a_wall_action() {
    let (f, mut station, c, _h) = setup().await;
    enable(&c, true);
    let ctx = relay_ctx(&f, &c);
    for action in [
        DesktopAction::WallClose {
            display_id: "d".into(),
        },
        DesktopAction::WallReload {
            display_id: "d".into(),
            url: "https://deck.ts.net/display".into(),
        },
        DesktopAction::WallOpen {
            display_id: "d".into(),
            monitor: "DP-1".into(),
            workspace: None,
            url: "https://deck.ts.net/display".into(),
        },
    ] {
        let r = act(&ctx, f.station.clone(), action, Actor::Owner).await;
        assert_eq!(r.unwrap_err(), RelayError::Invalid);
    }
    quiet(&mut station).await;
}
